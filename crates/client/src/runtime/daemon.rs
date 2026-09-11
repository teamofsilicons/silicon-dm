use super::{DaemonCommand, queue, store};
use crate::{
    ClientFrame, ServerFrame,
    relay::{Operation, RelayAcknowledgement, RelayRequest},
};
use anyhow::{Context, Result, bail};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use fs2::FileExt;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
    time::Duration,
};
use tokio::{
    sync::{Mutex, OwnedSemaphorePermit, Semaphore, mpsc, watch},
    task::{JoinHandle, JoinSet},
};
use tokio_tungstenite::tungstenite::Message;
use uuid::Uuid;

type Channels = Arc<Mutex<BTreeMap<String, mpsc::Sender<ClientFrame>>>>;
type Statuses = Arc<Mutex<BTreeMap<String, Value>>>;
#[derive(Clone)]
struct App {
    context: RuntimeContext,
    token: String,
    shutdown: watch::Sender<bool>,
    statuses: Statuses,
}

#[derive(Clone)]
struct RuntimeContext {
    store: store::Store,
    queue: queue::Queue,
    payload_budget: Arc<Semaphore>,
}
// Count MiB of encoded queued work, not just tasks. Deserialization and HTTP
// serialization need additional memory. A larger single item takes the entire
// budget so it can progress, but cannot overlap another request or callback.
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

