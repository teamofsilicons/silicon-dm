use super::{DaemonCommand, host, queue, store};
use crate::relay::{RelayAcknowledgement, RelayRequest};
use anyhow::{Context, Result, bail};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use fs2::FileExt;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore, watch},
    task::{JoinHandle, JoinSet},
};
use uuid::Uuid;

/// The one relay process of this operating-system user. Every DM home (each
/// Silicon's SILICON_HOME, or a Carbon's own) attaches to it; a request
/// authenticates with its home's bearer and only reaches that home's
/// profiles and queue.
struct Host {
    home: PathBuf,
    port: u16,
    attach_bearer: blake3::Hash,
    homes: Mutex<BTreeMap<PathBuf, Arc<Tenant>>>,
    attaching: tokio::sync::Mutex<()>,
    payload_budget: Arc<Semaphore>,
    workers: Arc<Semaphore>,
    shutdown: watch::Sender<bool>,
}
impl Host {
    fn homes(&self) -> MutexGuard<'_, BTreeMap<PathBuf, Arc<Tenant>>> {
        self.homes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
    fn accepting(&self, headers: &HeaderMap) -> bool {
        !*self.shutdown.borrow() && !headers.contains_key("origin")
    }
    fn tenant(&self, headers: &HeaderMap) -> Option<Arc<Tenant>> {
        if !self.accepting(headers) {
            return None;
        }
        let presented = blake3::hash(headers.get("authorization")?.as_bytes());
        self.homes()
            .values()
            .find(|tenant| tenant.bearer == presented)
            .cloned()
    }
    fn owner(&self, headers: &HeaderMap) -> bool {
        self.accepting(headers)
            && headers
                .get("authorization")
                .is_some_and(|value| blake3::hash(value.as_bytes()) == self.attach_bearer)
    }
}

struct Tenant {
    context: RuntimeContext,
    bearer: blake3::Hash,
    outbox: Mutex<Option<JoinHandle<()>>>,
    // Declared last so the home's lock is released only after its outbox stops.
    _lock: std::fs::File,
}
impl Drop for Tenant {
    fn drop(&mut self) {
        if let Ok(mut outbox) = self.outbox.lock()
            && let Some(task) = outbox.take()
        {
            task.abort();
        }
    }
}

#[derive(Clone)]
struct RuntimeContext {
    store: store::Store,
    queue: queue::Queue,
    payload_budget: Arc<Semaphore>,
    workers: Arc<Semaphore>,
}
// Count MiB of encoded queued work, not just tasks. Deserialization and HTTP
// serialization need additional memory. A larger single item takes the entire
// budget so it can progress, but cannot overlap another outgoing request.
// Both limits are shared by every attached home.
const PAYLOAD_BUDGET_MIB: u32 = 128;
const WORKERS: usize = 16;
fn reserve_payload(context: &RuntimeContext, bytes: u32) -> Option<OwnedSemaphorePermit> {
    let permits = bytes.div_ceil(1024 * 1024).clamp(1, PAYLOAD_BUDGET_MIB);
    context
        .payload_budget
        .clone()
        .try_acquire_many_owned(permits)
        .ok()
}
struct AbortOnDrop(JoinHandle<()>);
impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}
struct ShutdownOnDrop(watch::Sender<bool>);
impl Drop for ShutdownOnDrop {
    fn drop(&mut self) {
        let _ = self.0.send(true);
    }
}

fn bearer(token: &str) -> blake3::Hash {
    blake3::hash(format!("Bearer {token}").as_bytes())
}

fn outgoing_only_status(status: &Value) -> bool {
    status["incoming_delivery"]["code"] == "delivery_moved_to_ting"
        && status["incoming_delivery"]["provider"] == "ting"
        && status["incoming_delivery"]["forwarding"] == false
}

fn shared_status(status: &Value) -> bool {
    outgoing_only_status(status) && status["host"]["shared"] == true
}

fn dm_relay_status(status: &Value) -> bool {
    status["running"] == true
        && status["pid"].as_u64().is_some()
        && status["version"].as_str().is_some()
        && status["profiles"].is_object()
        && status["queues"].is_object()
}

/// A newer CLI replaces an older shared relay so no home is held to operations
/// the old one cannot parse. Unknown versions are left running.
fn older_than_this(status: &Value) -> bool {
    let running = status["version"]
        .as_str()
        .and_then(|version| semver::Version::parse(version).ok());
    let this = semver::Version::parse(env!("CARGO_PKG_VERSION")).ok();
    matches!((running, this), (Some(running), Some(this)) if running < this)
}

/// A requested port is only a preference: the relay may already serve other
/// homes elsewhere, or have fallen back from a busy port. Report both.
fn on_requested_port(mut status: Value, port: Option<u16>) -> Value {
    if let Some(port) = port
        && status["host"]["port"].as_u64() != Some(u64::from(port))
        && status["host"].is_object()
    {
        status["host"]["requested_port"] = json!(port);
    }
    status
}

fn launch_relay(
    state: &store::Store,
    home: &std::path::Path,
    launch: &DaemonCommand,
) -> Result<tokio::sync::oneshot::Receiver<std::io::Result<std::process::ExitStatus>>> {
    let mut log = store::secure_open(&home.join("daemon.log"))?;
    use std::io::Seek;
    log.seek(std::io::SeekFrom::End(0))?;
    let mut command = std::process::Command::new(&launch.executable);
    command
        .args(&launch.arguments)
        .env("SILICON_DM_HOME", state.directory())
        .env("SILICON_DM_RELAY_HOME", home)
        .stdin(std::process::Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // setsid is async-signal-safe and detaches from the invoking shell/PTY.
        // Do not also set process_group(0): a process-group leader cannot setsid.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
    }
    let mut child = tokio::process::Command::from(command)
        .spawn()
        .context("could not start the shared DM relay")?;
    let (exited, exit_status) = tokio::sync::oneshot::channel();
    // The SDK may live much longer than a CLI invocation. Reap a stopped child
    // without tying its lifetime to the launching command's future.
    tokio::spawn(async move {
        let _ = exited.send(child.wait().await);
    });
    Ok(exit_status)
}

