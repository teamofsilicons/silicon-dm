use super::{DaemonCommand, queue, store};
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
    sync::Arc,
    time::Duration,
};
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore, watch},
    task::{JoinHandle, JoinSet},
};
use uuid::Uuid;

#[derive(Clone)]
struct App {
    context: RuntimeContext,
    token: String,
    shutdown: watch::Sender<bool>,
}

#[derive(Clone)]
struct RuntimeContext {
    store: store::Store,
    queue: queue::Queue,
    payload_budget: Arc<Semaphore>,
}
// Count MiB of encoded queued work, not just tasks. Deserialization and HTTP
// serialization need additional memory. A larger single item takes the entire
// budget so it can progress, but cannot overlap another outgoing request.
const PAYLOAD_BUDGET_MIB: u32 = 128;
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

fn outgoing_only_status(status: &Value) -> bool {
    status["incoming_delivery"]["code"] == "delivery_moved_to_ting"
        && status["incoming_delivery"]["provider"] == "ting"
        && status["incoming_delivery"]["forwarding"] == false
}

fn dm_relay_status(status: &Value) -> bool {
    status["running"] == true
        && status["pid"].as_u64().is_some()
        && status["version"].as_str().is_some()
        && status["profiles"].is_object()
        && status["queues"].is_object()
}