pub async fn start(
    state: &store::Store,
    launch: &DaemonCommand,
    port: Option<u16>,
) -> Result<Value> {
    let config = state.load()?;
    if let Ok(Ok(status)) =
        tokio::time::timeout(Duration::from_secs(1), store::relay(&config)?.status()).await
    {
        return Ok(status);
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
            if let Ok(status) = store::relay(&state.load()?)?.status().await {
                return Ok::<Value, anyhow::Error>(status);
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
    let channels: Channels = Arc::default();
    let statuses: Statuses = Arc::default();
    let app = App {
        context: context.clone(),
        token: config.relay_token.clone(),
        shutdown: shutdown.clone(),
        statuses: statuses.clone(),
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
    let mut supervisor = AbortOnDrop(tokio::spawn(supervise(
        context.clone(),
        channels.clone(),
        statuses,
    )));
    let mut outgoing = AbortOnDrop(tokio::spawn(outbox(context.clone(), channels)));
    let mut callbacks = AbortOnDrop(tokio::spawn(webhooks(context.clone())));
    let server = axum::serve(listener, router).with_graceful_shutdown(async move {
        let _ = shutdown_rx.changed().await;
    });
    eprintln!(
        "DM relay listening at http://dm.localhost:{} (127.0.0.1); durable state in {}",
        config.relay_port,
        context.store.directory().display()
    );
    tokio::select! {result=server=>{result?;},_=tokio::signal::ctrl_c()=>{let _=shutdown.send(true);}}
    supervisor.0.abort();
    outgoing.0.abort();
    callbacks.0.abort();
    let _ = tokio::join!(&mut supervisor.0, &mut outgoing.0, &mut callbacks.0);
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
    match context.queue.stats(){Ok(queues)=>Json(json!({"running":true,"pid":std::process::id(),"version":env!("CARGO_PKG_VERSION"),"profiles":*app.statuses.lock().await,"queues":queues})).into_response(),Err(_)=>error(StatusCode::INTERNAL_SERVER_ERROR,"storage_error","local database unavailable")}
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
async fn supervise(context: RuntimeContext, channels: Channels, statuses: Statuses) {
    let mut jobs: BTreeMap<String, (String, AbortOnDrop)> = BTreeMap::new();
    loop {
        if let Ok(config) = context.store.load() {
            let active: BTreeMap<_, _> = config
                .profiles
                .iter()
                .filter(|(_, p)| p.enabled)
                .map(|(key, p)| {
                    (
                        key.clone(),
                        format!(
                            "{}:{}:{:?}:{:?}",
                            p.base_url, p.tokens.actor.id, p.webhook_url, p.testing_environment_id
                        ),
                    )
                })
                .collect();
            let remove: Vec<_> = jobs
                .iter()
                .filter(|(key, (fingerprint, handle))| {
                    handle.0.is_finished() || active.get(*key) != Some(fingerprint)
                })
                .map(|(key, _)| key.clone())
                .collect();
            for key in remove {
                if let Some((_, job)) = jobs.remove(&key) {
                    job.0.abort()
                }
                channels.lock().await.remove(&key);
                statuses.lock().await.remove(&key);
            }
            for (key, fingerprint) in active {
                if let std::collections::btree_map::Entry::Vacant(entry) = jobs.entry(key.clone()) {
                    let task = AbortOnDrop(tokio::spawn(connection_loop(
                        context.clone(),
                        key.clone(),
                        channels.clone(),
                        statuses.clone(),
                    )));
                    entry.insert((fingerprint, task));
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}
async fn connection_loop(
    context: RuntimeContext,
    key: String,
    channels: Channels,
    statuses: Statuses,
) {
    let mut delay = 1;
    loop {
        statuses
            .lock()
            .await
            .insert(key.clone(), json!({"state":"connecting"}));
        match connected(&context, &key, &channels, &statuses).await {
            Ok(()) => delay = 1,
            Err(error) => {
                let detail = connection_error(&error);
                let requires_login = detail["stage"].as_str() == Some("load_profile")
                    && matches!(detail["status"].as_u64(), Some(401 | 403));
                let mut status = json!({
                    "state": if requires_login { "authentication_required" } else { "reconnecting" },
                    "retry_in_seconds": delay,
                    "last_error": detail,
                });
                if requires_login {
                    status["recovery"] = json!(
                        "Log in again with a fresh IAM short-lived token for this profile; queued work is retained."
                    );
                }
                statuses.lock().await.insert(key.clone(), status);
            }
        }
        channels.lock().await.remove(&key);
        tokio::time::sleep(Duration::from_secs(delay)).await;
        delay = (delay * 2).min(30);
    }
}
async fn connected(
    context: &RuntimeContext,
    key: &str,
    channels: &Channels,
    statuses: &Statuses,
) -> Result<()> {
    let (config, profile) = context
        .store
        .fresh_profile(key)
        .await
        .context("load_profile")?;
    let client = store::client(&config, &profile).context("configure_client")?;
    let known_generation = context.queue.generation(key)?;
    let mut stream_session = queue::stream_session(key, known_generation);
    let socket = client
        .connect_with_generation(
            std::slice::from_ref(&profile.tokens.actor.id),
            &profile.device_id,
            known_generation,
        )
        .await;
    if matches!(
        &socket,
        Err(crate::Error::WebSocket(tokio_tungstenite::tungstenite::Error::Http(response)))
            if response.status() == StatusCode::UNAUTHORIZED
    ) {
        // IAM may revoke access while this independent refresh family remains
        // active. Let the next connection attempt refresh under its profile
        // lock, without expiring a newer login that completed during the dial.
        context
            .store
            .update(|config| {
                if let Some(current) = config.profiles.get_mut(key)
                    && current.tokens.access_token == profile.tokens.access_token
                {
                    current.expires_at = 0;
                }
                Ok(())
            })
            .context("load_profile")?;
    }
    let mut socket = socket.context("open_socket")?;
    let (tx, mut rx) = mpsc::channel::<ClientFrame>(64);
    let refresh_in = profile.expires_at.saturating_sub(store::now() + 45).max(1);
    let expiry = tokio::time::sleep(Duration::from_secs(refresh_in));
    tokio::pin!(expiry);
    let mut timeout = tokio::time::interval(Duration::from_secs(30));
    let mut last_received = tokio::time::Instant::now();
    loop {
        tokio::select! {
            _=&mut expiry=>{socket.close(None).await?;return Ok(())},
            _=timeout.tick()=>{if last_received.elapsed()>Duration::from_secs(120){bail!("server heartbeat expired")}},
            command=rx.recv()=>{if let Some(command)=command{socket.send(Message::Text(serde_json::to_string(&command)?.into())).await?;}},
            incoming=socket.next()=>{
                match incoming.context("WebSocket ended")?? {
                    Message::Text(text)=>{
                        last_received=tokio::time::Instant::now();
                        let frame:ServerFrame=serde_json::from_str(&text).context("decode_frame")?;
                        match &frame {
                            ServerFrame::Ping{ping_id}=>socket.send(Message::Text(serde_json::to_string(&ClientFrame::Pong{ping_id:ping_id.clone()})?.into())).await?,
                            ServerFrame::Ready{protocol_version,actors,testing_generation,..}=>{
                                if *protocol_version!=silicon_dm_protocol::WEBSOCKET_VERSION{bail!("unsupported protocol version")}
                                context.queue.adopt_generation(key,*testing_generation)?;
                                stream_session=queue::stream_session(key,*testing_generation);
                                channels.lock().await.insert(key.to_owned(), tx.clone());
                                for actor in actors {
                                    let after=context.queue.cursor(&stream_session,actor)?;
                                    socket.send(Message::Text(serde_json::to_string(&ClientFrame::Resume{actor_id:actor.clone(),after_sequence:after})?.into())).await?;
                                    if after>0{socket.send(Message::Text(serde_json::to_string(&ClientFrame::Ack{actor_id:actor.clone(),through_sequence:after})?.into())).await?;}
                                }
                                statuses.lock().await.insert(key.to_owned(),json!({"state":"connected","actor_id":profile.tokens.actor.id,"testing_environment_id":profile.testing_environment_id}));
                            },
                            ServerFrame::Error{recoverable:false,..}=>bail!("server closed authorization"),
                            _=>{
                                if let Some((_,actor,sequence))=frame.delivery_position(){
                                    // ACK occurs only after durable inbox + cursor transaction commits.
                                    let through=context.queue.receive(&stream_session,&frame,&text).context("persist_delivery")?;
                                    if through>0{socket.send(Message::Text(serde_json::to_string(&ClientFrame::Ack{actor_id:actor.into(),through_sequence:through})?.into())).await?;}
                                    if through<sequence{socket.send(Message::Text(serde_json::to_string(&ClientFrame::Resume{actor_id:actor.into(),after_sequence:through})?.into())).await?;}
                                }
                            }
                        }
                    },
                    Message::Ping(data)=>socket.send(Message::Pong(data)).await?,
                    Message::Close(_)=>return Ok(()),
                    _=>{}
                }
            }
        }
    }
}
fn connection_error(error: &anyhow::Error) -> Value {
    // Only fixed stage names and error codes enter status responses. Transport
    // error strings can contain URLs; never expose credentials or full frames.
    let stage = error.to_string();
    let stage = match stage.as_str() {
        "load_profile" | "configure_client" | "open_socket" | "decode_frame"
        | "persist_delivery" => stage.as_str(),
        _ => "realtime_connection",
    };
    if let Some(crate::Error::Api { status, code, .. }) = error.downcast_ref::<crate::Error>() {
        return json!({"stage":stage,"code":code,"status":status});
    }
    if let Some(crate::Error::WebSocket(tokio_tungstenite::tungstenite::Error::Http(response))) =
        error.downcast_ref::<crate::Error>()
    {
        return json!({"stage":stage,"code":"websocket_handshake_rejected","status":response.status().as_u16()});
    }
    if let Some(error) = error.downcast_ref::<serde_json::Error>() {
        return json!({"stage":stage,"code":"invalid_json_shape","line":error.line(),"column":error.column()});
    }
    json!({"stage":stage,"code":"connection_failed"})
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
            json!({"code":"transport_error","message":"DM request did not complete; retry preserves its idempotency key"})
        }
    }
}
async fn outbox(context: RuntimeContext, channels: Channels) {
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
                        let channels = channels.clone();
                        let task = workers.spawn(async move {
                            let _permit = permit;
                            execute_request(context, request, channels).await;
                        });
                        running.insert(task.id(), candidate.session);
                    }
                }
            }
        }
    }
}
async fn execute_request(context: RuntimeContext, request: RelayRequest, channels: Channels) {
    let session = store::session_key(&request.profile, request.testing_environment_id);
    if request.testing_environment_id.is_some()
        && request.request.is_mutation()
        && (request.testing_generation.is_none() || !channels.lock().await.contains_key(&session))
    {
        let _ = context.queue.defer_until_ready(
            request.request_id,
            &json!({"code":"awaiting_testing_generation","message":"waiting for the selected sandbox's authenticated realtime generation before writing"}),
        );
        return;
    }
    let (config, profile) = match context.store.fresh_profile(&session).await {
        Ok(p) => p,
        Err(_) => {
            let _ = context.queue.retry(
                request.request_id,
                &json!({"code":"authentication_required","message":"log in again to resume this profile's pending requests"}),
            );
            return;
        }
    };
    if let Operation::SetPresence { activity } = request.request {
        let result = if let Some(sender) = channels.lock().await.get(&session).cloned() {
            sender
                .send(ClientFrame::Presence {
                    actor_id: profile.tokens.actor.id.clone(),
                    activity,
                })
                .await
                .is_ok()
        } else {
            false
        };
        let _ = if result {
            context.queue.finish(
                request.request_id,
                Some(&json!({"queued_to_socket":true,"transient":true})),
                None,
            )
        } else {
            context.queue.finish(
                request.request_id,
                None,
                Some(
                    &json!({"code":"not_connected","message":"presence requires a connected daemon; check dm daemon status"}),
                ),
            )
        };
        return;
    }
    let mut client = match store::client(&config, &profile) {
        Ok(c) => c,
        Err(_) => return,
    };
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
    match request.request.execute(&client).await {
        Ok(value) => {
            let _ = context.queue.finish(request.request_id, Some(&value), None);
        }
        Err(error) => {
            if error.unauthorized() {
                let _ = context.store.update(|c| {
                    if let Some(p) = c.profiles.get_mut(&session) {
                        p.expires_at = 0
                    }
                    Ok(())
                });
            }
            let detail = client_error(&error);
            // Read failures are returned for the caller to retry. An
            // unavailable discovery provider must not hold later sends.
            let _ = if (request.request.is_mutation() && error.retryable()) || error.unauthorized()
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
async fn webhooks(context: RuntimeContext) {
    let http = match reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .build()
    {
        Ok(c) => c,
        Err(_) => return,
    };
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
                let Ok(config) = context.store.load() else { continue; };
                let profiles = config.profiles.iter().filter(|(_, p)| p.enabled && p.webhook_url.is_some())
                    .map(|(key, _)| key.clone()).collect::<Vec<_>>();
                if let Ok(candidates) = context.queue.webhook_candidates(&profiles) {
                    for candidate in candidates {
                        if running.len() >= 16 { break; }
                        if running.values().any(|active| active == &candidate.session) { continue; }
                        let Some(permit) = reserve_payload(&context, candidate.payload_bytes) else { continue; };
                        let Ok(Some(item)) = context.queue.load_webhook(&candidate.session, &candidate.delivery_id) else { continue; };
                        let context = context.clone();
                        let http = http.clone();
                        let task = workers.spawn(async move {
                            let _permit = permit;
                            deliver_webhook(context, item, http).await;
                        });
                        running.insert(task.id(), candidate.session);
                    }
                }
            }
        }
    }
}
#[derive(Serialize)]
struct CallbackPayload<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
    data: CallbackData<'a>,
    // Silicon's native event contract requires a root metadata object. Message
    // metadata remains unmodified inside data; these are transport identifiers.
    metadata: CallbackMetadata<'a>,
}
#[derive(Serialize)]
struct CallbackMetadata<'a> {
    source: &'static str,
    delivery_id: &'a str,
}
#[derive(Serialize)]
struct CallbackData<'a> {
    profile: &'a str,
    testing_environment_id: Option<Uuid>,
    #[serde(flatten)]
    event: &'a Value,
}

