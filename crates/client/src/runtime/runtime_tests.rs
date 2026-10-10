//! Stored sign-ins: the legacy and damaged files, single-flight refresh, sign-in
//! replacement rules and sign-out, against an in-process stand-in for Silicon Accounts.
use super::*;
use axum::{
    Form, Json, Router, extract::State, http::StatusCode, response::IntoResponse, routing::post,
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

#[derive(Default)]
struct Accounts {
    live: HashMap<String, u32>,
    refreshes: u32,
    revoked: Vec<HashMap<String, String>>,
    issued: u32,
}
type Shared = Arc<Mutex<Accounts>>;

fn tokens(issued: u32, uuid: &str, id: &str) -> serde_json::Value {
    json!({"access_token":format!("access-{issued}"),"token_type":"Bearer","expires_in":1800,
        "refresh_token":format!("sar_refresh-{issued}"),"refresh_token_expires_at":"2028-01-01T00:00:00.000Z",
        "scope":"profile","membership_id":format!("dm:{uuid}"),
        "account":{"uuid":uuid,"kind":"silicon","id":id,"display_name":"","pfp_url":"","version":1}})
}

async fn token(
    State(state): State<Shared>,
    Form(form): Form<HashMap<String, String>>,
) -> axum::response::Response {
    // Slow enough that concurrent refreshes overlap if they are not single-flight.
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    let mut state = state.lock().unwrap_or_else(|p| p.into_inner());
    assert_eq!(form["client_id"], "dm");
    assert!(!form.contains_key("client_secret"));
    let presented = form.get("refresh_token").cloned().unwrap_or_default();
    if state.live.remove(&presented).is_none() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"invalid_grant","error_description":"The refresh token was already used, so the sign-in was revoked."})),
        )
            .into_response();
    }
    state.refreshes += 1;
    state.issued += 1;
    let issued = state.issued;
    state.live.insert(format!("sar_refresh-{issued}"), issued);
    Json(tokens(issued, "S1l", "si:scout")).into_response()
}

async fn revoke(
    State(state): State<Shared>,
    Form(form): Form<HashMap<String, String>>,
) -> axum::response::Response {
    state
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .revoked
        .push(form);
    Json(json!({})).into_response()
}

struct Fixture {
    root: tempfile::TempDir,
    accounts: Shared,
    base: String,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Fixture {
    async fn new() -> Result<Self> {
        let accounts = Shared::default();
        let app = Router::new()
            .route("/v1/oauth/token", post(token))
            .route("/v1/oauth/revoke", post(revoke))
            .with_state(accounts.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("http://127.0.0.1:{}", listener.local_addr()?.port());
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(Self {
            root: tempfile::tempdir()?,
            accounts,
            base,
            server,
        })
    }
    fn store(&self) -> Result<store::Store> {
        store::Store::new(self.root.path().join("home"))?
            .with_relay_home(self.root.path().join("relay"))
    }
    /// A signed-in Silicon whose access token expires at `expires_at`.
    fn sign_in(&self, expires_at: u64) -> Result<()> {
        let refresh = "sar_refresh-0".to_owned();
        self.accounts
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .live
            .insert(refresh.clone(), 0);
        let profile: store::Profile = serde_json::from_value(json!({
            "format":2,"name":"default","base_url":"http://127.0.0.1:9","accounts_url":self.base,"app_id":"dm",
            "device_id":"device","expires_at":expires_at,"enabled":true,
            "account":{"uuid":"S1l","kind":"silicon","id":"si:scout"},
            "tokens":{"access_token":"access-0","refresh_token":refresh,"expires_in":1800}
        }))?;
        self.store()?.update(|config| {
            config.telemetry_enabled = false;
            config
                .profiles
                .insert(store::session_key("default"), profile);
            Ok(())
        })
    }
}

#[test]
fn a_profile_from_before_silicon_accounts_loads_signed_out_and_keeps_its_old_id() -> Result<()> {
    let root = tempfile::tempdir()?;
    let state = store::Store::new(root.path())?;
    std::fs::write(
        root.path().join("config.json"),
        serde_json::to_vec(&json!({"default_profile":"work","profiles":{
            "work:production":{"name":"work","base_url":"https://api.dm.teamofsilicons.com","device_id":"kept",
                "expires_at":4102444800u64,"enabled":true,"tokens":{"access_token":"oat_old","refresh_token":"ort_old",
                "token_type":"Bearer","expires_in":1800,"scope":"dm","actor":{"type":"carbon","id":"c:ada"},"organization_id":"tos"}},
            "work:00000000-0000-0000-0000-000000000001":{"name":"work","base_url":"https://api.dm.teamofsilicons.com",
                "device_id":"sandbox","expires_at":1,"testing_environment_id":"00000000-0000-0000-0000-000000000001",
                "tokens":{"access_token":"oat_t","refresh_token":"ort_t","token_type":"Bearer","expires_in":1,"scope":"dm",
                "actor":{"type":"carbon","id":"c:ada"},"organization_id":"tos"}}},
            "testing_keys":{"00000000-0000-0000-0000-000000000001":{"key":"ask_secret","base_url":"x"}},
            "relay_port":19781,"relay_token":"token","auto_update":false,"telemetry_enabled":true}))?,
    )?;
    let config = state.load()?;
    assert_eq!(config.default_profile, "work");
    let work = &config.profiles["work:production"];
    assert!(!work.signed_in());
    assert!(work.tokens.is_none());
    assert_eq!(work.device_id, "kept");
    assert_eq!(
        work.legacy.as_ref().and_then(|l| l.actor_id.as_deref()),
        Some("c:ada")
    );
    let error = store::profile(&config, "work")
        .err()
        .context("signed out")?;
    assert_eq!(
        error.downcast_ref::<store::SessionError>().map(|e| e.code),
        Some("sign_in_required")
    );
    // Written back in the current format, without the retired secrets.
    state.update(|_| Ok(()))?;
    let written = std::fs::read_to_string(root.path().join("config.json"))?;
    assert!(
        !written.contains("ask_secret")
            && !written.contains("ort_old")
            && !written.contains("organization")
    );
    assert!(written.contains("\"format\": 2"));
    Ok(())
}

#[test]
fn an_unreadable_state_file_is_kept_aside_and_replaced() -> Result<()> {
    let root = tempfile::tempdir()?;
    let state = store::Store::new(root.path())?;
    std::fs::write(root.path().join("config.json"), "{\"profiles\": [")?;
    assert!(store::peek(root.path()).is_err());
    let config = state.load()?;
    assert!(config.profiles.is_empty());
    let aside = std::fs::read_dir(root.path())?
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("config.json.unreadable-")
        })
        .count();
    assert_eq!(aside, 1);
    assert!(store::peek(root.path())?.is_some());
    assert!(store::peek(&root.path().join("missing"))?.is_none());
    Ok(())
}

