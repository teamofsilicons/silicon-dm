//! Optional durable relay owned by an explicitly selected local state directory.
//!
//! The default `Client` remains stateless. Enable the `runtime` feature to host
//! the same relay used by the CLI or launch the packaged `dm-relay` executable.
mod daemon;
mod queue;
pub mod store;
pub mod updates;

use anyhow::{Result, bail};
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
    pub webhook_url: &'a Url,
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
    /// Uses `SILICON_DM_HOME`, or `~/.silicon-dm` when it is absent.
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
            .login(
                options.short_lived_token,
                options.webhook_url,
                options.idempotency_key,
            )
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
                webhook_url: options.webhook_url.to_string(), device_id,
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