/// Only queued v2 frames are upgraded here; live sockets require v3.
fn upgrade_callback_frame(mut frame: Value) -> Value {
    if frame.get("data").is_some() {
        return frame;
    }
    let Some(fields) = frame.as_object_mut() else {
        return frame;
    };
    let mut kind = fields.remove("type").unwrap_or(Value::Null);
    if kind == "message" {
        kind = json!("new_message");
        if let Some(Value::Object(mut message)) = fields.remove("message") {
            if let Some(text) = message.remove("text") {
                message.insert("message".into(), text);
            }
            message.entry("metadata").or_insert_with(|| json!({}));
            fields.extend(message);
        }
    }
    json!({"type":kind,"data":frame})
}
async fn deliver_webhook(
    context: RuntimeContext,
    mut item: queue::WebhookWork,
    http: reqwest::Client,
) {
    let config = match context.store.load() {
        Ok(c) => c,
        Err(_) => return,
    };
    let profile_key = item.session.split('#').next().unwrap_or(&item.session);
    let Some(profile) = config.profiles.get(profile_key).filter(|p| p.enabled) else {
        return;
    };
    let Some(webhook_url) = &profile.webhook_url else {
        return;
    };
    let http = match url::Url::parse(webhook_url) {
        Ok(url)
            if url
                .host_str()
                .is_some_and(|host| host.ends_with(".localhost")) =>
        {
            // Preserve the Host header for Caddy, but never ask DNS or a proxy
            // to route a reserved localhost name outside this machine.
            let address = std::net::SocketAddr::from((
                [127, 0, 0, 1],
                url.port_or_known_default().unwrap_or(80),
            ));
            match reqwest::Client::builder()
                .no_proxy()
                .resolve(url.host_str().unwrap_or("localhost"), address)
                .timeout(Duration::from_secs(120))
                .redirect(reqwest::redirect::Policy::none())
                .build()
            {
                Ok(client) => client,
                Err(_) => return,
            }
        }
        _ => http,
    };
    // This item may have been archived after the worker selected its batch.
    // The transactional completion guard also covers resets during HTTP I/O.
    if !matches!(
        context
            .queue
            .webhook_is_pending(&item.session, &item.delivery_id),
        Ok(true)
    ) {
        return;
    }
    item.frame = upgrade_callback_frame(item.frame);
    let Some(kind) = item.frame.get("type").and_then(Value::as_str) else {
        return;
    };
    let Some(event) = item.frame.get("data") else {
        return;
    };
    let payload = CallbackPayload {
        kind,
        data: CallbackData {
            profile: &profile.name,
            testing_environment_id: profile.testing_environment_id,
            event,
        },
        metadata: CallbackMetadata {
            source: "dm",
            delivery_id: &item.delivery_id,
        },
    };
    let success = match http
        .post(webhook_url)
        .header("Idempotency-Key", &item.delivery_id)
        .json(&payload)
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => {
            callback_acknowledged(response, &item.delivery_id).await
        }
        _ => false,
    };
    // Callback ACK proves this recipient endpoint received the message.
    // Commit callback completion + a retryable Delivered receipt together.
    // Sender copies, receipt events and deletion tombstones need no receipt.
    let receipt = if success {
        delivery_receipt(&item, profile)
    } else {
        None
    };
    let _ = context
        .queue
        .webhook_done(&item.session, &item.delivery_id, success, receipt.as_ref());
}
async fn callback_acknowledged(mut response: reqwest::Response, delivery_id: &str) -> bool {
    // An ACK is two small fields. Do not let an endpoint's accidental event
    // echo or unbounded response become another full message allocation.
    const MAX_ACK_BYTES: usize = 16 * 1024;
    if response
        .content_length()
        .is_some_and(|size| size > MAX_ACK_BYTES as u64)
    {
        return false;
    }
    let mut body = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) if chunk.len() <= MAX_ACK_BYTES.saturating_sub(body.len()) => {
                body.extend_from_slice(&chunk)
            }
            Ok(Some(_)) | Err(_) => return false,
            Ok(None) => break,
        }
    }
    acknowledges_callback(&body, delivery_id)
}