pub async fn start(
    state: &store::Store,
    launch: &DaemonCommand,
    port: Option<u16>,
) -> Result<Value> {
    let config = state.load()?;
    if let Ok(Ok(status)) =
        tokio::time::timeout(Duration::from_secs(1), store::relay(&config)?.status()).await
    {
        if outgoing_only_status(&status) {
            return Ok(status);
        }
        if !dm_relay_status(&status) {
            bail!("the configured relay port returned an unrecognized service; it was not stopped");
        }
        // An explicitly started upgraded runtime must not silently reuse a
        // process that still owns incoming delivery. Its durable data stays put.
        store::relay(&config)?.stop().await?;
        let lock = store::secure_open(&state.directory().join("daemon.lock"))?;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if lock.try_lock_exclusive().is_ok() {
                    FileExt::unlock(&lock)?;
                    return Ok::<(), anyhow::Error>(());
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .context("the legacy DM relay is still stopping; retry after its current outgoing request completes")??;
    }
    if let Some(port) = port {
        if port == 0 {
            bail!("relay port must be greater than zero")
        }
        state.update(|c| {
            c.relay_port = port;
            Ok(())
        })?;
    }
    let mut log = store::secure_open(&state.directory().join("daemon.log"))?;
    use std::io::Seek;
    log.seek(std::io::SeekFrom::End(0))?;
    let executable = &launch.executable;
    let mut command = std::process::Command::new(executable);
    command
        .args(&launch.arguments)
        .env("SILICON_DM_HOME", state.directory())
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
        .context("could not start relay daemon")?;
    let (exited, mut exit_status) = tokio::sync::oneshot::channel();
    // The SDK may live much longer than a CLI invocation. Reap a stopped child
    // without tying its lifetime to the launching command's future.
    tokio::spawn(async move {
        let _ = exited.send(child.wait().await);
    });
    let readiness = async {
        loop {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let relay = store::relay(&state.load()?)?;
            if let Ok(status) = relay.status().await {
                if outgoing_only_status(&status) {
                    return Ok::<Value, anyhow::Error>(status);
                }
                if dm_relay_status(&status) {
                    relay.stop().await?;
                }
                bail!(
                    "the launched relay does not support Ting delivery ownership; install the matching DM relay executable"
                );
            }
        }
    };
    tokio::select! {
        result = tokio::time::timeout(Duration::from_secs(5), readiness) => {
            if let Ok(result) = result { return result; }
        }
        _ = &mut exit_status => {
            bail!("relay exited before becoming ready; inspect {}", state.directory().join("daemon.log").display());
        }
    }
    bail!(
        "relay did not become ready; inspect {}",
        state.directory().join("daemon.log").display()
    )
}
pub async fn run(state: store::Store) -> Result<()> {
    let lock = store::secure_open(&state.directory().join("daemon.lock"))?;
    lock.try_lock_exclusive()
        .context("a DM relay daemon is already running")?;
    let context = RuntimeContext {
        queue: queue::Queue::open(&state)?,
        store: state,
        payload_budget: Arc::new(Semaphore::new(PAYLOAD_BUDGET_MIB as usize)),
    };
    context.queue.expire_transient_commands()?;
    let config = context.store.load()?;
    let listener =
        tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, config.relay_port)).await?;
    let (shutdown, mut shutdown_rx) = watch::channel(false);
    // Axum owns spawned connection tasks. Cancelling the host future must also
    // signal those tasks, including idle authenticated keep-alive connections.
    let _shutdown_guard = ShutdownOnDrop(shutdown.clone());
    let app = App {
        context: context.clone(),
        token: config.relay_token.clone(),
        shutdown: shutdown.clone(),
    };
    let router = Router::new()
        .route("/status", get(status))
        .route("/requests", post(submit))
        .route("/requests/{id}", get(request_result))
        .route("/requests/{id}/status", get(request_status))
        .route("/shutdown", post(stop))
        .layer(DefaultBodyLimit::max(128 * 1024 * 1024))
        .layer(axum::middleware::from_fn(silicon_dm_protocol::responses))
        .with_state(app);
    let mut outgoing = AbortOnDrop(tokio::spawn(outbox(context.clone())));
    let telemetry_store = context.store.clone();
    let mut telemetry_task = AbortOnDrop(tokio::spawn(async move {
        let mut timer = tokio::time::interval(Duration::from_secs(60));
        loop {
            timer.tick().await;
            if let Ok(config) = telemetry_store.load() {
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
        "DM relay listening at http://dm.localhost:{} (127.0.0.1); durable state in {}",
        config.relay_port,
        context.store.directory().display()
    );
    tokio::select! {result=server=>{result?;},_=tokio::signal::ctrl_c()=>{let _=shutdown.send(true);}}
    outgoing.0.abort();
    telemetry_task.0.abort();
    let _ = tokio::join!(&mut outgoing.0, &mut telemetry_task.0);
    FileExt::unlock(&lock)?;
    Ok(())
}
fn authorized(headers: &HeaderMap, app: &App) -> bool {
    if *app.shutdown.borrow() || headers.contains_key("origin") {
        return false;
    }
    let expected = blake3::hash(format!("Bearer {}", app.token).as_bytes());
    headers
        .get("authorization")
        .is_some_and(|value| blake3::hash(value.as_bytes()) == expected)
}
fn error(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(json!({"error":{"code":code,"message":message}})),
    )
        .into_response()
}
async fn status(State(app): State<App>, headers: HeaderMap) -> Response {
    let context = &app.context;
    if !authorized(&headers, &app) {
        return error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "local relay bearer token required",
        );
    }
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
            }})).into_response(),
        Err(_) => error(StatusCode::INTERNAL_SERVER_ERROR,"storage_error","local database unavailable")
    }
}
async fn submit(
    State(app): State<App>,
    headers: HeaderMap,
    Json(original): Json<Value>,
) -> Response {
    let context = &app.context;
    if !authorized(&headers, &app) {
        return error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "local relay bearer token required",
        );
    }
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
    State(app): State<App>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    if !authorized(&headers, &app) {
        return error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "local relay bearer token required",
        );
    }
    match app.context.queue.request_status(id) {
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
    State(app): State<App>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    let context = &app.context;
    if !authorized(&headers, &app) {
        return error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "local relay bearer token required",
        );
    }
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
async fn stop(State(app): State<App>, headers: HeaderMap) -> Response {
    if !authorized(&headers, &app) {
        return error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "local relay bearer token required",
        );
    }
    let _ = app.shutdown.send(true);
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
                        if running.len() >= 16 { break; }
                        if running.values().any(|active| active == &candidate.session) { continue; }
                        let Some(permit) = reserve_payload(&context, candidate.payload_bytes) else { continue; };
                        let Ok(Some(request)) = context.queue.load_request(candidate.request_id) else { continue; };
                        let context = context.clone();
                        let task = workers.spawn(async move {
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn fixture_store(base: &str, testing: Option<Uuid>) -> Result<store::Store> {
        let root = std::env::temp_dir().join(format!("dm-outgoing-runtime-{}", Uuid::new_v4()));
        let state = store::Store::new(root)?;
        let profile: store::Profile = serde_json::from_value(json!({
            "name":"default","base_url":base,"webhook_url":format!("{base}/events"),
            "device_id":"outgoing-device","expires_at":store::now()+3600,
            "testing_environment_id":testing,"enabled":true,
            "tokens":{"access_token":"fixture","refresh_token":"fixture-refresh","token_type":"Bearer","expires_in":3600,
                "scope":"dm","actor":{"type":"silicon","id":"bob"},"organization_id":"tos"}
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
                participant_ids: vec!["alice".into(), "bob".into()],
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
            .route("/api/v1/conversations/alice::bob/messages", post(move |headers: HeaderMap, Json(body): Json<Value>| {
                let count = send_count.clone();
                async move {
                    assert_eq!(headers["idempotency-key"], "following-send");
                    assert_eq!(body["data"]["message"], "durable message");
                    count.fetch_add(1, Ordering::SeqCst);
                    Json(json!({"type":"message","data":{
                        "message-id":"001","conversation_id":"alice::bob","sender":{"type":"silicon","id":"bob"},
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
        };
        let presence = activity(None, None);
        let send = RelayRequest {
            request: crate::relay::Operation::SendMessage {
                conversation_id: "alice::bob".into(),
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
                    Json(json!({"type":"iam","data":{"app_id":"tos>dm","iam_base_url":"http://localhost",
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

    #[tokio::test]
    async fn start_reuses_only_ting_aware_relays_and_stops_only_recognized_legacy_relays()
    -> Result<()> {
        let mode = Arc::new(AtomicUsize::new(0));
        let stopped = Arc::new(AtomicUsize::new(0));
        let mode_handler = mode.clone();
        let mode_shutdown = mode.clone();
        let stopped_handler = stopped.clone();
        let router = Router::new()
            .route("/status", get(move || {
                let mode = mode_handler.clone();
                async move {
                    if mode.load(Ordering::SeqCst) == 3 {
                        return StatusCode::SERVICE_UNAVAILABLE.into_response();
                    }
                    let mut value = json!({"running":true});
                    if mode.load(Ordering::SeqCst) > 0 {
                        value = json!({"running":true,"pid":123,"version":"legacy","profiles":{},"queues":{}});
                    }
                    if mode.load(Ordering::SeqCst) == 2 {
                        value["incoming_delivery"] = json!({"code":"delivery_moved_to_ting","provider":"ting","forwarding":false});
                    }
                    Json(json!({"type":"relay_status","data":value})).into_response()
                }
            }))
            .route("/shutdown", post(move || {
                let stopped = stopped_handler.clone();
                let mode = mode_shutdown.clone();
                async move {
                    stopped.fetch_add(1, Ordering::SeqCst);
                    mode.store(3, Ordering::SeqCst);
                    StatusCode::NO_CONTENT
                }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
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
        assert!(unrelated.to_string().contains("unrecognized service"));
        assert_eq!(stopped.load(Ordering::SeqCst), 0);
        mode.store(1, Ordering::SeqCst);
        assert!(start(&state, &launch, None).await.is_err());
        assert_eq!(stopped.load(Ordering::SeqCst), 1);
        mode.store(2, Ordering::SeqCst);
        assert!(outgoing_only_status(&start(&state, &launch, None).await?));
        assert_eq!(stopped.load(Ordering::SeqCst), 1);
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
                assert_eq!(body, json!({"type":"create_conversation","data":{"participant_ids":["alice","bob"]}}));
                if sends.fetch_add(1, Ordering::SeqCst) == 0 {
                    return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"type":"error","data":{"error":{"code":"retry","message":"fixture retry"}}}))).into_response();
                }
                Json(json!({"type":"conversation","data":{
                    "id":"alice::bob","org_id":"tos","participants":[],"last_message":null,
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
        let legacy = json!({"type":"message.created","data":{"message-id":"000","conversation_id":"alice::bob",
            "message":"retained","sender":{"type":"carbon","id":"alice"},"recipient_id":"bob",
            "metadata":{"source":"dm","delivery_id":Uuid::nil(),"delivery_sequence":1}}});
        let legacy_db = rusqlite::Connection::open(state.directory().join("relay.sqlite3"))?;
        legacy_db.execute_batch("CREATE TABLE inbox(session TEXT,delivery_id TEXT,actor TEXT,sequence INTEGER,frame TEXT,delivered INTEGER NOT NULL DEFAULT 0,attempts INTEGER NOT NULL DEFAULT 0,next_attempt INTEGER NOT NULL DEFAULT 0);")?;
        legacy_db.execute(
            "INSERT INTO inbox(session,delivery_id,actor,sequence,frame) VALUES('default:production',?1,'bob',1,?2)",
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
            Json(json!({"type":"iam","data":{"app_id":"tos>dm","iam_base_url":"http://localhost",
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