#[tokio::test]
async fn concurrent_refreshes_rotate_the_token_once_and_store_it_before_use() -> Result<()> {
    let fixture = Fixture::new().await?;
    fixture.sign_in(0)?;
    let key = store::session_key("default");
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let state = fixture.store()?;
        let key = key.clone();
        tasks.push(tokio::spawn(async move { state.fresh_profile(&key).await }));
    }
    let mut access = std::collections::BTreeSet::new();
    for task in tasks {
        let (_, profile) = task.await??;
        access.insert(profile.access_token().unwrap_or_default().to_owned());
    }
    assert_eq!(
        access.into_iter().collect::<Vec<_>>(),
        vec!["access-1".to_owned()]
    );
    assert_eq!(
        fixture
            .accounts
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .refreshes,
        1
    );
    let saved = fixture.store()?.load()?;
    let profile = &saved.profiles[&key];
    assert_eq!(
        profile.tokens.as_ref().map(|t| t.refresh_token.as_str()),
        Some("sar_refresh-1")
    );
    assert!(profile.expires_at > store::now() + 1700);
    Ok(())
}

#[tokio::test]
async fn a_refused_refresh_ends_the_sign_in_and_says_why() -> Result<()> {
    let fixture = Fixture::new().await?;
    fixture.sign_in(0)?;
    fixture
        .accounts
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .live
        .clear();
    let error = fixture
        .store()?
        .fresh_profile(&store::session_key("default"))
        .await
        .err()
        .context("refused")?;
    let reason = error
        .downcast_ref::<store::SessionError>()
        .context("session error")?;
    assert_eq!(reason.code, "sign_in_ended");
    assert!(reason.message.contains("already used"));
    let config = fixture.store()?.load()?;
    let profile = &config.profiles[&store::session_key("default")];
    assert!(profile.tokens.is_none() && !profile.enabled);
    assert_eq!(
        profile.ended.as_ref().map(|e| e.reason.as_str()),
        Some("refresh_refused")
    );
    Ok(())
}

#[tokio::test]
async fn an_unreachable_silicon_accounts_keeps_a_token_that_still_works() -> Result<()> {
    let fixture = Fixture::new().await?;
    for (expires_in, usable) in [(40, true), (5, false)] {
        fixture.sign_in(store::now() + expires_in)?;
        let state = fixture.store()?;
        state.update(|config| {
            if let Some(profile) = config.profiles.get_mut(&store::session_key("default")) {
                profile.accounts_url = "http://127.0.0.1:9".into();
            }
            Ok(())
        })?;
        let result = state.fresh_profile(&store::session_key("default")).await;
        assert_eq!(result.is_ok(), usable, "{expires_in}");
    }
    Ok(())
}