fn acknowledges_callback(body: &[u8], delivery_id: &str) -> bool {
    #[derive(Deserialize)]
    struct Acknowledgement {
        acknowledged: bool,
        delivery_id: String,
    }
    if serde_json::from_slice::<crate::Envelope<Acknowledgement>>(body).is_ok_and(|ack| {
        ack.kind == "ack" && ack.data.acknowledged && ack.data.delivery_id == delivery_id
    }) {
        return true;
    }
    // Silicon acknowledges completion of its event flow with its own event ID.
    // This is acceptance/delivery to the provider, not a completed model reply.
    #[derive(Deserialize)]
    struct SiliconAcknowledgement {
        status: String,
        event_id: Uuid,
    }
    serde_json::from_slice::<SiliconAcknowledgement>(body)
        .is_ok_and(|ack| ack.status == "ok" && !ack.event_id.is_nil())
}
fn delivery_receipt(item: &queue::WebhookWork, profile: &store::Profile) -> Option<RelayRequest> {
    if item.frame.get("type")?.as_str()? != "new_message" {
        return None;
    }
    let message = item.frame.get("data")?;
    if message.pointer("/sender/id")?.as_str()? == profile.tokens.actor.id
        || message.get("deleted_at").is_some_and(|v| !v.is_null())
    {
        return None;
    }
    let conversation_id = message.get("conversation_id")?.as_str()?.parse().ok()?;
    let message_id = message.get("id")?.as_str()?.parse().ok()?;
    let digest =
        blake3::hash(format!("delivered:{}:{}", item.session, item.delivery_id).as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest.as_bytes()[..16]);
    Some(RelayRequest {
        request_id: Uuid::from_bytes(bytes),
        profile: profile.name.clone(),
        testing_environment_id: profile.testing_environment_id,
        testing_generation: item.session.split('#').nth(1).and_then(|n| n.parse().ok()),
        request: Operation::Receipt {
            conversation_id,
            message_id,
            status: crate::ReceiptStatus::Delivered,
            device_id: profile.device_id.clone(),
        },
    })
}

