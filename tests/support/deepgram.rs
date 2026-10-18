//! A fake Deepgram for tests: the Voice Agent WebSocket protocol (`Welcome`, `Settings` →
//! `SettingsApplied`, `InjectAgentMessage` → `ConversationText` + PCM + `AgentAudioDone`,
//! `KeepAlive`) with its failure modes, and `POST /v1/listen`. It records what DM sent.
#![allow(
    dead_code,
    reason = "each integration test binary uses a different subset"
)]

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    Json, Router,
    body::Bytes,
    extract::{
        DefaultBodyLimit, Query, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{any, post},
};
use serde_json::{Value, json};

/// The key DM is configured with in tests.
pub const DEEPGRAM_TEST_KEY: &str = "dg-test-key";
/// Milliseconds of audio the fake renders per spoken word.
pub const MILLISECONDS_PER_WORD: u64 = 100;

/// How the fake Voice Agent behaves for the next sessions.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AgentMode {
    /// Speaks every injected message.
    #[default]
    Speak,
    /// `Error` `FAILED_TO_SPEAK` after the first injection.
    FailToSpeak,
    /// `Error` `INVALID_SETTINGS` instead of `SettingsApplied`.
    InvalidSettings,
    /// Closes the socket right after the settings arrive.
    CloseEarly,
    /// `Error` with an unknown code after the first injection.
    ProviderError,
    /// Never sends `Welcome`.
    Silent,
}

/// How the fake `/v1/listen` answers.
#[derive(Clone, Debug, Default)]
pub enum ListenMode {
    /// 200 with a transcript naming how many bytes arrived.
    #[default]
    Hear,
    /// 200 with an empty transcript.
    Empty,
    /// 400 with Deepgram's error body.
    Unreadable,
    /// 503.
    Unavailable,
}

/// One Voice Agent session as the fake saw it.
#[derive(Clone, Debug, Default)]
pub struct AgentSession {
    /// The `Authorization` header of the upgrade.
    pub authorization: Option<String>,
    /// The `Settings` message.
    pub settings: Option<Value>,
    /// Every injected message, in order.
    pub injections: Vec<String>,
    /// How many `KeepAlive` messages arrived.
    pub keepalives: usize,
}

/// One `/v1/listen` request as the fake saw it.
#[derive(Clone, Debug, Default)]
pub struct ListenRequest {
    /// Query parameters.
    pub query: HashMap<String, String>,
    /// `Content-Type`.
    pub content_type: Option<String>,
    /// `Authorization`.
    pub authorization: Option<String>,
    /// Bytes received.
    pub bytes: usize,
}

#[derive(Default)]
struct Recorded {
    agent_mode: AgentMode,
    listen_mode: ListenMode,
    /// Sessions that answer with `ProviderError` before the mode applies again.
    failures_left: usize,
    sessions: Vec<AgentSession>,
    listens: Vec<ListenRequest>,
}

/// The fake, listening on 127.0.0.1.
#[derive(Clone)]
pub struct FakeDeepgram {
    /// `http://127.0.0.1:port` (`DM_DEEPGRAM_URL`).
    pub base_url: String,
    /// `ws://127.0.0.1:port/v1/agent/converse` (`DM_DEEPGRAM_AGENT_URL`).
    pub agent_url: String,
    recorded: Arc<Mutex<Recorded>>,
}

