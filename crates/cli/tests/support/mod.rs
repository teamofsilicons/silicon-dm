//! An in-process stand-in for Silicon Accounts (device flow, short-lived tokens, refresh,
//! revoke) and the DM service (discovery, /me, sync, messages), plus a fresh DM home per test.
#![allow(dead_code)]
use axum::{
    Form, Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{Arc, Mutex},
    time::Instant,
};

pub type Shared = Arc<Mutex<StubState>>;

pub struct StubState {
    pub base: String,
    /// Scripted device-poll answers: pending, slow_down, denied, expired, approved.
    pub device_answers: VecDeque<&'static str>,
    pub device_expires_in: u64,
    pub device_polls: Vec<Instant>,
    /// What DM discovery says it trusts (defaults to this stub).
    pub trusted_accounts_url: Option<String>,
    pub carbon: Value,
    pub silicon: Value,
    pub live_refresh: HashMap<String, Value>,
    pub spent_refresh: HashSet<String>,
    pub live_access: HashMap<String, Value>,
    pub refreshes: usize,
    pub token_forms: Vec<HashMap<String, String>>,
    pub revoke_forms: Vec<HashMap<String, String>>,
    pub me_calls: usize,
    /// Sync pages handed out in order after the reset page.
    pub sync_pages: VecDeque<Value>,
    pub sync_calls: Vec<HashMap<String, String>>,
    pub receipts: Vec<String>,
    /// Speech: whether the service has voices at all (false: a DM service before 0.17).
    pub speech_supported: bool,
    /// Whether the service has a speech provider configured (`enabled` in the voice list).
    pub speech_enabled: bool,
    /// Voices chosen per account uuid.
    pub chosen_voices: HashMap<String, String>,
    /// Every voice route call: method, decoded account, body, Idempotency-Key.
    pub voice_calls: Vec<(String, String, Value, Option<String>)>,
    /// Bodies of `POST /api/v1/speech`.
    pub renders: Vec<Value>,
    /// Queries of message speech requests.
    pub listens: Vec<HashMap<String, String>>,
    /// Transcriptions: content type, query and the bytes sent.
    pub transcriptions: Vec<(String, HashMap<String, String>, Vec<u8>)>,
    /// Bodies of message sends.
    pub sent: Vec<Value>,
    /// Bodies of bulk receipts, with the conversation.
    pub bulk_receipts: Vec<(String, Value)>,
    /// Messages to answer for GET .../messages/{code} instead of the default one.
    pub messages: HashMap<String, Value>,
    /// Headers of every request to the stand-in attachment host (`/files/…`).
    pub attachment_requests: Vec<HeaderMap>,
    counter: u64,
}

fn oauth(error: &str, description: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error": error, "error_description": description})),
    )
        .into_response()
}

impl StubState {
    fn issue(&mut self, account: &Value) -> Value {
        self.counter += 1;
        let access = format!("access-{}", self.counter);
        let refresh = format!("sar_refresh-{}", self.counter);
        self.live_access.insert(access.clone(), account.clone());
        self.live_refresh.insert(refresh.clone(), account.clone());
        json!({"access_token":access,"token_type":"Bearer","expires_in":1800,"refresh_token":refresh,
            "refresh_token_expires_at":"2028-01-01T00:00:00.000Z","scope":"profile",
            "membership_id":format!("dm:{}", account["uuid"].as_str().unwrap_or_default()),"account":account})
    }
}

async fn device_authorize(State(state): State<Shared>, Json(body): Json<Value>) -> Response {
    let state = state.lock().unwrap_or_else(|p| p.into_inner());
    assert_eq!(body["client_id"], "dm");
    Json(json!({"device_code":"sad_device","user_code":"WDJB-MJHT",
        "verification_uri":format!("{}/device", state.base),
        "verification_uri_complete":format!("{}/device?code=WDJB-MJHT", state.base),
        "expires_in":state.device_expires_in,"interval":1}))
    .into_response()
}

