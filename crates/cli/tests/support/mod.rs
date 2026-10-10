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
impl Home {
    pub fn new(stub: &Stub) -> Home {
        Home {
            dir: tempfile::tempdir().expect("tempdir"),
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
            .env("DM_TELEMETRY_ENABLED", "false")
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