fn session(uuid: &str, id: &str, issued: u32) -> accounts::Session {
    accounts::Session {
        access_token: format!("access-{issued}"),
        refresh_token: format!("sar_refresh-{issued}"),
        expires_in: 1800,
        expires_at: store::now() + 1800,
        refresh_expires_at: None,
        scope: None,
        account: Some(accounts::SessionAccount {
            uuid: uuid.into(),
            kind: crate::ActorType::Silicon,
            id: id.into(),
            display_name: None,
            pfp_url: None,
            custodian: None,
        }),
    }
}

#[tokio::test]
async fn another_account_never_takes_over_a_profile_with_queued_requests() -> Result<()> {
    let fixture = Fixture::new().await?;
    fixture.sign_in(store::now() + 1800)?;
    let runtime = LocalRuntime {
        store: fixture.store()?,
    };
    let target = SignInTarget::new("default", "http://127.0.0.1:9", &fixture.base, "dm")?;
    let launch = DaemonCommand {
        executable: fixture.root.path().join("no-relay"),
        arguments: vec![],
    };
    let queued = crate::relay::RelayRequest::new(
        "default",
        crate::relay::Operation::CreateConversation {
            participant_ids: vec!["c:ada".into()],
            idempotency_key: "queued-key".into(),
        },
    );
    queue::Queue::open(runtime.store())?.enqueue(&queued, &serde_json::to_value(&queued)?)?;
    let refused = runtime
        .save_sign_in(&target, &session("Oth", "si:other", 7), "slt", &launch)
        .await
        .err()
        .context("refused")?;
    assert_eq!(
        refused
            .downcast_ref::<store::SessionError>()
            .map(|e| e.code),
        Some("profile_in_use")
    );
    // The same account signs in again: replaced, and the replaced sign-in is revoked.
    let result = runtime
        .save_sign_in(&target, &session("S1l", "si:scout", 8), "slt", &launch)
        .await?;
    assert_eq!(result["uuid"], "S1l");
    assert_eq!(result["replaced_sign_in_revoked"], true);
    assert_eq!(result["queue"]["pending"], 1);
    assert_eq!(result["relay"]["running"], false);
    let revoked = fixture
        .accounts
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .revoked
        .clone();
    assert_eq!(revoked[0]["token"], "sar_refresh-0");
    // Sign-out revokes at Silicon Accounts and keeps the queue.
    let out = runtime.logout("default").await?;
    assert_eq!(
        (out["signed_out"].clone(), out["revoked"].clone()),
        (json!(true), json!(true))
    );
    assert_eq!(out["pending_requests_retained"], 1);
    let revoked = fixture
        .accounts
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .revoked
        .clone();
    assert_eq!(revoked[1]["token"], "sar_refresh-8");
    assert_eq!(revoked[1]["token_type_hint"], "refresh_token");
    assert_eq!(revoked[1]["client_id"], "dm");
    assert_eq!(runtime.logout("default").await?["signed_out"], false);
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn the_sign_in_file_and_its_directory_are_owner_only_and_replaced_atomically() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new().await?;
    fixture.sign_in(0)?;
    let state = fixture.store()?;
    state.fresh_profile(&store::session_key("default")).await?;
    let directory = state.directory();
    let mode = |path: &std::path::Path| -> Result<u32> {
        Ok(std::fs::metadata(path)?.permissions().mode() & 0o777)
    };
    assert_eq!(mode(&directory)?, 0o700);
    assert_eq!(mode(&directory.join("config.json"))?, 0o600);
    assert_eq!(mode(&directory.join("config.lock"))?, 0o600);
    let lock = std::fs::read_dir(&directory)?
        .flatten()
        .find(|e| e.file_name().to_string_lossy().starts_with("refresh-"))
        .context("refresh lock")?;
    assert_eq!(mode(&lock.path())?, 0o600);
    // No temporary file is left behind by the atomic replace.
    assert!(
        std::fs::read_dir(&directory)?
            .flatten()
            .all(|e| !e.file_name().to_string_lossy().ends_with(".tmp"))
    );
    // A symbolic link in place of the file is refused.
    let target = fixture.root.path().join("elsewhere.json");
    std::fs::write(&target, "{}")?;
    std::fs::remove_file(directory.join("config.json"))?;
    std::os::unix::fs::symlink(&target, directory.join("config.json"))?;
    assert!(state.load().is_err());
    Ok(())
}