async fn token(State(state): State<Shared>, Form(form): Form<HashMap<String, String>>) -> Response {
    let mut state = state.lock().unwrap_or_else(|p| p.into_inner());
    state.token_forms.push(form.clone());
    assert!(
        !form.contains_key("client_secret"),
        "a public client never sends a secret"
    );
    if form.get("client_id").map(String::as_str) != Some("dm") {
        return oauth("invalid_client", "unknown client");
    }
    match form.get("grant_type").map(String::as_str) {
        Some("urn:ietf:params:oauth:grant-type:device_code") => {
            state.device_polls.push(Instant::now());
            match state.device_answers.pop_front().unwrap_or("pending") {
                "slow_down" => oauth("slow_down", "polling too fast"),
                "denied" => oauth("access_denied", "the Carbon declined"),
                "expired" => oauth("expired_token", "the code expired"),
                "approved" => {
                    let carbon = state.carbon.clone();
                    Json(state.issue(&carbon)).into_response()
                }
                _ => oauth("authorization_pending", "not approved yet"),
            }
        }
        Some("urn:silicon:params:oauth:grant-type:slt") => {
            let slt = form.get("slt").cloned().unwrap_or_default();
            match slt.as_str() {
                "slt_ok" | "slt_ok_again" => {
                    let silicon = state.silicon.clone();
                    Json(state.issue(&silicon)).into_response()
                }
                "slt_carbon" => {
                    let carbon = state.carbon.clone();
                    Json(state.issue(&carbon)).into_response()
                }
                "slt_spent" => oauth(
                    "invalid_grant",
                    "The short-lived token was already used; each one works once. Get a new one.",
                ),
                "slt_stale" => oauth(
                    "invalid_grant",
                    "The short-lived token expired at 2026-10-10T10:00:00.000Z (they last 120 seconds); get a new one with `silicon-accounts login --app dm`.",
                ),
                "slt_remind" => oauth(
                    "invalid_grant",
                    "The short-lived token was issued for the app 'remind', not for 'dm'; get one for 'dm' with `silicon-accounts login --app dm`.",
                ),
                _ => oauth(
                    "invalid_grant",
                    "The short-lived token is not known: it is mistyped or was never issued.",
                ),
            }
        }
        Some("refresh_token") => {
            let presented = form.get("refresh_token").cloned().unwrap_or_default();
            if let Some(account) = state.live_refresh.remove(&presented) {
                state.spent_refresh.insert(presented);
                state.refreshes += 1;
                Json(state.issue(&account)).into_response()
            } else {
                oauth(
                    "invalid_grant",
                    "The refresh token was already used, so the whole sign-in was revoked; sign in again.",
                )
            }
        }
        _ => oauth("unsupported_grant_type", "unknown grant"),
    }
}

async fn revoke(
    State(state): State<Shared>,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    let mut state = state.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(token) = form.get("token") {
        state.live_refresh.remove(token);
    }
    state.revoke_forms.push(form);
    Json(json!({})).into_response()
}

fn caller(state: &StubState, headers: &HeaderMap) -> Option<Value> {
    let token = headers
        .get("authorization")?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")?;
    state.live_access.get(token).cloned()
}
fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({"type":"error","data":{"error":{"code":"token_expired","message":"The access token is not valid."}}})),
    )
        .into_response()
}
fn envelope(kind: &str, data: Value) -> Response {
    Json(json!({"type": kind, "data": data})).into_response()
}

async fn discovery(State(state): State<Shared>) -> Response {
    let state = state.lock().unwrap_or_else(|p| p.into_inner());
    let accounts = state
        .trusted_accounts_url
        .clone()
        .unwrap_or_else(|| state.base.clone());
    envelope(
        "accounts",
        json!({"app_id":"dm","accounts_url":accounts,"api_base_url":format!("{}/api/v1", state.base),
        "contract_version":4,"sign_in":{},"proof_scopes":[],"delivery":{"transport":"sync","ting_enabled":false,
        "sync_path":"/api/v1/sync","sync_long_poll_seconds":30,"dm_websocket_supported":false}}),
    )
}

