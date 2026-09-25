//! Optional durable outgoing command relay and explicit Ting delivery setup.
//!
//! The default `Client` remains stateless. Enable the `runtime` feature to host
//! the same outgoing relay used by the CLI or launch `dm-relay`. One relay
//! process serves every DM home of an operating-system user. Ting's installed
//! system daemon exclusively owns incoming sockets, callbacks, queues and ACKs.
mod daemon;
mod host;
mod queue;
pub mod store;
mod ting_delivery;
pub mod updates;
pub use ting_delivery::{DeliveryAttachOptions, DeliveryLoginOptions, DeliveryTestCredentials};

use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::path::PathBuf;
use url::Url;
use uuid::Uuid;

fn validate_callback_endpoint(url: &Url) -> Result<()> {
    // Silicon uses subdomains of the reserved localhost namespace. This
    // exception is for local callbacks, not DM backend endpoint validation.
    let mut checked = url.clone();
    if url
        .host_str()
        .is_some_and(|host| host.ends_with(".localhost"))
    {
        checked.set_host(Some("localhost"))?;
    }
    crate::validate_endpoint(&checked)?;
    Ok(())
}

/// Executable and arguments used for an independently running relay process.
#[derive(Clone, Debug)]
pub struct DaemonCommand {
    pub executable: PathBuf,
    pub arguments: Vec<String>,
}
impl Default for DaemonCommand {
    fn default() -> Self {
        Self {
            executable: "dm-relay".into(),
            arguments: vec!["run".into()],
        }
    }
}

/// A DM login's local mapping. Incoming delivery is configured separately with Ting.
pub struct LoginOptions<'a> {
    pub profile: &'a str,
    pub base_url: &'a str,
    pub short_lived_token: &'a str,
    /// Retired: any supplied URL fails before the SLT is exchanged. Use
    /// `delivery_login` and `delivery_attach` for an incoming destination.
    pub webhook_url: Option<&'a Url>,
    pub testing_environment_id: Option<Uuid>,
    pub idempotency_key: &'a str,
}

/// Optional stateful component, separate from the stateless protocol client.
#[derive(Clone)]
pub struct LocalRuntime {
    store: store::Store,
}
impl LocalRuntime {
    /// Opens a private, caller-owned directory. Directories keep independent
    /// profiles and queues but share this user's one outgoing relay process.
    pub fn new(directory: impl Into<PathBuf>) -> Result<Self> {
        Ok(Self {
            store: store::Store::new(directory)?,
        })
    }
    /// Shares a relay only with homes using the same caller-owned directory,
    /// instead of this user's default; tests and isolated embedders use it.
    pub fn with_relay_home(self, directory: impl Into<PathBuf>) -> Result<Self> {
        Ok(Self {
            store: self.store.with_relay_home(directory)?,
        })
    }
    /// Uses the explicit DM state override, configured home, or SILICON_HOME/HOME.
    pub fn from_environment() -> Result<Self> {
        Ok(Self {
            store: store::Store::from_environment()?,
        })
    }
    /// Caller-controlled profile and testing-key storage.
    pub fn store(&self) -> &store::Store {
        &self.store
    }
    /// Selects and remembers a sandbox from its IAM test app secret, without an IAM root key.
    pub async fn select_testing_application(
        &self,
        base_url: &str,
        secret: &str,
    ) -> Result<crate::IamInfo> {
        let info = crate::Client::new(base_url)?
            .with_test_key(secret)?
            .iam()
            .await?;
        let id = info.testing_environment_id.context(
            "DM did not validate a testing environment; no production fallback is allowed",
        )?;
        let name = info
            .testing_environment
            .as_ref()
            .and_then(|v| v["name"].as_str())
            .unwrap_or("Testing environment")
            .to_owned();
        self.store.update(|config| {
            config.testing_keys.insert(
                id,
                store::TestKey {
                    key: secret.into(),
                    base_url: base_url.into(),
                },
            );
            config.testing_names.insert(id, name);
            Ok(())
        })?;
        Ok(info)
    }