/// Attaches this home to the user's shared relay, launching the relay only
/// when none is running. `port` is a preference for a new relay; a busy port
/// falls back to the next free one.
pub async fn start(
    state: &store::Store,
    launch: &DaemonCommand,
    port: Option<u16>,
) -> Result<Value> {
    if port == Some(0) {
        bail!("relay port must be greater than zero")
    }
    let config = state.load()?;
    if let Ok(Ok(status)) =
        tokio::time::timeout(Duration::from_secs(1), store::relay(&config)?.status()).await
        && shared_status(&status)
        && !older_than_this(&status)
    {
        return Ok(on_requested_port(status, port));
    }
    let home = state.relay_home()?;
    let directory = host::register(&home, &state.directory())?;
    let log = home.join("daemon.log");
    let mut launched = None;
    let mut replacing = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        match host::state(&home)? {
            host::HostState::Running(record) => {
                let owner = crate::relay::RelayClient::new(
                    &format!("http://127.0.0.1:{}/", record.port),
                    record.token,
                )?;
                // Attaching may wait up to 5s for an older per-home relay to stop.
                let attached =
                    tokio::time::timeout(Duration::from_secs(8), owner.attach(&directory)).await;
                let Ok(attached) = attached else {
                    continue_after_pause(deadline, &log).await?;
                    continue;
                };
                match attached {
                    Ok(status) if !older_than_this(&status) => {
                        return Ok(on_requested_port(status, port));
                    }
                    Ok(_) if !replacing => {
                        // Queues stay on disk; the new relay resumes every home.
                        let _ = owner.stop().await;
                        replacing = true;
                    }
                    // Still starting, or the older relay is still stopping.
                    Ok(_) | Err(crate::Error::Transport(_)) => {}
                    Err(_) if replacing => {}
                    Err(error) => {
                        return Err(anyhow::Error::new(error).context(format!(
                            "the shared DM relay could not attach {}",
                            directory.display()
                        )));
                    }
                }
            }
            host::HostState::Stopped => match &mut launched {
                None => {
                    if let Some(port) = port {
                        state.update(|config| {
                            config.relay_port = port;
                            Ok(())
                        })?;
                    }
                    launched = Some(launch_relay(state, &home, launch)?);
                }
                Some(exited) => {
                    if exited.try_recv().is_ok() {
                        bail!(
                            "the shared DM relay exited before becoming ready; inspect {}",
                            log.display()
                        );
                    }
                }
            },
            host::HostState::Starting => {}
        }
        continue_after_pause(deadline, &log).await?;
    }
}