#[cfg(test)]
mod wire_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn queued_v2_delivery_survives_upgrade_and_retries_until_enveloped_ack() -> Result<()> {
        queued_delivery_ack(false).await
    }

    #[tokio::test]
    async fn queued_delivery_accepts_native_silicon_ack() -> Result<()> {
        queued_delivery_ack(true).await
    }

    async fn queued_delivery_ack(silicon: bool) -> Result<()> {
        let attempts = Arc::new(AtomicUsize::new(0));
        let counter = attempts.clone();
        let app = Router::new().route(
            "/events",
            post(move |headers: HeaderMap, Json(body): Json<Value>| {
                let counter = counter.clone();
                async move {
                    assert_eq!(body.as_object().map(serde_json::Map::len), Some(3));
                    assert_eq!(
                        body["metadata"],
                        json!({"source":"dm", "delivery_id":Uuid::nil()})
                    );
                    assert_eq!(body["type"], "new_message");
                    assert_eq!(body["data"]["message"], "hello");
                    assert_eq!(
                        body["data"]["metadata"],
                        json!({"type":"user", "data":{"message":"nested"}})
                    );
                    assert_eq!(body["data"]["profile"], "default");
                    assert_eq!(body["data"]["recipient_id"], "deliberate@cos:tos");
                    assert_eq!(headers["idempotency-key"], Uuid::nil().to_string());
                    let ack = json!({"acknowledged":true,"delivery_id":Uuid::nil()});
                    // A legacy bare ACK must not mark delivery complete.
                    if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                        Json(ack)
                    } else {
                        if silicon {
                            Json(json!({"status":"ok", "event_id":Uuid::new_v4()}))
                        } else {
                            Json(json!({"type":"ack", "data":ack}))
                        }
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let callback = if silicon {
            format!(
                "http://assistant.my-org.localhost:{}/events",
                listener.local_addr()?.port()
            )
        } else {
            format!("http://{}/events", listener.local_addr()?)
        };
        let server = tokio::spawn(async move { axum::serve(listener, app).await });
        let root = std::env::temp_dir().join(format!("dm-envelope-{}", Uuid::new_v4()));
        let store = store::Store::new(&root)?;
        let profile: store::Profile = serde_json::from_value(json!({
            "name":"default","base_url":"http://localhost:8080","webhook_url":callback,
            "device_id":"device","expires_at":0,"testing_environment_id":null,"enabled":true,
            "tokens":{"access_token":"test","refresh_token":"test","token_type":"Bearer","expires_in":3600,
                "scope":"dm","actor":{"type":"silicon","id":"cos:tos"},"organization_id":"tos"}
        }))?;
        store.update(|config| {
            config.profiles.insert("default:production".into(), profile);
            Ok(())
        })?;
        let legacy = json!({
            "type":"message","delivery_id":Uuid::nil(),"actor_id":"cos:tos","delivery_sequence":1,
            "message":{"id":Uuid::nil(),"conversation_id":Uuid::nil(),"sender":{"type":"carbon","id":"alice"},
                "recipient_id":"deliberate@cos:tos","text":"hello","metadata":{"type":"user","data":{"message":"nested"}},
                "sequence":1,"status":"sent","created_at":"2026-09-11T00:00:00Z","version":1}
        });
        let upgraded = upgrade_callback_frame(legacy.clone());
        let frame: ServerFrame = serde_json::from_value(upgraded.clone())?;
        let queue = queue::Queue::open(&store)?;
        assert_eq!(
            queue.receive("default:production", &frame, &legacy.to_string())?,
            1
        );
        drop(queue);
        let queue = queue::Queue::open(&store)?;
        // Replay under v3 must match the immutable identity of the saved v2 frame.
        assert_eq!(
            queue.receive("default:production", &frame, &upgraded.to_string())?,
            1
        );
        let context = RuntimeContext {
            store,
            queue,
            payload_budget: Arc::new(Semaphore::new(128)),
        };
        let work = context
            .queue
            .load_webhook("default:production", &Uuid::nil().to_string())?
            .context("queued delivery")?;
        deliver_webhook(context.clone(), work, reqwest::Client::new()).await;
        assert!(
            context
                .queue
                .webhook_is_pending("default:production", &Uuid::nil().to_string())?
        );
        deliver_webhook(
            context.clone(),
            queue::WebhookWork {
                session: "default:production".into(),
                delivery_id: Uuid::nil().to_string(),
                frame: legacy,
            },
            reqwest::Client::new(),
        )
        .await;
        assert!(
            !context
                .queue
                .webhook_is_pending("default:production", &Uuid::nil().to_string())?
        );
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert_eq!(context.queue.request_candidates()?.len(), 1);
        server.abort();
        drop(context);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn native_silicon_and_dm_acknowledgements_are_explicit() {
        let id = Uuid::new_v4().to_string();
        let accepts =
            |value: Value| acknowledges_callback(&serde_json::to_vec(&value).unwrap(), &id);
        assert!(accepts(json!({"status":"ok", "event_id":Uuid::new_v4()})));
        assert!(accepts(
            json!({"type":"ack", "data":{"acknowledged":true,"delivery_id":id}})
        ));
        for invalid in [
            json!({"status":"ok"}),
            json!({"status":"error", "event_id":Uuid::new_v4()}),
            json!({"status":"ok", "event_id":"not-a-uuid"}),
            json!({"status":"ok", "event_id":Uuid::nil()}),
            json!({"acknowledged":true,"delivery_id":id}),
            json!({"type":"ack", "data":{"acknowledged":false,"delivery_id":id}}),
            json!({"type":"ack", "data":{"acknowledged":true,"delivery_id":"wrong"}}),
        ] {
            assert!(
                !accepts(invalid.clone()),
                "unexpected acceptance: {invalid}"
            );
        }
    }
}