    /// Persist the diagnostic opt-in for CLI and daemon requests.
    pub fn set_telemetry(&self, enabled: bool) -> Result<()> {
        self.store.update(|config| {
            config.telemetry_enabled = enabled;
            Ok(())
        })
    }
    /// Best-effort, bounded diagnostics using the selected profile's own credentials.
    pub async fn record_diagnostic(
        &self,
        profile: &str,
        testing: Option<Uuid>,
        source: &'static str,
        event: &str,
        success: bool,
        duration_ms: u64,
    ) {
        let Ok(config) = self.store.load() else {
            return;
        };
        let Ok(profile) = store::profile(&config, profile, testing) else {
            return;
        };
        if !config.telemetry_enabled
            || std::env::var("DM_TELEMETRY_ENABLED").as_deref() == Ok("false")
        {
            return;
        }
        // Spool locally; the relay uploads it. A command never waits on the
        // network for its own diagnostic.
        if let Ok(queue) = queue::Queue::open(&self.store) {
            let _ = queue.record_diagnostic(
                &store::session_key(&profile.name, testing),
                source,
                event,
                success,
                duration_ms,
            );
        }
    }
    /// Durably queues `request` and returns without waiting for the backend.
    ///
    /// When the shared relay is running and serves this home, the request is
    /// handed to it over loopback and starts immediately. Otherwise it is
    /// written straight into this home's durable queue and `starter` is
    /// launched detached to attach the home (starting the relay if needed);
    /// the relay resumes the queue as soon as it attaches. Either way the
    /// request survives crashes and is sent exactly once per idempotency key.
    ///
    /// `starter` must attach this home to the shared relay and exit, as
    /// `dm daemon start` does. An error means nothing was queued; the caller
    /// may fall back to [`LocalRuntime::start_with`] and submit normally.
    pub async fn submit_or_queue(
        &self,
        request: &crate::relay::RelayRequest,
        starter: &DaemonCommand,
    ) -> Result<crate::relay::RelayAcknowledgement> {
        let config = self.store.load()?;
        if daemon::serving(&config).await
            && let Ok(Ok(ack)) = tokio::time::timeout(
                std::time::Duration::from_millis(500),
                store::relay(&config)?.submit(request),
            )
            .await
        {
            return Ok(ack);
        }
        queue::Queue::open(&self.store)?.enqueue(request, &serde_json::to_value(request)?)?;
        let mut command = std::process::Command::new(&starter.executable);
        command
            .args(&starter.arguments)
            .env("SILICON_DM_HOME", self.store.directory())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        if let Ok(relay_home) = self.store.relay_home() {
            command.env("SILICON_DM_RELAY_HOME", relay_home);
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // Detach so the caller can exit (and its pipes close) at once.
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
        // The request is already durable; if the starter cannot launch, the
        // next DM command that starts the relay sends it.
        let _ = command.spawn();
        Ok(crate::relay::RelayAcknowledgement {
            acknowledged: true,
            request_id: request.request_id,
            request: serde_json::to_value(crate::Envelope::new("request", request))?,
        })
    }
    /// An authenticated transport to this runtime's local HTTP relay.
    pub fn client(&self) -> Result<crate::relay::RelayClient> {
        store::relay(&self.store.load()?)
    }
    /// Runs the shared relay for this home and every registered home until local
    /// shutdown or Ctrl-C. It refuses to run when this user's relay already
    /// runs. Dropping this future cancels its owned workers and connections;
    /// queues remain on disk.
    pub async fn run(&self) -> Result<()> {
        daemon::run(self.store.clone()).await
    }
    /// Attaches this home to the running shared relay, or starts the packaged
    /// `dm-relay` executable from PATH when none runs. A busy port falls back
    /// to the next free one, which is saved as this home's `relay_port`.
    pub async fn start(&self, port: Option<u16>) -> Result<Value> {
        self.start_with(&DaemonCommand::default(), port).await
    }
    /// Starts a caller-selected executable implementing the relay entry point.
    pub async fn start_with(&self, launch: &DaemonCommand, port: Option<u16>) -> Result<Value> {
        daemon::start(&self.store, launch, port).await
    }
    /// Exchanges an SLT, durably stores the local mapping, and starts the relay.
    /// If launch fails the profile remains saved so starting can be retried
    /// without consuming a new SLT. A profile cannot switch actor or backend.
    pub async fn login(&self, options: &LoginOptions<'_>, launch: &DaemonCommand) -> Result<Value> {
        if options.profile.is_empty()
            || options.profile.len() > 64
            || !options
                .profile
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        {
            bail!("profile names must use 1-64 letters, digits, underscores or hyphens");
        }
        if options.webhook_url.is_some() {
            bail!(
                "incoming delivery moved to Ting: log in to DM without webhook_url, then call delivery_login and delivery_attach; explicitly register DM delivery consent separately"
            );
        }
        let session = store::session_key(options.profile, options.testing_environment_id);
        let lock = self.store.profile_lock(&session).await?;
        let config = self.store.load()?;
        let mut client = crate::Client::new(options.base_url)?;
        if let Some(id) = options.testing_environment_id {
            let key = config
                .testing_keys
                .get(&id)
                .ok_or_else(|| anyhow::anyhow!("import the testing key before login"))?;
            if key.base_url.trim_end_matches('/') != options.base_url.trim_end_matches('/') {
                bail!("testing key belongs to another backend");
            }
            client = client.with_test_key(&key.key)?;
        }
        let tokens = client
            .login(options.short_lived_token, options.idempotency_key)
            .await?;
        self.store.update(|config| {
            if let Some(previous) = config.profiles.get(&session)
                && (previous.tokens.actor != tokens.actor
                    || previous.tokens.organization_id != tokens.organization_id
                    || previous.base_url.trim_end_matches('/') != options.base_url.trim_end_matches('/')) {
                bail!("profile belongs to another actor, organization or backend; choose a new profile");
            }
            let device_id = config.profiles.get(&session).map_or_else(|| Uuid::new_v4().to_string(), |p| p.device_id.clone());
            config.profiles.insert(session.clone(), store::Profile {
                name: options.profile.into(), base_url: options.base_url.into(), tokens: tokens.clone(),
                webhook_url: config.profiles.get(&session).and_then(|p| p.webhook_url.clone()), device_id,
                expires_at: store::now().saturating_add(tokens.expires_in.max(0) as u64),
                refresh_started_at: None,
                testing_environment_id: options.testing_environment_id, enabled: true,
            });
            Ok(())
        })?;
        drop(lock);
        let status = self.start_with(launch, None).await?;
        Ok(
            serde_json::json!({"login":{"authenticated":true,"profile":options.profile,"actor":tokens.actor,"organization_id":tokens.organization_id,"testing_environment_id":options.testing_environment_id,"webhook_url":options.webhook_url},"daemon":status}),
        )
    }
    /// Verifies the selected saved session with DM, refreshing it when needed.
    /// Missing or revoked credentials return authenticated:false; network errors remain errors.
    pub async fn login_status(&self, name: &str, test: Option<Uuid>) -> Result<Value> {
        let config = self.store.load()?;
        if store::profile(&config, name, test).is_err() {
            return Ok(
                serde_json::json!({"authenticated":false,"profile":name,"testing_environment_id":test}),
            );
        }
        let key = store::session_key(name, test);
        let result = async {
            for attempt in 0..2 {
                let (config, profile) = self.store.fresh_profile(&key).await?;
                match store::client(&config, &profile)?.me().await {
                    Err(crate::Error::Api { status: 401, .. }) if attempt == 0 => {
                        // Access can be invalidated before its advertised expiry. Only
                        // invalidate this generation; a concurrent renewal wins.
                        self.store.update(|config| {
                            if let Some(current) = config.profiles.get_mut(&key)
                                && current.tokens.access_token == profile.tokens.access_token
                            {
                                current.expires_at = 0;
                            }
                            Ok(())
                        })?;
                    }
                    result => {
                        let identity = result?;
                        return Ok::<_, anyhow::Error>(serde_json::json!({"authenticated":true,"profile":name,"testing_environment_id":test,
                            "actor":identity.actor,"organization_id":identity.organization_id,"reconsent_required":identity.reconsent_required,"identity":identity,"webhook_url":profile.webhook_url}));
                    }
                }
            }
            unreachable!("the second identity check returns directly")
        }.await;
        match result {
            Err(error)
                if matches!(
                    error.downcast_ref::<crate::Error>(),
                    Some(crate::Error::Api { status: 401, .. })
                ) =>
            {
                Ok(
                    serde_json::json!({"authenticated":false,"profile":name,"testing_environment_id":test}),
                )
            }
            other => other,
        }
    }
    /// Sets the legacy outgoing command-response callback secret only.
    pub fn webhook_secret(
        &self,
        name: &str,
        test: Option<Uuid>,
        secret: Option<&str>,
    ) -> Result<()> {
        if secret.is_some_and(|s| {
            s.is_empty() || s.len() > 8192 || s.bytes().any(|b| b.is_ascii_control())
        }) {
            bail!("invalid callback secret");
        }
        self.store.update(|config| {
            let key = store::session_key(name, test);
            store::profile(config, name, test)?;
            if let Some(secret) = secret {
                config.webhook_secrets.insert(key, secret.into());
            } else {
                config.webhook_secrets.remove(&key);
            }
            Ok(())
        })
    }
    /// Incoming webhooks require the asynchronous Ting attachment workflow.
    pub fn webhook(&self, _name: &str, _test: Option<Uuid>, _url: Option<&Url>) -> Result<Value> {
        bail!(
            "incoming delivery moved to Ting: use delivery_login and delivery_attach, or delivery_unhook; the local endpoint must accept Ting batches for all applications"
        )
    }
    /// Revokes the selected family and disables its local mapping, retaining
    /// queued work. Serializes with login and refresh for the same profile.
    pub async fn logout(
        &self,
        name: &str,
        testing_environment_id: Option<Uuid>,
        idempotency_key: &str,
    ) -> Result<Value> {
        let session = store::session_key(name, testing_environment_id);
        let _lock = self.store.profile_lock(&session).await?;
        let config = self.store.load()?;
        let profile = store::profile(&config, name, testing_environment_id)?;
        store::client(&config, profile)?
            .logout(&profile.tokens.refresh_token, idempotency_key)
            .await?;
        let newer_login_retained = self.store.update(|config| {
            if let Some(current) = config.profiles.get_mut(&session) {
                // Also defend against a caller changing the public store
                // directly while the remote revocation was in flight.
                if current.tokens.refresh_token != profile.tokens.refresh_token {
                    return Ok(true);
                }
                current.enabled = false;
                current.tokens.access_token.clear();
                current.tokens.refresh_token.clear();
            }
            Ok(false)
        })?;
        Ok(serde_json::json!({
            "logged_out": !newer_login_retained,
            "profile": name,
            "testing_environment_id": testing_environment_id,
            "pending_requests_retained": true,
            "newer_login_retained": newer_login_retained,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        http::{HeaderMap, StatusCode},
        routing::{get, post},
    };
    use serde_json::json;

    #[test]
    fn silicon_callback_hosts_are_loopback_only() -> Result<()> {
        for url in [
            "http://assistant.my-org.localhost/events",
            "http://127.0.0.1/events",
            "https://example.com/events",
        ] {
            assert!(validate_callback_endpoint(&url.parse()?).is_ok());
        }
        for url in [
            "http://notlocalhost/events",
            "http://localhost.example.com/events",
            "http://user:secret@assistant.my-org.localhost/events",
            "ftp://assistant.my-org.localhost/events",
        ] {
            assert!(validate_callback_endpoint(&url.parse()?).is_err());
        }
        assert!(
            crate::validate_endpoint(&"http://assistant.my-org.localhost/events".parse()?).is_err()
        );
        Ok(())
    }

    #[tokio::test]
    async fn login_then_hook_status_and_unhook() -> Result<()> {
        let tokens = json!({"access_token":"access", "refresh_token":"refresh", "token_type":"Bearer",
            "expires_in":3600,"scope":"dm","actor":{"type":"silicon","id":"si:cos"},"organization_id":"tos"});
        let login_tokens = tokens.clone();
        let app = Router::new()
            .route("/api/v1/auth/login", post(move |Json(body): Json<Value>| async move {
                assert_eq!(body, json!({"type":"login", "data":{"slt":"short-lived"}}));
                Json(login_tokens)
            }))
            .route("/api/v1/auth/me", get(|headers: HeaderMap| async move {
                assert_eq!(headers["authorization"], "Bearer access");
                Json(json!({"actor":{"type":"silicon","id":"si:cos"},"organization_id":"tos",
                    "principal_id":"principal","session_id":null,"org_role":null,"capabilities":[]}))
            }))
            .route("/api/v1/iam", get(|| async { Json(json!({"app_id":"dm","iam_base_url":"https://iam.example",
                "api_base_url":"https://dm.example/api/v1"})) }))
            .route("/status", get(|| async { Json(json!({"running":true,"incoming_delivery":{"code":"delivery_moved_to_ting","provider":"ting","forwarding":false},"host":{"shared":true}})) }))
            .layer(axum::middleware::from_fn(silicon_dm_protocol::responses));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let server = tokio::spawn(async move { axum::serve(listener, app).await });
        let root = std::env::temp_dir().join(format!("dm-onboarding-{}", Uuid::new_v4()));
        let runtime = LocalRuntime::new(&root)?.with_relay_home(root.join("shared-relay"))?;
        runtime.store.update(|config| {
            config.relay_port = port;
            Ok(())
        })?;
        assert_eq!(
            runtime.login_status("default", None).await?["authenticated"],
            false
        );
        let base = format!("http://127.0.0.1:{port}");
        assert_eq!(crate::Client::new(&base)?.iam().await?.app_id, "dm");
        let old_callback: Url = "http://localhost:9000/events".parse()?;
        assert!(
            runtime
                .login(
                    &LoginOptions {
                        profile: "default",
                        base_url: &base,
                        short_lived_token: "must-not-consume-this-slt",
                        webhook_url: Some(&old_callback),
                        testing_environment_id: None,
                        idempotency_key: "rejected-login-key",
                    },
                    &DaemonCommand {
                        executable: root.join("must-not-launch"),
                        arguments: vec![]
                    }
                )
                .await
                .is_err()
        );
        assert!(runtime.store.load()?.profiles.is_empty());
        let result = runtime
            .login(
                &LoginOptions {
                    profile: "default",
                    base_url: &base,
                    short_lived_token: "short-lived",
                    webhook_url: None,
                    testing_environment_id: None,
                    idempotency_key: "test-login-key",
                },
                &DaemonCommand {
                    executable: root.join("must-not-launch"),
                    arguments: vec![],
                },
            )
            .await?;
        assert_eq!(result["login"]["authenticated"], true);
        assert!(
            runtime.store.load()?.profiles["default:production"]
                .webhook_url
                .is_none()
        );
        let url: Url = "http://localhost:9000/events".parse()?;
        assert!(runtime.webhook("default", None, Some(&url)).is_err());
        let reopened = LocalRuntime::new(&root)?;
        assert!(
            reopened.store.load()?.profiles["default:production"]
                .webhook_url
                .is_none()
        );
        let status = reopened.login_status("default", None).await?;
        assert_eq!(status["authenticated"], true);
        assert_eq!(status["actor"]["id"], "si:cos");
        assert!(!status.to_string().contains("refresh_token"));
        assert!(
            runtime
                .webhook(
                    "default",
                    None,
                    Some(&"http://remote.example/events".parse()?)
                )
                .is_err()
        );
        assert!(
            runtime
                .webhook("default", Some(Uuid::new_v4()), None)
                .is_err()
        );
        assert!(runtime.webhook("default", None, None).is_err());
        assert_eq!(
            runtime.login_status("default", None).await?["authenticated"],
            true
        );
        assert!(
            runtime.store.load()?.profiles["default:production"]
                .webhook_url
                .is_none()
        );
        server.abort();
        let _ = server.await;

        // A saved file alone is not evidence that its credentials are still valid.
        let app = Router::new()
            .route(
                "/api/v1/auth/me",
                get(|| async { StatusCode::UNAUTHORIZED }),
            )
            .route(
                "/api/v1/auth/refresh",
                post(|| async { StatusCode::UNAUTHORIZED }),
            );
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
        let server = tokio::spawn(async move { axum::serve(listener, app).await });
        assert_eq!(
            runtime.login_status("default", None).await?["authenticated"],
            false
        );
        server.abort();
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[tokio::test]
    async fn early_access_rejection_renews_once_without_losing_the_family() -> Result<()> {
        use axum::response::IntoResponse;
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = calls.clone();
        let app = Router::new()
            .route("/api/v1/auth/refresh", post(move |Json(body): Json<Value>| {
                let calls = observed.clone();
                async move {
                    assert_eq!(body["data"]["refresh_token"], "old");
                    calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Json(json!({"access_token":"fresh-access","refresh_token":"fresh","token_type":"Bearer",
                        "expires_in":1800,"scope":"dm","actor":{"type":"silicon","id":"si:cos"},"organization_id":"tos"}))
                }
            }))
            .route("/api/v1/auth/me", get(|headers: HeaderMap| async move {
                if headers["authorization"] == "Bearer old-access" { return StatusCode::UNAUTHORIZED.into_response(); }
                assert_eq!(headers["authorization"], "Bearer fresh-access");
                Json(json!({"actor":{"type":"silicon","id":"si:cos"},"organization_id":"tos",
                    "principal_id":"principal","session_id":null,"org_role":null,"capabilities":[]})).into_response()
            }))
            .layer(axum::middleware::from_fn(silicon_dm_protocol::responses));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("http://{}", listener.local_addr()?);
        let server = tokio::spawn(async move { axum::serve(listener, app).await });
        let root = std::env::temp_dir().join(format!("dm-early-rejection-{}", Uuid::new_v4()));
        let runtime = LocalRuntime::new(&root)?;
        runtime.store.update(|config| {
            config.profiles.insert("default:production".into(), serde_json::from_value(json!({
                "name":"default","base_url":base,"device_id":"fixture","enabled":true,"expires_at":4_000_000_000u64,
                "testing_environment_id":null,"webhook_url":null,
                "tokens":{"access_token":"old-access","refresh_token":"old","token_type":"Bearer","expires_in":1800,
                "scope":"dm","actor":{"type":"silicon","id":"si:cos"},"organization_id":"tos"}}))?);
            Ok(())
        })?;
        for _ in 0..2 {
            assert_eq!(
                LocalRuntime::new(&root)?
                    .login_status("default", None)
                    .await?["authenticated"],
                true
            );
        }
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(
            runtime.store.load()?.profiles["default:production"]
                .tokens
                .refresh_token,
            "fresh"
        );
        server.abort();
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[tokio::test]
    async fn delayed_refresh_replay_is_renewed_before_status_and_saved_for_the_next_process()
    -> Result<()> {
        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed = calls.clone();
        let app = Router::new()
            .route("/api/v1/auth/refresh", post(move |headers: HeaderMap, Json(body): Json<Value>| {
                let calls = observed.clone();
                async move {
                    let old = body["data"]["refresh_token"].as_str().unwrap();
                    calls.lock().unwrap().push((old.to_owned(), headers["idempotency-key"].to_str().unwrap().to_owned()));
                    let new = if old == "old" { "replayed" } else { assert_eq!(old, "replayed"); "fresh" };
                    Json(json!({"access_token":format!("access-{new}"),"refresh_token":new,"token_type":"Bearer",
                        "expires_in":1800,"scope":"dm","actor":{"type":"silicon","id":"si:cos"},"organization_id":"tos"}))
                }
            }))
            .route("/api/v1/auth/me", get(|headers: HeaderMap| async move {
                assert_eq!(headers["authorization"], "Bearer access-fresh");
                Json(json!({"actor":{"type":"silicon","id":"si:cos"},"organization_id":"tos",
                    "principal_id":"principal","session_id":null,"org_role":null,"capabilities":[]}))
            }))
            .layer(axum::middleware::from_fn(silicon_dm_protocol::responses));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("http://{}", listener.local_addr()?);
        let server = tokio::spawn(async move { axum::serve(listener, app).await });
        let root = std::env::temp_dir().join(format!("dm-refresh-replay-{}", Uuid::new_v4()));
        let runtime = LocalRuntime::new(&root)?;
        runtime.store.update(|config| {
            config.profiles.insert("default:production".into(), serde_json::from_value(json!({
                "name":"default","base_url":base,"device_id":"fixture","enabled":true,"expires_at":0,"refresh_started_at":1,
                "testing_environment_id":null,"webhook_url":null,
                "tokens":{"access_token":"old-access","refresh_token":"old","token_type":"Bearer","expires_in":1800,
                "scope":"dm","actor":{"type":"silicon","id":"si:cos"},"organization_id":"tos"}}))?);
            Ok(())
        })?;
        assert_eq!(
            runtime.login_status("default", None).await?["authenticated"],
            true
        );
        let reopened = LocalRuntime::new(&root)?;
        assert_eq!(
            reopened.login_status("default", None).await?["authenticated"],
            true
        );
        let saved = reopened.store.load()?;
        assert_eq!(
            saved.profiles["default:production"].tokens.refresh_token,
            "fresh"
        );
        assert_eq!(
            saved.profiles["default:production"].refresh_started_at,
            None
        );
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert_ne!(calls[0].1, calls[1].1);
        server.abort();
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}

#[cfg(test)]
mod queue_first_tests {
    use super::*;

    #[tokio::test]
    async fn a_send_is_durably_queued_without_a_running_relay() -> Result<()> {
        let root = std::env::temp_dir().join(format!("dm-queue-first-{}", Uuid::new_v4()));
        let runtime = LocalRuntime::new(root.join("home"))?.with_relay_home(root.join("relay"))?;
        // Nothing listens on this port, so the relay is not serving this home.
        let port = std::net::TcpListener::bind("127.0.0.1:0")?
            .local_addr()?
            .port();
        runtime.store().update(|config| {
            config.relay_port = port;
            Ok(())
        })?;
        let request = crate::relay::RelayRequest {
            request_id: Uuid::new_v4(),
            profile: "default".into(),
            testing_environment_id: None,
            testing_generation: None,
            request: crate::relay::Operation::SendMessage {
                conversation_id: "c:alice::c:bob".into(),
                message: crate::MessageCreate {
                    text: Some("queued before any relay runs".into()),
                    ..Default::default()
                },
                idempotency_key: "queue-first-key".into(),
            },
        };
        // A starter that cannot launch must not lose the already-durable request.
        let starter = DaemonCommand {
            executable: root.join("missing-dm-starter"),
            arguments: vec![],
        };
        let started = std::time::Instant::now();
        let ack = runtime.submit_or_queue(&request, &starter).await?;
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        assert!(ack.acknowledged);
        assert_eq!(ack.request_id, request.request_id);
        assert_eq!(ack.request["type"], "request");
        let stored = queue::Queue::open(runtime.store())?
            .result(request.request_id)?
            .expect("the request is in the durable queue");
        assert_eq!(stored.state, "pending");
        assert_eq!(stored.request, serde_json::to_value(&request)?);
        Ok(())
    }
}