impl FakeDeepgram {
    pub async fn start() -> Self {
        let recorded = Arc::new(Mutex::new(Recorded::default()));
        let router = Router::new()
            .route("/v1/agent/converse", any(converse))
            .route(
                "/v1/listen",
                post(listen).layer(DefaultBodyLimit::disable()),
            )
            .with_state(recorded.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap_or_else(|error| panic!("fake Deepgram bind: {error}"));
        let address = listener
            .local_addr()
            .unwrap_or_else(|error| panic!("fake Deepgram address: {error}"));
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        Self {
            base_url: format!("http://{address}"),
            agent_url: format!("ws://{address}/v1/agent/converse"),
            recorded,
        }
    }

    fn with<T>(&self, action: impl FnOnce(&mut Recorded) -> T) -> T {
        let mut recorded = self
            .recorded
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        action(&mut recorded)
    }

    pub fn set_agent_mode(&self, mode: AgentMode) {
        self.with(|recorded| recorded.agent_mode = mode);
    }

    /// The next `count` sessions fail with a provider error, then the mode applies again.
    pub fn fail_next(&self, count: usize) {
        self.with(|recorded| recorded.failures_left = count);
    }

    pub fn set_listen_mode(&self, mode: ListenMode) {
        self.with(|recorded| recorded.listen_mode = mode);
    }

    pub fn sessions(&self) -> Vec<AgentSession> {
        self.with(|recorded| recorded.sessions.clone())
    }

    pub fn listens(&self) -> Vec<ListenRequest> {
        self.with(|recorded| recorded.listens.clone())
    }
}

async fn converse(
    State(recorded): State<Arc<Mutex<Recorded>>>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    let authorization = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    upgrade.on_upgrade(move |socket| agent(socket, recorded, authorization))
}

fn lock(recorded: &Mutex<Recorded>) -> std::sync::MutexGuard<'_, Recorded> {
    recorded
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// PCM for an injected message: a tone per spoken word, louder for later words so the
/// waveform is not flat.
fn pcm_for(message: &str) -> Vec<u8> {
    let words = silicon_dm::domain::speech::spoken_words(message)
        .split_whitespace()
        .count()
        .max(1);
    let samples_per_word = usize::try_from(24 * MILLISECONDS_PER_WORD).unwrap_or(2_400);
    let mut pcm = Vec::with_capacity(words * samples_per_word * 2);
    for word in 0..words {
        let amplitude = i16::try_from(1_000 + (word % 8) * 1_000).unwrap_or(1_000);
        for sample in 0..samples_per_word {
            let value = if sample % 2 == 0 {
                amplitude
            } else {
                -amplitude
            };
            pcm.extend_from_slice(&value.to_le_bytes());
        }
    }
    pcm
}

async fn send(socket: &mut WebSocket, value: Value) -> bool {
    socket
        .send(Message::Text(value.to_string().into()))
        .await
        .is_ok()
}

#[allow(
    clippy::too_many_lines,
    reason = "one fake Voice Agent session, every mode visible together"
)]
async fn agent(
    mut socket: WebSocket,
    recorded: Arc<Mutex<Recorded>>,
    authorization: Option<String>,
) {
    let (mode, index) = {
        let mut state = lock(&recorded);
        let mode = if state.failures_left > 0 {
            state.failures_left -= 1;
            AgentMode::ProviderError
        } else {
            state.agent_mode
        };
        state.sessions.push(AgentSession {
            authorization,
            ..AgentSession::default()
        });
        (mode, state.sessions.len() - 1)
    };
    if mode == AgentMode::Silent {
        tokio::time::sleep(Duration::from_secs(60)).await;
        return;
    }
    if !send(
        &mut socket,
        json!({"type": "Welcome", "request_id": "fake"}),
    )
    .await
    {
        return;
    }
    while let Some(Ok(message)) = socket.recv().await {
        let Message::Text(text) = message else {
            continue;
        };
        let Ok(event) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        match event["type"].as_str() {
            Some("Settings") => {
                lock(&recorded).sessions[index].settings = Some(event.clone());
                match mode {
                    AgentMode::InvalidSettings => {
                        send(
                            &mut socket,
                            json!({"type": "Error", "code": "INVALID_SETTINGS",
                            "description": "unknown voice"}),
                        )
                        .await;
                        return;
                    }
                    AgentMode::CloseEarly => {
                        let _ = socket.send(Message::Close(None)).await;
                        return;
                    }
                    _ => {
                        send(&mut socket, json!({"type": "SettingsApplied"})).await;
                    }
                }
            }
            Some("KeepAlive") => lock(&recorded).sessions[index].keepalives += 1,
            Some("InjectAgentMessage") => {
                let message = event["message"].as_str().unwrap_or_default().to_owned();
                lock(&recorded).sessions[index]
                    .injections
                    .push(message.clone());
                match mode {
                    AgentMode::FailToSpeak => {
                        send(
                            &mut socket,
                            json!({"type": "Error", "code": "FAILED_TO_SPEAK",
                            "description": "the voice could not speak"}),
                        )
                        .await;
                        return;
                    }
                    AgentMode::ProviderError => {
                        send(
                            &mut socket,
                            json!({"type": "Error", "code": "UNKNOWN",
                            "description": "upstream trouble"}),
                        )
                        .await;
                        return;
                    }
                    _ => {}
                }
                send(
                    &mut socket,
                    json!({"type": "ConversationText", "role": "assistant",
                    "content": message}),
                )
                .await;
                let pcm = pcm_for(&message);
                for frame in pcm.chunks(9_600) {
                    if socket
                        .send(Message::Binary(Bytes::copy_from_slice(frame)))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                send(&mut socket, json!({"type": "AgentAudioDone"})).await;
            }
            _ => {}
        }
    }
}

async fn listen(
    State(recorded): State<Arc<Mutex<Recorded>>>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    };
    let mode = {
        let mut state = lock(&recorded);
        state.listens.push(ListenRequest {
            query: query.clone(),
            content_type: header("content-type"),
            authorization: header("authorization"),
            bytes: body.len(),
        });
        state.listen_mode.clone()
    };
    let language = query.get("language").cloned();
    let detected =
        (query.get("detect_language").map(String::as_str) == Some("true")).then(|| "en".to_owned());
    let transcript = match mode {
        ListenMode::Hear => format!("I heard {} bytes.", body.len()),
        ListenMode::Empty => String::new(),
        ListenMode::Unreadable => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"err_code": "Bad Request",
                    "err_msg": "Bad Request: failed to process audio: corrupt or unsupported data",
                    "request_id": "fake"})),
            )
                .into_response();
        }
        ListenMode::Unavailable => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    #[allow(clippy::cast_precision_loss, reason = "test bodies are small")]
    let duration = body.len() as f64 / 32_000.0;
    let mut channel = json!({"alternatives": [{"transcript": transcript, "confidence": 0.98}]});
    if language.is_none()
        && let Some(detected) = detected
    {
        channel["detected_language"] = json!(detected);
    }
    Json(json!({
        "metadata": {"duration": duration, "request_id": "fake"},
        "results": {"channels": [channel]},
    }))
    .into_response()
}