async fn me(State(state): State<Shared>, headers: HeaderMap) -> Response {
    let mut state = state.lock().unwrap_or_else(|p| p.into_inner());
    state.me_calls += 1;
    let Some(account) = caller(&state, &headers) else {
        return unauthorized();
    };
    envelope(
        "me",
        json!({"account":{"type":account["kind"],"id":account["id"],"uuid":account["uuid"],"display_name":account["display_name"]},
        "custodian":account.get("custodian").map(|c| json!({"type":"carbon","id":c["id"],"uuid":c["uuid"]})),
        "custodian_of":[],"scopes":[],"via_app":null,"delivery":{"transport":"sync","ting_enabled":false,
        "sync_path":"/api/v1/sync","sync_long_poll_seconds":30,"dm_websocket_supported":false}}),
    )
}

async fn sync(
    State(shared): State<Shared>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let page = {
        let mut state = shared.lock().unwrap_or_else(|p| p.into_inner());
        if caller(&state, &headers).is_none() {
            return unauthorized();
        }
        state.sync_calls.push(query.clone());
        if query.get("reset").map(String::as_str) == Some("true") {
            return envelope(
                "sync",
                json!({"events":[],"cursor":"cursor-0","has_more":false,"upper_sequence":0}),
            );
        }
        state.sync_pages.pop_front()
    };
    let page = match page {
        Some(page) => page,
        None => {
            // A long-poll with nothing new: hold it briefly, as the service would.
            if query.get("wait").is_some_and(|wait| wait != "0") {
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            }
            json!({"events":[],"cursor":query.get("cursor").cloned().unwrap_or_else(|| "cursor-0".into()),"has_more":false,"upper_sequence":0})
        }
    };
    envelope("sync", page)
}

async fn message(
    State(state): State<Shared>,
    headers: HeaderMap,
    Path((conversation, code)): Path<(String, String)>,
) -> Response {
    let state = state.lock().unwrap_or_else(|p| p.into_inner());
    if caller(&state, &headers).is_none() {
        return unauthorized();
    }
    if code == "404" {
        return (StatusCode::NOT_FOUND, Json(json!({"type":"error","data":{"error":{"code":"not_found","message":"no such message"}}}))).into_response();
    }
    if let Some(found) = state.messages.get(&code) {
        return envelope("message", found.clone());
    }
    envelope(
        "message",
        json!({"message-id":code,"conversation_id":conversation,"recipient_id":null,
        "sender":{"type":"carbon","id":"c:ada","uuid":"Ada"},"message":"hello from ada","attachments":[],
        "voice_transcript":null,"reply":null,"bundle":null,"history":[],"created_at":"2026-10-10T10:00:00Z",
        "updated_at":null,"deleted_at":null,"delivered_at":null,"read_at":null}),
    )
}

async fn receipt(
    State(state): State<Shared>,
    headers: HeaderMap,
    Path((conversation, code)): Path<(String, String)>,
) -> Response {
    let mut state = state.lock().unwrap_or_else(|p| p.into_inner());
    if caller(&state, &headers).is_none() {
        return unauthorized();
    }
    state.receipts.push(format!("{conversation}/{code}"));
    envelope(
        "receipt",
        json!({"message-id":code,"conversation_id":conversation,"recipient_id":null,
        "sender":{"type":"carbon","id":"c:ada","uuid":"Ada"},"message":"hello from ada","attachments":[],
        "voice_transcript":null,"reply":null,"bundle":null,"history":[],"created_at":"2026-10-10T10:00:00Z",
        "updated_at":null,"deleted_at":null,"delivered_at":"2026-10-10T10:00:01Z","read_at":null}),
    )
}

const VOICES: [(&str, &str, &str); 5] = [
    (
        "mark",
        "Mark",
        "Relaxed and natural, like a friend catching you up. Male, American.",
    ),
    (
        "brittney",
        "Brittney",
        "Bright and upbeat. Female, American.",
    ),
    (
        "jamal",
        "Young Jamal",
        "Chill and easygoing. Male, American.",
    ),
    ("jessa", "Jessa", "Warm and casual. Female, American."),
    ("hope", "Hope", "Clear and cheerful. Female, American."),
];

