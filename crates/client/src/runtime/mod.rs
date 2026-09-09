//! Optional durable relay owned by an explicitly selected local state directory.
//!
//! The default `Client` remains stateless. Enable the `runtime` feature to host
//! the same relay used by the CLI or launch the packaged `dm-relay` executable.
mod daemon;
mod queue;
pub mod store;
pub mod updates;

use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::path::PathBuf;
use url::Url;
use uuid::Uuid;

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

/// A login's local mapping. The SLT is sent only to DM; the callback stays local.
pub struct LoginOptions<'a> {
    pub profile: &'a str,
    pub base_url: &'a str,
    pub short_lived_token: &'a str,
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
    /// Opens a private, caller-owned directory; multiple directories are independent.
    pub fn new(directory: impl Into<PathBuf>) -> Result<Self> {
        Ok(Self {
            store: store::Store::new(directory)?,
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
    /// An authenticated transport to this runtime's local HTTP relay.
    pub fn client(&self) -> Result<crate::relay::RelayClient> {
        store::relay(&self.store.load()?)
    }
    /// Runs the durable listener until local shutdown or Ctrl-C. Dropping this
    /// future cancels its owned workers and connections; queues remain on disk.
    pub async fn run(&self) -> Result<()> {
        daemon::run(self.store.clone()).await
    }
    /// Starts the packaged `dm-relay` executable from PATH.
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
        if let Some(url) = options.webhook_url {
            crate::validate_endpoint(url)?;
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
                webhook_url: options.webhook_url.map(ToString::to_string), device_id,
                expires_at: store::now().saturating_add(tokens.expires_in.max(0) as u64),
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
        let result = async {
            let (config, profile) = self.store.fresh_profile(&store::session_key(name, test)).await?;
            let identity = store::client(&config, &profile)?.me().await?;
            Ok::<_, anyhow::Error>(serde_json::json!({"authenticated":true,"profile":name,"testing_environment_id":test,
                "actor":identity.actor,"organization_id":identity.organization_id,"identity":identity,"webhook_url":profile.webhook_url}))
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
    /// Sets or removes a local callback without changing the IAM login or deleting queued events.
    pub fn webhook(&self, name: &str, test: Option<Uuid>, url: Option<&Url>) -> Result<Value> {
        if let Some(url) = url {
            crate::validate_endpoint(url)?;
        }
        self.store.update(|config| {
            let profile = config.profiles.get_mut(&store::session_key(name, test))
                .filter(|p| p.enabled).context("log in first with dm login <slt>")?;
            profile.webhook_url = url.map(ToString::to_string);
            Ok(serde_json::json!({"profile":name,"testing_environment_id":test,"webhook_url":profile.webhook_url,"hooked":url.is_some()}))
        })
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

    #[tokio::test]
    async fn login_then_hook_status_and_unhook() -> Result<()> {
        let tokens = json!({"access_token":"access", "refresh_token":"refresh", "token_type":"Bearer",
            "expires_in":3600,"scope":"dm","actor":{"type":"silicon","id":"cos:tos"},"organization_id":"tos"});
        let login_tokens = tokens.clone();
        let app = Router::new()
            .route("/api/v1/auth/login", post(move |Json(body): Json<Value>| async move {
                assert_eq!(body, json!({"slt":"short-lived"}));
                Json(login_tokens)
            }))
            .route("/api/v1/auth/me", get(|headers: HeaderMap| async move {
                assert_eq!(headers["authorization"], "Bearer access");
                Json(json!({"actor":{"type":"silicon","id":"cos:tos"},"organization_id":"tos",
                    "principal_id":"principal","session_id":null,"org_role":null,"capabilities":[]}))
            }))
            .route("/api/v1/iam", get(|| async { Json(json!({"app_id":"tos>dm","iam_base_url":"https://iam.example",
                "api_base_url":"https://dm.example/api/v1"})) }))
            .route("/status", get(|| async { Json(json!({"running":true})) }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let server = tokio::spawn(async move { axum::serve(listener, app).await });
        let root = std::env::temp_dir().join(format!("dm-onboarding-{}", Uuid::new_v4()));
        let runtime = LocalRuntime::new(&root)?;
        runtime.store.update(|config| {
            config.relay_port = port;
            Ok(())
        })?;
        assert_eq!(
            runtime.login_status("default", None).await?["authenticated"],
            false
        );
        let base = format!("http://127.0.0.1:{port}");
        assert_eq!(crate::Client::new(&base)?.iam().await?.app_id, "tos>dm");
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
        assert_eq!(
            runtime.webhook("default", None, Some(&url))?["hooked"],
            true
        );
        let reopened = LocalRuntime::new(&root)?;
        assert_eq!(
            reopened.store.load()?.profiles["default:production"]
                .webhook_url
                .as_deref(),
            Some(url.as_str())
        );
        let status = reopened.login_status("default", None).await?;
        assert_eq!(status["authenticated"], true);
        assert_eq!(status["actor"]["id"], "cos:tos");
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
        assert_eq!(runtime.webhook("default", None, None)?["hooked"], false);
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
        let app = Router::new().route(
            "/api/v1/auth/me",
            get(|| async { StatusCode::UNAUTHORIZED }),
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
}