async fn continue_after_pause(deadline: tokio::time::Instant, log: &std::path::Path) -> Result<()> {
    if tokio::time::Instant::now() >= deadline {
        bail!(
            "the shared DM relay did not become ready; inspect {}",
            log.display()
        );
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    Ok(())
}

/// Stops a relay from an older release that served only this home and still
/// holds its lock. Anything unrecognized on its port is left alone.
async fn stop_per_home_relay(state: &store::Store) {
    let Ok(relay) = state.load().and_then(|config| store::relay(&config)) else {
        return;
    };
    if let Ok(Ok(status)) = tokio::time::timeout(Duration::from_secs(1), relay.status()).await
        && dm_relay_status(&status)
        && !shared_status(&status)
    {
        let _ = relay.stop().await;
    }
}

async fn attach_home(host: &Arc<Host>, directory: PathBuf) -> Result<Arc<Tenant>> {
    let _serial = host.attaching.lock().await;
    let directory = host::register(&host.home, &directory).with_context(|| {
        format!(
            "DM home {} is not an existing directory",
            directory.display()
        )
    })?;
    if let Some(tenant) = host.homes().get(&directory) {
        return Ok(tenant.clone());
    }
    let state = store::Store::new(&directory)?;
    let lock = store::secure_open(&directory.join("daemon.lock"))?;
    if lock.try_lock_exclusive().is_err() {
        stop_per_home_relay(&state).await;
        tokio::time::timeout(Duration::from_secs(5), async {
            while lock.try_lock_exclusive().is_err() {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .with_context(|| {
            format!(
                "another DM relay still serves {}; retry after its current outgoing request completes",
                directory.display()
            )
        })?;
    }
    let queue = queue::Queue::open(&state)?;
    queue.expire_transient_commands()?;
    let port = host.port;
    let token = state.update(|config| {
        config.relay_port = port;
        Ok(config.relay_token.clone())
    })?;
    let context = RuntimeContext {
        store: state,
        queue,
        payload_budget: host.payload_budget.clone(),
        workers: host.workers.clone(),
    };
    let tenant = Arc::new(Tenant {
        bearer: bearer(&token),
        outbox: Mutex::new(Some(tokio::spawn(outbox(context.clone())))),
        context,
        _lock: lock,
    });
    host.homes().insert(directory, tenant.clone());
    Ok(tenant)
}

/// Runs the shared relay until local shutdown or Ctrl-C, serving `state` and
/// every home registered before it.
pub async fn run(state: store::Store) -> Result<()> {
    let home = state.relay_home()?;
    let lock = host::acquire(&home).await?;
    let launching = host::register(&home, &state.directory())?;
    let listener = host::bind(state.load()?.relay_port).await?;
    let port = listener.local_addr()?.port();
    let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let (shutdown, mut shutdown_rx) = watch::channel(false);
    // Axum owns spawned connection tasks. Cancelling the host future must also
    // signal those tasks, including idle authenticated keep-alive connections.
    let _shutdown_guard = ShutdownOnDrop(shutdown.clone());
    let host = Arc::new(Host {
        home: home.clone(),
        port,
        attach_bearer: bearer(&token),
        homes: Mutex::default(),
        attaching: tokio::sync::Mutex::default(),
        payload_budget: Arc::new(Semaphore::new(PAYLOAD_BUDGET_MIB as usize)),
        workers: Arc::new(Semaphore::new(WORKERS)),
        shutdown: shutdown.clone(),
    });
    if let Err(error) = attach_home(&host, launching.clone()).await {
        eprintln!("could not attach {}: {error:#}", launching.display());
    }
    let pid = std::process::id();
    host::publish(
        &home,
        &host::HostRecord {
            pid,
            port,
            version: env!("CARGO_PKG_VERSION").into(),
            token,
        },
    )?;
    let router = Router::new()
        .route("/status", get(status))
        .route("/homes", post(attach))
        .route("/requests", post(submit))
        .route("/requests/{id}", get(request_result))
        .route("/requests/{id}/status", get(request_status))
        .route("/shutdown", post(stop))
        .layer(DefaultBodyLimit::max(128 * 1024 * 1024))
        .layer(axum::middleware::from_fn(silicon_dm_protocol::responses))
        .with_state(host.clone());
    // Resume every registered home's queue now, then retry homes that could not
    // be attached yet, such as one whose older per-home relay was busy.
    let attach_host = host.clone();
    let mut attach_task = AbortOnDrop(tokio::spawn(async move {
        let mut timer = tokio::time::interval(Duration::from_secs(30));
        loop {
            timer.tick().await;
            let Ok(registered) = host::registered(&attach_host.home) else {
                continue;
            };
            for directory in registered {
                if attach_host.homes().contains_key(&directory) {
                    continue;
                }
                if let Err(error) = attach_home(&attach_host, directory.clone()).await {
                    eprintln!("could not attach {}: {error:#}", directory.display());
                }
            }
        }
    }));
    let telemetry_host = host.clone();
    let mut telemetry_task = AbortOnDrop(tokio::spawn(async move {
        let mut timer = tokio::time::interval(Duration::from_secs(60));
        loop {
            timer.tick().await;
            let tenants: Vec<_> = telemetry_host.homes().values().cloned().collect();
            for tenant in tenants {
                let Ok(config) = tenant.context.store.load() else {
                    continue;
                };
                for profile in config.profiles.values().filter(|p| p.enabled) {
                    if let Ok(client) = store::client(&config, profile) {
                        let _ = client
                            .with_source("daemon")
                            .telemetry("queue", true, 0)
                            .await;
                    }
                }
            }
        }
    }));
    let server = axum::serve(listener, router).with_graceful_shutdown(async move {
        let _ = shutdown_rx.changed().await;
    });
    eprintln!(
        "DM relay listening at http://dm.localhost:{port} (127.0.0.1) for every DM home of this user; shared state in {}",
        home.display()
    );
    let served = tokio::select! {result=server=>result.map_err(Into::into),_=tokio::signal::ctrl_c()=>{let _=shutdown.send(true);Ok(())}};
    attach_task.0.abort();
    telemetry_task.0.abort();
    let _ = tokio::join!(&mut attach_task.0, &mut telemetry_task.0);
    host::retract(&home, pid);
    let tenants: Vec<_> = std::mem::take(&mut *host.homes()).into_values().collect();
    for tenant in &tenants {
        let outbox = tenant
            .outbox
            .lock()
            .ok()
            .and_then(|mut outbox| outbox.take());
        if let Some(outbox) = outbox {
            outbox.abort();
            let _ = outbox.await;
        }
    }
    drop(tenants);
    FileExt::unlock(&lock)?;
    served
}
fn error(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(json!({"error":{"code":code,"message":message}})),
    )
        .into_response()
}
fn unauthorized() -> Response {
    error(
        StatusCode::UNAUTHORIZED,
        "unauthorized",
        "local relay bearer token required",
    )
}
fn home_status(host: &Host, tenant: &Tenant) -> Response {
    let context = &tenant.context;
    let config = match context.store.load() {
        Ok(config) => config,
        Err(_) => {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "storage_error",
                "cannot load profiles",
            );
        }
    };
    let profiles: BTreeMap<_, _> = config.profiles.iter().map(|(key, profile)| {
        (key, json!({"state":if profile.enabled { "outgoing_only" } else { "logged_out" },
            "member_id":profile.tokens.actor.id,"testing_environment_id":profile.testing_environment_id,
            "incoming_delivery":"delivery_moved_to_ting"}))
    }).collect();
    match context.queue.stats() {
        Ok(queues) => Json(json!({"running":true,"pid":std::process::id(),"version":env!("CARGO_PKG_VERSION"),
            "profiles":profiles,"queues":queues,"incoming_delivery":{
                "code":"delivery_moved_to_ting","provider":"ting","forwarding":false,
                "message":"Configure the local listening endpoint with Ting. Retained legacy DM inbox records are not forwarded."
            },"host":{"shared":true,"port":host.port,"directory":host.home,"homes":host.homes().len()}})).into_response(),
        Err(_) => error(StatusCode::INTERNAL_SERVER_ERROR,"storage_error","local database unavailable")
    }
}
async fn status(State(host): State<Arc<Host>>, headers: HeaderMap) -> Response {
    match host.tenant(&headers) {
        Some(tenant) => home_status(&host, &tenant),
        None => unauthorized(),
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Attach {
    directory: PathBuf,
}
async fn attach(
    State(host): State<Arc<Host>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !host.owner(&headers) {
        return unauthorized();
    }
    let directory = match crate::Envelope::<Attach>::deserialize(&body) {
        Ok(envelope) if envelope.kind == "attach" && envelope.data.directory.is_absolute() => {
            envelope.data.directory
        }
        _ => {
            return error(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "expected type: attach and data.directory: an absolute DM home",
            );
        }
    };
    match attach_home(&host, directory).await {
        Ok(tenant) => home_status(&host, &tenant),
        Err(failure) => error(
            StatusCode::CONFLICT,
            "attach_failed",
            &format!("{failure:#}"),
        ),
    }
}
async fn submit(
    State(host): State<Arc<Host>>,
    headers: HeaderMap,
    Json(original): Json<Value>,
) -> Response {
    let Some(tenant) = host.tenant(&headers) else {
        return unauthorized();
    };
    let context = &tenant.context;
    let envelope = match crate::Envelope::<Value>::deserialize(&original) {
        Ok(envelope) if envelope.kind == "request" => envelope,
        _ => {
            return error(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "expected exactly type: request and data: RelayRequest",
            );
        }
    };
    let request = match RelayRequest::deserialize(&envelope.data) {
        Ok(r) => r,
        Err(_) => {
            return error(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "expected request_id, profile, optional testing_environment_id, and typed request; run dm relay submit --help",
            );
        }
    };
    let config = match context.store.load() {
        Ok(c) => c,
        Err(_) => {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "storage_error",
                "cannot load profiles",
            );
        }
    };
    if store::profile(&config, &request.profile, request.testing_environment_id).is_err() {
        return error(
            StatusCode::BAD_REQUEST,
            "profile_missing",
            "log in to this profile and testing environment first",
        );
    }
    // Capture the current generation before acknowledging a new test mutation.
    // An offline, never-bound send cannot be relabeled after a clean.
    if request.testing_environment_id.is_some() && request.request.is_mutation() {
        let session = store::session_key(&request.profile, request.testing_environment_id);
        if context.queue.generation(&session).ok().flatten().is_none() {
            let discovery = async {
                let profile =
                    store::profile(&config, &request.profile, request.testing_environment_id)?;
                let info = store::client(&config, profile)?.iam().await?;
                if info.testing_environment_id != request.testing_environment_id {
                    bail!("testing environment mismatch");
                }
                let generation = info
                    .testing_generation
                    .context("testing generation unavailable")?;
                context.queue.adopt_generation(&session, Some(generation))
            }
            .await;
            if discovery.is_err() {
                return error(
                    StatusCode::CONFLICT,
                    "testing_generation_unavailable",
                    "Cannot queue this test mutation until HTTP discovery confirms the sandbox generation. Retry after the sandbox is ready.",
                );
            }
        }
    }
    let queued = context.queue.enqueue(&request, &envelope.data);
    let request_id = request.request_id;
    drop(request);
    match queued {
        Ok(()) => (
            StatusCode::ACCEPTED,
            Json(RelayAcknowledgement {
                acknowledged: true,
                request_id,
                request: original,
            }),
        )
            .into_response(),
        Err(e) => error(StatusCode::CONFLICT, "queue_conflict", &e.to_string()),
    }
}
async fn request_status(
    State(host): State<Arc<Host>>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    let Some(tenant) = host.tenant(&headers) else {
        return unauthorized();
    };
    match tenant.context.queue.request_status(id) {
        Ok(Some(status)) => Json(status).into_response(),
        Ok(None) => error(StatusCode::NOT_FOUND, "not_found", "unknown request ID"),
        Err(_) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "storage_error",
            "cannot read request state",
        ),
    }
}
async fn request_result(
    State(host): State<Arc<Host>>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    let Some(tenant) = host.tenant(&headers) else {
        return unauthorized();
    };
    let context = &tenant.context;
    match context.queue.result(id) {
        Ok(Some(mut result)) => {
            result.request = json!({"type":"request", "data":result.request});
            Json(result).into_response()
        }
        Ok(None) => error(StatusCode::NOT_FOUND, "not_found", "unknown request ID"),
        Err(_) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "storage_error",
            "cannot read request state",
        ),
    }
}
/// Stops the shared relay for every home; their queues stay on disk and the
/// next DM command from any home starts it again.
async fn stop(State(host): State<Arc<Host>>, headers: HeaderMap) -> Response {
    if !host.owner(&headers) && host.tenant(&headers).is_none() {
        return unauthorized();
    }
    let _ = host.shutdown.send(true);
    StatusCode::NO_CONTENT.into_response()
}
fn client_error(error: &crate::Error) -> Value {
    match error {
        crate::Error::Api {
            status,
            code,
            message,
            body,
            request_id,
            retry_after,
        } => {
            json!({"status":status,"code":code,"message":message,"body":body,"request_id":request_id,"retry_after":retry_after})
        }
        _ => {
            json!({"code":"transport_error","message":"DM request did not complete"})
        }
    }
}
async fn outbox(context: RuntimeContext) {
    let mut workers = JoinSet::new();
    let mut running = HashMap::new();
    let mut polling = tokio::time::interval(Duration::from_millis(300));
    loop {
        tokio::select! {
            finished = workers.join_next_with_id(), if !workers.is_empty() => {
                match finished {
                    Some(Ok((id, ()))) => { running.remove(&id); }
                    Some(Err(error)) => { running.remove(&error.id()); }
                    None => {}
                }
            }
            _ = polling.tick() => {
                if let Ok(candidates) = context.queue.request_candidates() {
                    for candidate in candidates {
                        if running.values().any(|active| active == &candidate.session) { continue; }
                        let Ok(worker) = context.workers.clone().try_acquire_owned() else { break; };
                        let Some(permit) = reserve_payload(&context, candidate.payload_bytes) else { continue; };
                        let Ok(Some(request)) = context.queue.load_request(candidate.request_id) else { continue; };
                        let context = context.clone();
                        let task = workers.spawn(async move {
                            let _worker = worker;
                            let _permit = permit;
                            execute_request(context, request).await;
                        });
                        running.insert(task.id(), candidate.session);
                    }
                }
            }
        }
    }
}
fn transient_request(request: &RelayRequest) -> bool {
    matches!(request.request, crate::relay::Operation::SetPresence { .. })
}

fn retry_or_finish(context: &RuntimeContext, request: &RelayRequest, detail: &Value) {
    // An activity describes the present moment. Replaying it after recovery can
    // restore stale typing, and holding it pending blocks this session's sends.
    let _ = if transient_request(request) {
        context.queue.finish(request.request_id, None, Some(detail))
    } else {
        context.queue.retry(request.request_id, detail)
    };
}

fn expire_attempted_token(state: &store::Store, session: &str, attempted_token: &str) {
    let _ = state.update(|config| {
        if let Some(profile) = config.profiles.get_mut(session)
            && profile.tokens.access_token == attempted_token
        {
            profile.expires_at = 0;
        }
        Ok(())
    });
}

async fn execute_request(context: RuntimeContext, request: RelayRequest) {
    let session = store::session_key(&request.profile, request.testing_environment_id);
    let (config, profile) = match context.store.fresh_profile(&session).await {
        Ok(p) => p,
        Err(_) => {
            let message = if transient_request(&request) {
                "This activity could not be authenticated; log in again before submitting a current activity."
            } else {
                "log in again to resume this profile's pending requests"
            };
            retry_or_finish(
                &context,
                &request,
                &json!({"code":"authentication_required","message":message}),
            );
            return;
        }
    };
    let mut client = match store::client(&config, &profile) {
        Ok(c) => c,
        Err(_) => {
            retry_or_finish(
                &context,
                &request,
                &json!({
                    "code":"configuration_error","message":"The profile's HTTP client could not be configured; check its backend URL and testing credentials."
                }),
            );
            return;
        }
    };
    // Refresh sandbox authority through HTTP for every write attempt. A socket
    // is no longer a prerequisite, and an old command must never adopt a new
    // generation after a clean or restore.
    if request.testing_environment_id.is_some() && request.request.is_mutation() {
        let info = match client.iam().await {
            Ok(info) => info,
            Err(error) => {
                if error.unauthorized() {
                    expire_attempted_token(&context.store, &session, &profile.tokens.access_token);
                }
                retry_or_finish(&context, &request, &client_error(&error));
                return;
            }
        };
        if info.testing_environment_id != request.testing_environment_id {
            let _ = context.queue.finish(request.request_id, None, Some(&json!({
                "code":"testing_environment_mismatch","message":"HTTP discovery returned another data plane; select the original sandbox before explicitly resubmitting."
            })));
            return;
        }
        let Some(generation) = info.testing_generation.filter(|generation| *generation > 0) else {
            let detail = json!({
                "code":"awaiting_testing_generation","message":"The selected sandbox's HTTP discovery generation is unavailable."
            });
            let _ = if transient_request(&request) {
                context
                    .queue
                    .finish(request.request_id, None, Some(&detail))
            } else {
                context.queue.defer_until_ready(request.request_id, &detail)
            };
            return;
        };
        if context
            .queue
            .adopt_generation(&session, Some(generation))
            .is_err()
        {
            retry_or_finish(
                &context,
                &request,
                &json!({
                    "code":"storage_error","message":"The sandbox generation could not be recorded locally."
                }),
            );
            return;
        }
        if request.testing_generation != Some(generation) {
            let _ = context.queue.finish(request.request_id, None, Some(&json!({
                "code":"testing_generation_changed","message":"The queued command belongs to another or unknown sandbox generation; inspect it before explicitly resubmitting."
            })));
            return;
        }
    }
    if let Some(generation) = request.testing_generation {
        match client.clone().with_testing_generation(generation) {
            Ok(bound) => client = bound,
            Err(error) => {
                let _ = context
                    .queue
                    .finish(request.request_id, None, Some(&client_error(&error)));
                return;
            }
        }
    }
    match request
        .request
        .execute_with_device(&client, &profile.device_id)
        .await
    {
        Ok(value) => {
            let _ = context.queue.finish(request.request_id, Some(&value), None);
        }
        Err(error) => {
            if error.unauthorized() {
                expire_attempted_token(&context.store, &session, &profile.tokens.access_token);
            }
            let detail = client_error(&error);
            // Read failures are returned for the caller to retry. An
            // unavailable discovery provider must not hold later sends.
            let _ = if !transient_request(&request)
                && ((request.request.is_mutation() && error.retryable()) || error.unauthorized())
            {
                context.queue.retry(request.request_id, &detail)
            } else {
                context
                    .queue
                    .finish(request.request_id, None, Some(&detail))
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    fn fixture_store(base: &str, testing: Option<Uuid>) -> Result<store::Store> {
        let root = std::env::temp_dir().join(format!("dm-outgoing-runtime-{}", Uuid::new_v4()));
        let state = store::Store::new(&root)?.with_relay_home(root.join("shared-relay"))?;
        let profile: store::Profile = serde_json::from_value(json!({
            "name":"default","base_url":base,"webhook_url":format!("{base}/events"),
            "device_id":"outgoing-device","expires_at":store::now()+3600,
            "testing_environment_id":testing,"enabled":true,
            "tokens":{"access_token":"fixture","refresh_token":"fixture-refresh","token_type":"Bearer","expires_in":3600,
                "scope":"dm","actor":{"type":"carbon","id":"c:bob"},"organization_id":"tos"}
        }))?;
        let port = std::net::TcpListener::bind("127.0.0.1:0")?
            .local_addr()?
            .port();
        state.update(|config| {
            config.telemetry_enabled = false;
            config.relay_port = port;
            config
                .profiles
                .insert(store::session_key("default", testing), profile);
            if let Some(id) = testing {
                config.testing_keys.insert(
                    id,
                    store::TestKey {
                        key: "a".repeat(32),
                        base_url: base.into(),
                    },
                );
            }
            Ok(())
        })?;
        Ok(state)
    }

    fn command(testing: Option<Uuid>, generation: Option<i64>) -> RelayRequest {
        RelayRequest {
            request_id: Uuid::new_v4(),
            profile: "default".into(),
            testing_environment_id: testing,
            testing_generation: generation,
            request: crate::relay::Operation::CreateConversation {
                participant_ids: vec!["c:alice".into(), "c:bob".into()],
                idempotency_key: "stable-outgoing-key".into(),
            },
        }
    }

    fn activity(testing: Option<Uuid>, generation: Option<i64>) -> RelayRequest {
        RelayRequest {
            request: crate::relay::Operation::SetPresence {
                activity: Some(crate::Activity::Typing),
            },
            ..command(testing, generation)
        }
    }

    #[tokio::test]
    async fn failed_transient_activity_does_not_retry_or_block_a_durable_send() -> Result<()> {
        let activities = Arc::new(AtomicUsize::new(0));
        let sends = Arc::new(AtomicUsize::new(0));
        let activity_count = activities.clone();
        let send_count = sends.clone();
        let router = Router::new()
            .route("/api/v1/presence/devices/outgoing-device", axum::routing::put(move || {
                let count = activity_count.clone();
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"type":"error","data":{"error":{
                        "code":"presence_unavailable","message":"fixture presence failure"
                    }}})))
                }
            }))
            .route("/api/v1/conversations/c:alice::c:bob/messages", post(move |headers: HeaderMap, Json(body): Json<Value>| {
                let count = send_count.clone();
                async move {
                    assert_eq!(headers["idempotency-key"], "following-send");
                    assert_eq!(body["data"]["message"], "durable message");
                    count.fetch_add(1, Ordering::SeqCst);
                    Json(json!({"type":"message","data":{
                        "message-id":"001","conversation_id":"c:alice::c:bob","sender":{"type":"carbon","id":"c:bob"},
                        "message":"durable message","created_at":"2026-09-22T00:00:00Z"
                    }}))
                }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let state = fixture_store(&format!("http://{}", listener.local_addr()?), None)?;
        let server = tokio::spawn(async move { axum::serve(listener, router).await });
        let context = RuntimeContext {
            queue: queue::Queue::open(&state)?,
            store: state.clone(),
            payload_budget: Arc::new(Semaphore::new(PAYLOAD_BUDGET_MIB as usize)),
            workers: Arc::new(Semaphore::new(WORKERS)),
        };
        let presence = activity(None, None);
        let send = RelayRequest {
            request: crate::relay::Operation::SendMessage {
                conversation_id: "c:alice::c:bob".into(),
                message: crate::MessageCreate {
                    text: Some("durable message".into()),
                    ..Default::default()
                },
                idempotency_key: "following-send".into(),
            },
            ..command(None, None)
        };
        context
            .queue
            .enqueue(&presence, &serde_json::to_value(&presence)?)?;
        context
            .queue
            .enqueue(&send, &serde_json::to_value(&send)?)?;
        // Exercise the FIFO scheduler; startup expiry cannot satisfy this test.
        let worker = AbortOnDrop(tokio::spawn(outbox(context.clone())));
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if context
                    .queue
                    .result(send.request_id)?
                    .is_some_and(|r| r.state == "completed")
                {
                    return Ok::<(), anyhow::Error>(());
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await??;
        let failed = context.queue.result(presence.request_id)?.unwrap();
        assert_eq!(failed.state, "failed");
        assert_eq!(failed.error.unwrap()["code"], "presence_unavailable");
        assert_eq!(activities.load(Ordering::SeqCst), 1);
        assert_eq!(sends.load(Ordering::SeqCst), 1);
        assert!(context.queue.request_candidates()?.is_empty());
        worker.0.abort();
        server.abort();
        let _ = server.await;
        drop(context);
        std::fs::remove_dir_all(state.directory())?;
        Ok(())
    }

    #[tokio::test]
    async fn transient_activity_finishes_on_auth_configuration_and_discovery_failures() -> Result<()>
    {
        for case in [
            "logged_out",
            "refresh_unavailable",
            "invalid_url",
            "missing_test_key",
            "discovery_unavailable",
            "generation_unavailable",
        ] {
            let environment = Uuid::new_v4();
            let router = Router::new().route("/api/v1/iam", get(move || async move {
                if case == "generation_unavailable" {
                    Json(json!({"type":"iam","data":{"app_id":"dm","iam_base_url":"http://localhost",
                        "api_base_url":"http://localhost","testing_environment_id":environment,"testing_generation":null}})).into_response()
                } else {
                    (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"type":"error","data":{"error":{
                        "code":"discovery_unavailable","message":"fixture discovery failure"
                    }}}))).into_response()
                }
            })).fallback(|| async { StatusCode::SERVICE_UNAVAILABLE });
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
            let state = fixture_store(
                &format!("http://{}", listener.local_addr()?),
                Some(environment),
            )?;
            let server = tokio::spawn(async move { axum::serve(listener, router).await });
            let session = store::session_key("default", Some(environment));
            state.update(|config| {
                let profile = config.profiles.get_mut(&session).unwrap();
                match case {
                    "logged_out" => profile.enabled = false,
                    "refresh_unavailable" => profile.expires_at = 0,
                    "invalid_url" => profile.base_url = "invalid URL".into(),
                    "missing_test_key" => {
                        config.testing_keys.clear();
                    }
                    _ => {}
                }
                Ok(())
            })?;
            let context = RuntimeContext {
                queue: queue::Queue::open(&state)?,
                store: state.clone(),
                payload_budget: Arc::new(Semaphore::new(PAYLOAD_BUDGET_MIB as usize)),
                workers: Arc::new(Semaphore::new(WORKERS)),
            };
            context.queue.adopt_generation(&session, Some(1))?;
            let presence = activity(Some(environment), Some(1));
            let following = command(Some(environment), Some(1));
            context
                .queue
                .enqueue(&presence, &serde_json::to_value(&presence)?)?;
            context
                .queue
                .enqueue(&following, &serde_json::to_value(&following)?)?;
            execute_request(context.clone(), presence.clone()).await;
            let result = context.queue.result(presence.request_id)?.unwrap();
            assert_eq!(result.state, "failed", "{case}");
            assert!(result.error.is_some(), "{case}");
            let candidates = context.queue.request_candidates()?;
            assert_eq!(candidates.len(), 1, "{case}");
            assert_eq!(candidates[0].request_id, following.request_id, "{case}");
            server.abort();
            let _ = server.await;
            drop(context);
            std::fs::remove_dir_all(state.directory())?;
        }
        Ok(())
    }

    #[tokio::test]
    async fn late_unauthorized_response_expires_only_the_attempted_token() -> Result<()> {
        for testing in [None, Some(Uuid::new_v4())] {
            for replace_token in [false, true] {
                let entered = Arc::new(tokio::sync::Notify::new());
                let released = Arc::new(tokio::sync::Notify::new());
                let entered_handler = entered.clone();
                let released_handler = released.clone();
                let path = if testing.is_some() {
                    "/api/v1/iam"
                } else {
                    "/api/v1/conversations"
                };
                let router = Router::new().route(
                    path,
                    axum::routing::any(move |headers: HeaderMap| {
                        let entered = entered_handler.clone();
                        let released = released_handler.clone();
                        async move {
                            assert_eq!(headers["authorization"], "Bearer fixture");
                            entered.notify_one();
                            released.notified().await;
                            (
                                StatusCode::UNAUTHORIZED,
                                Json(json!({"type":"error","data":{"error":{
                                    "code":"unauthorized","message":"fixture expired token"
                                }}})),
                            )
                        }
                    }),
                );
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
                let state = fixture_store(&format!("http://{}", listener.local_addr()?), testing)?;
                let server = tokio::spawn(async move { axum::serve(listener, router).await });
                let context = RuntimeContext {
                    queue: queue::Queue::open(&state)?,
                    store: state.clone(),
                    payload_budget: Arc::new(Semaphore::new(PAYLOAD_BUDGET_MIB as usize)),
                    workers: Arc::new(Semaphore::new(WORKERS)),
                };
                let session = store::session_key("default", testing);
                let generation = testing.map(|_| 1);
                context.queue.adopt_generation(&session, generation)?;
                let request = command(testing, generation);
                context
                    .queue
                    .enqueue(&request, &serde_json::to_value(&request)?)?;
                let attempt = tokio::spawn(execute_request(context.clone(), request.clone()));
                tokio::time::timeout(Duration::from_secs(2), entered.notified()).await?;
                let refreshed_expiry = store::now() + 7200;
                if replace_token {
                    state.update(|config| {
                        let profile = config.profiles.get_mut(&session).unwrap();
                        profile.tokens.access_token = "new-login-token".into();
                        profile.tokens.refresh_token = "new-login-refresh".into();
                        profile.expires_at = refreshed_expiry;
                        Ok(())
                    })?;
                }
                released.notify_one();
                tokio::time::timeout(Duration::from_secs(2), attempt).await??;
                let saved = state.load()?.profiles[&session].clone();
                assert_eq!(
                    saved.expires_at,
                    if replace_token { refreshed_expiry } else { 0 }
                );
                assert_eq!(
                    saved.tokens.access_token,
                    if replace_token {
                        "new-login-token"
                    } else {
                        "fixture"
                    }
                );
                assert_eq!(
                    context.queue.result(request.request_id)?.unwrap().state,
                    "pending"
                );
                server.abort();
                let _ = server.await;
                drop(context);
                std::fs::remove_dir_all(state.directory())?;
            }
        }
        Ok(())
    }

    fn missing_executable(state: &store::Store) -> DaemonCommand {
        DaemonCommand {
            executable: state.directory().join("must-not-launch"),
            arguments: vec![],
        }
    }

    async fn wait_until_attached(state: &store::Store) -> Result<Value> {
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                // A port that accepts but never answers must not stall the poll.
                let relay = store::relay(&state.load()?)?;
                if let Ok(Ok(status)) =
                    tokio::time::timeout(Duration::from_millis(500), relay.status()).await
                    && shared_status(&status)
                {
                    return Ok::<Value, anyhow::Error>(status);
                }
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        })
        .await?
    }

    #[tokio::test]
    async fn start_reuses_an_attached_shared_relay_and_leaves_unrelated_services_running()
    -> Result<()> {
        let mode = Arc::new(AtomicUsize::new(0));
        let stopped = Arc::new(AtomicUsize::new(0));
        let mode_handler = mode.clone();
        let stopped_handler = stopped.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let router = Router::new()
            .route("/status", get(move || {
                let mode = mode_handler.clone();
                async move {
                    let value = if mode.load(Ordering::SeqCst) == 0 {
                        json!({"running":true})
                    } else {
                        json!({"running":true,"pid":123,"version":env!("CARGO_PKG_VERSION"),"profiles":{},"queues":{},
                            "incoming_delivery":{"code":"delivery_moved_to_ting","provider":"ting","forwarding":false},
                            "host":{"shared":true,"port":port}})
                    };
                    Json(json!({"type":"relay_status","data":value}))
                }
            }))
            .route("/shutdown", post(move || {
                let stopped = stopped_handler.clone();
                async move {
                    stopped.fetch_add(1, Ordering::SeqCst);
                    StatusCode::NO_CONTENT
                }
            }));
        let state = fixture_store("http://localhost", None)?;
        state.update(|config| {
            config.relay_port = port;
            Ok(())
        })?;
        let server = tokio::spawn(async move { axum::serve(listener, router).await });
        // This test executable exits after listing tests; it cannot open a relay.
        let launch = DaemonCommand {
            executable: std::env::current_exe()?,
            arguments: vec!["--list".into()],
        };
        let unrelated = start(&state, &launch, None).await.unwrap_err();
        assert!(
            unrelated
                .to_string()
                .contains("exited before becoming ready")
        );
        assert_eq!(stopped.load(Ordering::SeqCst), 0);
        mode.store(1, Ordering::SeqCst);
        let missing = missing_executable(&state);
        assert!(shared_status(&start(&state, &missing, None).await?));
        let same = start(&state, &missing, Some(port)).await?;
        assert!(same["host"].get("requested_port").is_none());
        // Another port is a preference for a new relay; the running one is kept.
        let other = port.wrapping_add(1).max(1);
        let kept = start(&state, &missing, Some(other)).await?;
        assert_eq!(kept["host"]["port"], port);
        assert_eq!(kept["host"]["requested_port"], other);
        assert_eq!(stopped.load(Ordering::SeqCst), 0);
        server.abort();
        let _ = server.await;
        std::fs::remove_dir_all(state.directory())?;
        Ok(())
    }

    #[tokio::test]
    async fn every_home_shares_one_relay_on_a_fallback_port_with_private_queues() -> Result<()> {
        let root = std::env::temp_dir().join(format!("dm-shared-relay-{}", Uuid::new_v4()));
        let shared = root.join("shared-relay");
        let first = fixture_store("http://localhost", None)?.with_relay_home(&shared)?;
        let second = fixture_store("http://localhost", None)?.with_relay_home(&shared)?;
        let occupied = std::net::TcpListener::bind("127.0.0.1:0")?;
        let busy = occupied.local_addr()?.port();
        first.update(|config| {
            config.relay_port = busy;
            Ok(())
        })?;
        let host_state = first.clone();
        let relay = tokio::spawn(async move { run(host_state).await });
        let first_status = wait_until_attached(&first).await?;
        let port = first.load()?.relay_port;
        assert_ne!(port, busy, "a busy preferred port must fall back");
        assert_eq!(first_status["host"]["port"], port);

        // A second home attaches to the running relay instead of launching one.
        let second_status = start(&second, &missing_executable(&second), None).await?;
        assert_eq!(second_status["pid"], first_status["pid"]);
        assert_eq!(second_status["host"]["port"], port);
        assert_eq!(second_status["host"]["homes"], 2);
        assert_eq!(second.load()?.relay_port, port);
        assert!(run(second.clone()).await.is_err(), "only one relay may run");

        // Each home's bearer reaches only its own queue.
        let request = command(None, None);
        let second_relay = store::relay(&second.load()?)?;
        second_relay.submit(&request).await?;
        assert!(second_relay.result(request.request_id).await.is_ok());
        let first_relay = store::relay(&first.load()?)?;
        assert!(matches!(
            first_relay.result(request.request_id).await,
            Err(crate::Error::Api { status: 404, .. })
        ));

        first_relay.stop().await?;
        tokio::time::timeout(Duration::from_secs(3), relay).await???;
        assert!(matches!(host::state(&shared)?, host::HostState::Stopped));
        assert!(!shared.join("host.json").exists());
        for home in [&first, &second] {
            let lock = store::secure_open(&home.directory().join("daemon.lock"))?;
            lock.try_lock_exclusive()?;
        }
        drop(occupied);
        for home in [first, second] {
            std::fs::remove_dir_all(home.directory())?;
        }
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[tokio::test]
    async fn a_newer_client_replaces_an_older_shared_relay() -> Result<()> {
        let state = fixture_store("http://localhost", None)?;
        let shared = state.relay_home()?;
        // Stand in for an older shared relay: it holds the host lock and
        // publishes its record until it is asked to stop.
        let held = host::lock_file(&shared)?;
        held.try_lock_exclusive()?;
        let held = Arc::new(Mutex::new(Some(held)));
        let stopped = Arc::new(AtomicUsize::new(0));
        let (release, stopped_handler) = (held.clone(), stopped.clone());
        let router = Router::new()
            .route("/homes", post(|| async {
                Json(json!({"type":"relay_status","data":{"running":true,"pid":123,"version":"0.0.1",
                    "profiles":{},"queues":{},"incoming_delivery":{"code":"delivery_moved_to_ting","provider":"ting","forwarding":false},
                    "host":{"shared":true,"port":1}}}))
            }))
            .route("/shutdown", post(move || {
                let (release, stopped) = (release.clone(), stopped_handler.clone());
                async move {
                    stopped.fetch_add(1, Ordering::SeqCst);
                    release.lock().unwrap().take();
                    StatusCode::NO_CONTENT
                }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        host::publish(
            &shared,
            &host::HostRecord {
                pid: 123,
                port: listener.local_addr()?.port(),
                version: "0.0.1".into(),
                token: "older-relay".into(),
            },
        )?;
        let server = tokio::spawn(async move { axum::serve(listener, router).await });
        let replaced = start(&state, &missing_executable(&state), None)
            .await
            .unwrap_err();
        // Once the older relay released its lock, this client launched its own.
        assert!(
            replaced
                .to_string()
                .contains("could not start the shared DM relay")
        );
        assert_eq!(stopped.load(Ordering::SeqCst), 1);
        assert!(older_than_this(&json!({"version":"0.0.1"})));
        assert!(!older_than_this(
            &json!({"version":env!("CARGO_PKG_VERSION")})
        ));
        assert!(!older_than_this(&json!({"version":"99.0.0"})));
        assert!(!older_than_this(&json!({})));
        server.abort();
        let _ = server.await;
        std::fs::remove_dir_all(state.directory())?;
        Ok(())
    }

    #[tokio::test]
    async fn an_older_per_home_relay_is_stopped_when_its_home_attaches() -> Result<()> {
        let state = fixture_store("http://localhost", None)?;
        let held = store::secure_open(&state.directory().join("daemon.lock"))?;
        held.try_lock_exclusive()?;
        let held = Arc::new(Mutex::new(Some(held)));
        let stopped = Arc::new(AtomicUsize::new(0));
        let (release, stopped_handler) = (held.clone(), stopped.clone());
        let router = Router::new()
            .route("/status", get(|| async {
                Json(json!({"type":"relay_status","data":{"running":true,"pid":123,"version":"0.11.1",
                    "profiles":{},"queues":{},"incoming_delivery":{"code":"delivery_moved_to_ting","provider":"ting","forwarding":false}}}))
            }))
            .route("/shutdown", post(move || {
                let (release, stopped) = (release.clone(), stopped_handler.clone());
                async move {
                    stopped.fetch_add(1, Ordering::SeqCst);
                    release.lock().unwrap().take();
                    StatusCode::NO_CONTENT
                }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let legacy_port = listener.local_addr()?.port();
        state.update(|config| {
            config.relay_port = legacy_port;
            Ok(())
        })?;
        let server = tokio::spawn(async move { axum::serve(listener, router).await });
        let host_state = state.clone();
        let relay = tokio::spawn(async move { run(host_state).await });
        let status = wait_until_attached(&state).await?;
        assert_eq!(stopped.load(Ordering::SeqCst), 1);
        assert!(held.lock().unwrap().is_none());
        assert_ne!(status["host"]["port"], legacy_port);
        store::relay(&state.load()?)?.stop().await?;
        tokio::time::timeout(Duration::from_secs(3), relay).await???;
        server.abort();
        let _ = server.await;
        std::fs::remove_dir_all(state.directory())?;
        Ok(())
    }

    #[tokio::test]
    async fn outgoing_relay_retries_http_without_connecting_or_forwarding_incoming_delivery()
    -> Result<()> {
        let sends = Arc::new(AtomicUsize::new(0));
        let unexpected = Arc::new(AtomicUsize::new(0));
        let sends_handler = sends.clone();
        let unexpected_handler = unexpected.clone();
        let router = Router::new().route("/api/v1/conversations", post(move |headers: HeaderMap, Json(body): Json<Value>| {
            let sends = sends_handler.clone();
            async move {
                assert_eq!(headers["idempotency-key"], "stable-outgoing-key");
                assert_eq!(body, json!({"type":"create_conversation","data":{"participant_ids":["c:alice","c:bob"]}}));
                if sends.fetch_add(1, Ordering::SeqCst) == 0 {
                    return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"type":"error","data":{"error":{"code":"retry","message":"fixture retry"}}}))).into_response();
                }
                Json(json!({"type":"conversation","data":{
                    "id":"c:alice::c:bob","org_id":"tos","participants":[],"last_message":null,
                    "created_at":"2026-09-22T00:00:00Z","updated_at":"2026-09-22T00:00:00Z"
                }})).into_response()
            }
        })).fallback(move || {
            let counter = unexpected_handler.clone();
            async move { counter.fetch_add(1, Ordering::SeqCst); StatusCode::NOT_FOUND }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let state = fixture_store(&format!("http://{}", listener.local_addr()?), None)?;
        let server = tokio::spawn(async move { axum::serve(listener, router).await });
        let queue = queue::Queue::open(&state)?;
        let legacy = json!({"type":"message.created","data":{"message-id":"000","conversation_id":"c:alice::c:bob",
            "message":"retained","sender":{"type":"carbon","id":"c:alice"},"recipient_id":"c:bob",
            "metadata":{"source":"dm","delivery_id":Uuid::nil(),"delivery_sequence":1}}});
        let legacy_db = rusqlite::Connection::open(state.directory().join("relay.sqlite3"))?;
        legacy_db.execute_batch("CREATE TABLE inbox(session TEXT,delivery_id TEXT,actor TEXT,sequence INTEGER,frame TEXT,delivered INTEGER NOT NULL DEFAULT 0,attempts INTEGER NOT NULL DEFAULT 0,next_attempt INTEGER NOT NULL DEFAULT 0);")?;
        legacy_db.execute(
            "INSERT INTO inbox(session,delivery_id,actor,sequence,frame) VALUES('default:production',?1,'c:bob',1,?2)",
            rusqlite::params![Uuid::nil().to_string(), legacy.to_string()],
        )?;
        let request = command(None, None);
        queue.enqueue(&request, &serde_json::to_value(&request)?)?;
        let relay = store::relay(&state.load()?)?;
        let daemon_state = state.clone();
        let daemon = tokio::spawn(async move { run(daemon_state).await });
        let final_status = tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                if relay
                    .request_status(request.request_id)
                    .await
                    .is_ok_and(|v| v.state == "completed")
                {
                    break relay.status().await;
                }
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        })
        .await??;
        assert_eq!(sends.load(Ordering::SeqCst), 2);
        assert_eq!(unexpected.load(Ordering::SeqCst), 0);
        assert_eq!(
            final_status["incoming_delivery"]["code"],
            "delivery_moved_to_ting"
        );
        assert_eq!(final_status["incoming_delivery"]["forwarding"], false);
        assert_eq!(
            final_status["queues"]["retained_legacy_pending_deliveries"],
            1
        );
        assert_eq!(final_status["queues"]["pending_webhooks"], 0);
        let result = relay.result(request.request_id).await?;
        assert_eq!(result.request["data"], serde_json::to_value(&request)?);
        relay.stop().await?;
        tokio::time::timeout(Duration::from_secs(2), daemon).await???;
        server.abort();
        let _ = server.await;
        drop(queue);
        drop(legacy_db);
        std::fs::remove_dir_all(state.directory())?;
        Ok(())
    }

    #[tokio::test]
    async fn http_discovery_rejects_stale_sandbox_commands_before_the_mutation() -> Result<()> {
        let environment = Uuid::new_v4();
        let mutations = Arc::new(AtomicUsize::new(0));
        let counter = mutations.clone();
        let router = Router::new().route("/api/v1/iam", get(move || async move {
            Json(json!({"type":"iam","data":{"app_id":"dm","iam_base_url":"http://localhost",
                "api_base_url":"http://localhost","testing_environment_id":environment,"testing_generation":2}}))
        })).fallback(move || {
            let counter = counter.clone();
            async move { counter.fetch_add(1, Ordering::SeqCst); StatusCode::NOT_FOUND }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let state = fixture_store(
            &format!("http://{}", listener.local_addr()?),
            Some(environment),
        )?;
        let server = tokio::spawn(async move { axum::serve(listener, router).await });
        let context = RuntimeContext {
            queue: queue::Queue::open(&state)?,
            store: state.clone(),
            payload_budget: Arc::new(Semaphore::new(PAYLOAD_BUDGET_MIB as usize)),
            workers: Arc::new(Semaphore::new(WORKERS)),
        };
        let request = command(Some(environment), Some(1));
        context
            .queue
            .adopt_generation(&store::session_key("default", Some(environment)), Some(1))?;
        context
            .queue
            .enqueue(&request, &serde_json::to_value(&request)?)?;
        execute_request(context.clone(), request.clone()).await;
        let result = context.queue.result(request.request_id)?.unwrap();
        assert_eq!(result.state, "failed");
        assert_eq!(result.error.unwrap()["code"], "testing_generation_changed");
        assert_eq!(mutations.load(Ordering::SeqCst), 0);
        server.abort();
        let _ = server.await;
        drop(context);
        std::fs::remove_dir_all(state.directory())?;
        Ok(())
    }
}