fn api_error(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(json!({"type":"error","data":{"error":{"code":code,"message":message,"hint":"stand-in hint"}}})),
    )
        .into_response()
}

async fn voices(State(state): State<Shared>) -> Response {
    let state = state.lock().unwrap_or_else(|p| p.into_inner());
    if !state.speech_supported {
        return api_error(
            StatusCode::NOT_FOUND,
            "not_found",
            "The resource was not found.",
        );
    }
    envelope(
        "voices",
        json!({"model":"eleven_v4","enabled":state.speech_enabled,"items":VOICES.iter().map(|(key,name,description)|
            json!({"key":key,"name":name,"description":description,"voice_id":format!("id-{key}")})).collect::<Vec<_>>(),
            "default_rule":"stand-in default rule"}),
    )
}

async fn voice(
    State(state): State<Shared>,
    method: axum::http::Method,
    headers: HeaderMap,
    Path(account): Path<String>,
    body: axum::body::Bytes,
) -> Response {
    let mut state = state.lock().unwrap_or_else(|p| p.into_inner());
    let Some(caller) = caller(&state, &headers) else {
        return unauthorized();
    };
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let key = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    state
        .voice_calls
        .push((method.to_string(), account.clone(), body.clone(), key));
    // The caller's own uuid, or another account by id.
    let uuid = if account == caller["uuid"] {
        account.clone()
    } else {
        format!("uuid-of-{account}")
    };
    if method == axum::http::Method::PUT {
        if body["type"] != "voice.set" {
            return api_error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "validation_error",
                "request type does not match this endpoint",
            );
        }
        let wanted = body["data"]["voice"].as_str().unwrap_or_default();
        if !VOICES.iter().any(|(key, _, _)| *key == wanted) {
            return api_error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "voice_not_found",
                "There is no such voice.",
            );
        }
        state.chosen_voices.insert(uuid.clone(), wanted.to_owned());
    } else if method == axum::http::Method::DELETE {
        state.chosen_voices.remove(&uuid);
    }
    let chosen = state.chosen_voices.get(&uuid).cloned();
    let key = chosen.clone().unwrap_or_else(|| "jamal".into());
    let (_, name, description) = VOICES
        .iter()
        .find(|(k, _, _)| *k == key)
        .copied()
        .unwrap_or(VOICES[2]);
    envelope(
        "voice",
        json!({"account":{"type":"silicon","id":account,"uuid":uuid},
            "voice":{"key":key,"name":name,"description":description},
            "chosen":chosen.is_some(),"chosen_by":null,"updated_at":null,"can_change":true}),
    )
}

/// Half a second of a loud 24 kHz mono 16-bit tone that starts at once, as DM's speech does.
pub fn wav(milliseconds: usize) -> Vec<u8> {
    let samples = 24 * milliseconds;
    let mut bytes = Vec::with_capacity(44 + samples * 2);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + samples as u32 * 2).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&24_000_u32.to_le_bytes());
    bytes.extend_from_slice(&48_000_u32.to_le_bytes());
    bytes.extend_from_slice(&2_u16.to_le_bytes());
    bytes.extend_from_slice(&16_u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&(samples as u32 * 2).to_le_bytes());
    for index in 0..samples {
        let sample: i16 = if (index / 55) % 2 == 0 { 8000 } else { -8000 };
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    bytes
}

fn audio(voice: &str, cache: &str) -> Response {
    (
        [
            ("content-type", "audio/wav".to_owned()),
            ("x-dm-voice", voice.to_owned()),
            ("x-dm-duration-ms", "500".to_owned()),
            ("x-dm-cache", cache.to_owned()),
            ("etag", "\"stand-in\"".to_owned()),
        ],
        wav(500),
    )
        .into_response()
}

async fn render(
    State(state): State<Shared>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let mut state = state.lock().unwrap_or_else(|p| p.into_inner());
    if caller(&state, &headers).is_none() {
        return unauthorized();
    }
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    state.renders.push(body.clone());
    if body["type"] != "speech.render" {
        return api_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "validation_error",
            "request type does not match this endpoint",
        );
    }
    audio(body["data"]["voice"].as_str().unwrap_or("jamal"), "miss")
}

async fn listen(
    State(state): State<Shared>,
    headers: HeaderMap,
    Path((_conversation, code)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let mut state = state.lock().unwrap_or_else(|p| p.into_inner());
    if caller(&state, &headers).is_none() {
        return unauthorized();
    }
    state.listens.push(query);
    if code == "0zz" {
        return api_error(
            StatusCode::NOT_FOUND,
            "nothing_to_speak",
            "This message has no words to say.",
        );
    }
    audio("mark", "hit")
}

async fn transcribe(
    State(state): State<Shared>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    body: axum::body::Bytes,
) -> Response {
    let mut state = state.lock().unwrap_or_else(|p| p.into_inner());
    if caller(&state, &headers).is_none() {
        return unauthorized();
    }
    let content_type = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let duration =
        silicon_dm_client::speech::wav_info(&body).map_or(1000, |info| info.duration_milliseconds);
    state
        .transcriptions
        .push((content_type, query, body.to_vec()));
    envelope(
        "transcription",
        json!({"transcript":"hello from the stand-in","language":"en","duration_milliseconds":duration,"confidence":0.9}),
    )
}

async fn send(
    State(state): State<Shared>,
    headers: HeaderMap,
    Path(conversation): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let mut state = state.lock().unwrap_or_else(|p| p.into_inner());
    let Some(caller) = caller(&state, &headers) else {
        return unauthorized();
    };
    state.sent.push(body.clone());
    let data = &body["data"];
    let speech = data.get("speech").filter(|s| !s.is_null()).map(|speech| {
        json!({"voice":speech["voice"].as_str().unwrap_or("jamal"),"voice_name":"Young Jamal",
            "script":speech["script"],"model":"eleven_v4","status":"pending","duration_milliseconds":null,"peaks":null})
    });
    envelope(
        "message.create.successful",
        json!({"message-id":"001","conversation_id":conversation,"recipient_id":null,
            "sender":{"type":caller["kind"],"id":caller["id"],"uuid":caller["uuid"]},
            "message":data["message"],"attachments":data["attachments"],"voice_transcript":null,
            "reply":null,"bundle":null,"history":[],"created_at":"2026-10-17T10:00:00Z","updated_at":null,
            "deleted_at":null,"delivered_at":null,"read_at":null,"speech":speech,"voice":null,"gif":null,
            "attachment_details":[],"metadata":{}}),
    )
}

async fn bulk_receipt(
    State(state): State<Shared>,
    headers: HeaderMap,
    Path(conversation): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let mut state = state.lock().unwrap_or_else(|p| p.into_inner());
    if caller(&state, &headers).is_none() {
        return unauthorized();
    }
    state.bulk_receipts.push((conversation, body.clone()));
    envelope(
        "receipts",
        json!({"updated":3,"unread_count":0,"last_read_message_id":body["data"]["up_to"]}),
    )
}

async fn attachment(
    State(state): State<Shared>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Response {
    let mut state = state.lock().unwrap_or_else(|p| p.into_inner());
    state.attachment_requests.push(headers);
    match name.as_str() {
        "note.ogg" => (
            [("content-type", "audio/ogg")],
            b"OggS\0\x02stand-in ogg audio".to_vec(),
        )
            .into_response(),
        "speech.bin" => ([("content-type", "application/octet-stream")], wav(300)).into_response(),
        "report.pdf" => {
            ([("content-type", "application/pdf")], b"%PDF-1.7".to_vec()).into_response()
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

pub struct Stub {
    pub base: String,
    pub state: Shared,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Stub {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Stub {
    pub async fn start() -> Stub {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let base = format!(
            "http://127.0.0.1:{}",
            listener.local_addr().expect("address").port()
        );
        let state = Arc::new(Mutex::new(StubState {
            base: base.clone(),
            device_answers: VecDeque::new(),
            device_expires_in: 600,
            device_polls: Vec::new(),
            trusted_accounts_url: None,
            carbon: json!({"uuid":"Ca7","kind":"carbon","id":"c:dm-cli-carbon","display_name":"Carbon Tester","pfp_url":"","version":1}),
            silicon: json!({"uuid":"S1l","kind":"silicon","id":"si:dm-cli-silicon","display_name":"","pfp_url":"","version":1,
                "custodian":{"uuid":"Ca7","id":"c:dm-cli-carbon"}}),
            live_refresh: HashMap::new(),
            spent_refresh: HashSet::new(),
            live_access: HashMap::new(),
            refreshes: 0,
            token_forms: Vec::new(),
            revoke_forms: Vec::new(),
            me_calls: 0,
            sync_pages: VecDeque::new(),
            sync_calls: Vec::new(),
            receipts: Vec::new(),
            speech_supported: true,
            speech_enabled: true,
            chosen_voices: HashMap::new(),
            voice_calls: Vec::new(),
            renders: Vec::new(),
            listens: Vec::new(),
            transcriptions: Vec::new(),
            sent: Vec::new(),
            bulk_receipts: Vec::new(),
            messages: HashMap::new(),
            attachment_requests: Vec::new(),
            counter: 0,
        }));
        let app = Router::new()
            .route("/v1/device/authorize", post(device_authorize))
            .route("/v1/oauth/token", post(token))
            .route("/v1/oauth/revoke", post(revoke))
            .route("/api/v1/accounts", get(discovery))
            .route("/api/v1/me", get(me))
            .route("/api/v1/sync", get(sync))
            .route(
                "/api/v1/conversations/{conversation}/messages/{code}",
                get(message),
            )
            .route(
                "/api/v1/conversations/{conversation}/messages/{code}/receipts",
                post(receipt),
            )
            .route("/api/v1/voices", get(voices))
            .route(
                "/api/v1/voices/{account}",
                get(voice).put(voice).delete(voice),
            )
            .route("/api/v1/speech", post(render))
            .route("/api/v1/speech/transcriptions", post(transcribe))
            .route(
                "/api/v1/conversations/{conversation}/messages/{code}/speech",
                get(listen),
            )
            .route("/api/v1/conversations/{conversation}/messages", post(send))
            .route(
                "/api/v1/conversations/{conversation}/receipts",
                post(bulk_receipt),
            )
            .route("/files/{name}", get(attachment))
            .route("/live", get(|| async { StatusCode::NO_CONTENT }))
            .with_state(state.clone());
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Stub {
            base,
            state,
            server,
        }
    }
    pub fn with<T>(&self, f: impl FnOnce(&mut StubState) -> T) -> T {
        f(&mut self.state.lock().unwrap_or_else(|p| p.into_inner()))
    }
}

/// A fresh DM home (and its own relay home) pointed at the stub. Stops its relay on drop.
pub struct Home {
    pub dir: tempfile::TempDir,
    pub base: String,
}
/// The relay ports test homes prefer: this build's CLI lane (4650-4699), never the default
/// 19780, which another DM on this machine may own. A busy port falls back to the next one.
static NEXT_RELAY_PORT: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);

impl Home {
    pub fn new(stub: &Stub) -> Home {
        let dir = tempfile::tempdir().expect("tempdir");
        let offset = NEXT_RELAY_PORT.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % 50;
        let port = 4650 + (u16::try_from(std::process::id() % 50).unwrap_or(0) + offset) % 50;
        std::fs::create_dir_all(dir.path().join("state")).expect("state dir");
        std::fs::write(
            dir.path().join("state").join("config.json"),
            json!({"relay_port": port}).to_string(),
        )
        .expect("config");
        Home {
            dir,
            base: stub.base.clone(),
        }
    }
    pub fn state(&self) -> std::path::PathBuf {
        self.dir.path().join("state")
    }
    pub fn command(&self, args: &[&str]) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_dm"));
        command
            .kill_on_drop(true)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", self.dir.path())
            .env("SILICON_DM_HOME", self.state())
            .env("SILICON_DM_RELAY_HOME", self.dir.path().join("relay"))
            .env("DM_API_URL", &self.base)
            .env("ACCOUNTS_URL", &self.base)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        // Winsock loads its providers beneath SystemRoot even for loopback requests.
        // Keep this OS setting while isolating all application credentials and config.
        #[cfg(windows)]
        command.env(
            "SystemRoot",
            std::env::var_os("SystemRoot").expect("SystemRoot"),
        );
        command
    }
    pub async fn run(&self, args: &[&str]) -> Run {
        Self::finish(self.command(args).spawn().expect("dm starts")).await
    }
    pub async fn run_with_stdin(&self, args: &[&str], input: &str) -> Run {
        use tokio::io::AsyncWriteExt;
        let mut command = self.command(args);
        command.stdin(std::process::Stdio::piped());
        let mut child = command.spawn().expect("dm starts");
        let mut stdin = child.stdin.take().expect("stdin");
        stdin
            .write_all(input.as_bytes())
            .await
            .expect("write stdin");
        drop(stdin);
        Self::finish(child).await
    }
    async fn finish(mut child: tokio::process::Child) -> Run {
        use tokio::io::AsyncReadExt;
        let mut stdout = child.stdout.take().expect("stdout");
        let mut stderr = child.stderr.take().expect("stderr");
        let mut out = Vec::new();
        let mut err = Vec::new();
        let done = tokio::time::timeout(std::time::Duration::from_secs(45), async {
            tokio::try_join!(
                async {
                    let status = child.wait().await?;
                    eprintln!("dm process exited: {status}; waiting for output pipes to close");
                    Ok::<_, std::io::Error>(status)
                },
                stdout.read_to_end(&mut out),
                stderr.read_to_end(&mut err),
            )
        })
        .await
        .expect("dm exits and closes its pipes within 45 seconds")
        .expect("dm runs");
        Run::from(std::process::Output {
            status: done.0,
            stdout: out,
            stderr: err,
        })
    }
    pub fn config(&self) -> Value {
        serde_json::from_slice(&std::fs::read(self.state().join("config.json")).expect("config"))
            .expect("json")
    }
    /// Every byte this home wrote, for "never stored" checks.
    pub fn all_files(&self) -> String {
        fn walk(dir: &std::path::Path, out: &mut String) {
            for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, out);
                } else if let Ok(bytes) = std::fs::read(&path) {
                    out.push_str(&String::from_utf8_lossy(&bytes));
                }
            }
        }
        let mut out = String::new();
        walk(self.dir.path(), &mut out);
        out
    }
}
impl Drop for Home {
    fn drop(&mut self) {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_dm"));
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", self.dir.path())
            .env("SILICON_DM_HOME", self.state())
            .env("SILICON_DM_RELAY_HOME", self.dir.path().join("relay"))
            .args(["daemon", "stop"]);
        #[cfg(windows)]
        command.env(
            "SystemRoot",
            std::env::var_os("SystemRoot").expect("SystemRoot"),
        );
        let _ = command.output();
    }
}

pub struct Run {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}
impl From<std::process::Output> for Run {
    fn from(output: std::process::Output) -> Self {
        Run {
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }
}
impl Run {
    /// The whole of standard output as JSON, else its last line (streams end with the result).
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.stdout)
            .or_else(|_| serde_json::from_str(self.stdout.lines().last().unwrap_or("null")))
            .unwrap_or_else(|_| {
                panic!(
                    "stdout is not JSON: {}\nstderr: {}",
                    self.stdout, self.stderr
                )
            })
    }
    pub fn lines(&self) -> Vec<Value> {
        self.stdout
            .lines()
            .map(|line| {
                serde_json::from_str(line).unwrap_or_else(|_| panic!("not a JSON line: {line}"))
            })
            .collect()
    }
    pub fn error(&self) -> Value {
        serde_json::from_str(&self.stderr)
            .or_else(|_| serde_json::from_str(self.stderr.lines().last().unwrap_or("null")))
            .unwrap_or_else(|_| panic!("stderr has no JSON error: {}", self.stderr))
    }
}
