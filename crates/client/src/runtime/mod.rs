//! Optional local runtime: Silicon Accounts sign-ins kept per profile, the durable
//! outgoing command relay, and the sync inbox.
//!
//! The default `Client` remains stateless. Enable the `runtime` feature to keep sign-ins
//! the way the CLI does, host the outgoing relay, or launch `dm-relay`. One relay process
//! serves every DM home of an operating-system user. Incoming updates are read from the
//! service's sync feed ([`inbox`]); nothing here listens for pushes.
mod daemon;
mod host;
pub mod inbox;
mod queue;
pub mod store;
pub use inbox::{InboxOptions, InboxStart};

use crate::accounts;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::path::PathBuf;

/// Windows keeps inherited standard-pipe handles inheritable. Even when the relay's
/// own stdout is a log file, CreateProcess can copy those unrelated parent handles,
/// holding a caller's output pipe open after the CLI exits. Explicit Stdio::inherit
/// still works: std duplicates the handle it is asked to pass to a child.
fn prevent_detached_pipe_inheritance() -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::{
            Foundation::{HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, SetHandleInformation},
            Storage::FileSystem::{FILE_TYPE_PIPE, GetFileType},
            System::Console::{
                GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
            },
        };
        for stream in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            // SAFETY: these APIs inspect an OS-owned standard handle and change only
            // its inheritance flag. They neither close it nor change its access rights.
            unsafe {
                let handle = GetStdHandle(stream);
                if !handle.is_null()
                    && handle != INVALID_HANDLE_VALUE
                    && GetFileType(handle) == FILE_TYPE_PIPE
                    && SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) == 0
                {
                    return Err(std::io::Error::last_os_error());
                }
            }
        }
    }
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

/// Where a profile signs in: the DM service and the Silicon Accounts that issues its tokens.
#[derive(Clone, Debug)]
pub struct SignInTarget {
    pub profile: String,
    /// The DM API (`DM_API_URL`, default the production service).
    pub api_url: String,
    /// The Silicon Accounts (`ACCOUNTS_URL`, default the production Silicon Accounts).
    pub accounts_url: String,
    /// DM's app id at Silicon Accounts (`dm`).
    pub app_id: String,
}
impl SignInTarget {
    /// Validates and normalizes both URLs and the profile name.
    pub fn new(profile: &str, api_url: &str, accounts_url: &str, app_id: &str) -> Result<Self> {
        store::validate_profile_name(profile)?;
        let api_url = crate::api_base(api_url)?
            .as_str()
            .trim_end_matches('/')
            .trim_end_matches("/api/v1")
            .to_owned();
        Ok(Self {
            profile: profile.into(),
            api_url,
            accounts_url: accounts::normalize_accounts_url(accounts_url)?,
            app_id: app_id.trim().to_owned(),
        })
    }
}

/// What the DM service said about signing in to it.
#[derive(Clone, Debug)]
pub enum ServiceCheck {
    /// It trusts this Silicon Accounts and app id.
    Matches(Box<crate::SignInDiscovery>),
    /// It could not be reached; signing in goes ahead (the service is checked on first use).
    Unreachable(String),
}

/// Optional stateful component, separate from the stateless protocol client.
#[derive(Clone)]
pub struct LocalRuntime {
    store: store::Store,
}

fn rfc3339(seconds: u64) -> Value {
    i64::try_from(seconds)
        .ok()
        .and_then(|seconds| time::OffsetDateTime::from_unix_timestamp(seconds).ok())
        .and_then(|at| {
            at.format(&time::format_description::well_known::Rfc3339)
                .ok()
        })
        .map_or(Value::Null, Value::String)
}

/// The status JSON of a signed-in profile (`dm login status --json`, `dm login`).
pub fn signed_in_status(profile: &store::Profile, verified: bool) -> Value {
    let account = profile.account.as_ref();
    let mut status = json!({
        "authenticated": true,
        "profile": profile.name,
        "uuid": account.map(|a| a.uuid.as_str()),
        "id": account.map(|a| a.id.as_str()),
        "kind": account.map(|a| a.kind.as_str()),
        "expires_at": rfc3339(profile.expires_at),
        "refresh_expires_at": profile
            .tokens
            .as_ref()
            .and_then(|t| t.refresh_expires_at)
            .map_or(Value::Null, rfc3339),
        "verified": verified,
        "method": profile.method,
        "api_url": profile.base_url,
        "accounts_url": profile.accounts_url,
    });
    if let Some(name) = account.and_then(|a| a.display_name.as_deref()) {
        status["display_name"] = json!(name);
    }
    if let Some(custodian) = account.and_then(|a| a.custodian.as_ref()) {
        status["custodian"] = json!({"uuid": custodian.uuid, "id": custodian.id});
    }
    status
}

/// `dm login status` without the network: `{"authenticated":false}` (plus `reason` and
/// `message` when there is something to say), or the stored sign-in. Creates nothing; works
/// without HOME. The second value says whether the profile holds a sign-in.
pub fn offline_status(profile: &str) -> (Value, bool) {
    let signed_out = json!({"authenticated": false});
    let Some(directory) = store::peek_directory() else {
        return (signed_out, false);
    };
    let config = match store::peek(&directory) {
        Ok(Some(config)) => config,
        Ok(None) => return (signed_out, false),
        Err(error) => {
            return (
                json!({"authenticated": false, "reason": "unreadable_state", "message": format!("{error:#}")}),
                false,
            );
        }
    };
    let Some(stored) = config.profiles.get(&store::session_key(profile)) else {
        return (signed_out, false);
    };
    if stored.signed_in() {
        return (signed_in_status(stored, false), true);
    }
    match store::profile(&config, profile) {
        Err(error) => match error.downcast_ref::<store::SessionError>() {
            Some(reason) if reason.code != "not_signed_in" => (
                json!({"authenticated": false, "reason": reason.code, "message": reason.message, "hint": reason.hint}),
                false,
            ),
            _ => (signed_out, false),
        },
        Ok(_) => (signed_out, false),
    }
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
    /// Caller-controlled profile storage.
    pub fn store(&self) -> &store::Store {
        &self.store
    }
    /// Persist the diagnostic opt-in for CLI and daemon requests.
    pub fn set_telemetry(&self, enabled: bool) -> Result<()> {
        self.store.update(|config| {
            config.telemetry_enabled = enabled;
            Ok(())
        })
    }
    /// Best-effort, bounded diagnostics for a signed-in profile, spooled locally and uploaded
    /// by the relay. A command never waits on the network for its own diagnostic.
    pub fn record_diagnostic(
        &self,
        profile: &str,
        source: &'static str,
        event: &str,
        success: bool,
        duration_ms: u64,
    ) {
        let Ok(config) = self.store.load() else {
            return;
        };
        if store::profile(&config, profile).is_err() || !config.telemetry() {
            return;
        }
        if let Ok(queue) = queue::Queue::open(&self.store) {
            let _ = queue.record_diagnostic(
                &store::session_key(profile),
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
        if request.testing_environment_id.is_some() {
            bail!(
                "test environments were retired with the move to Silicon Accounts; this request was not queued"
            );
        }
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
            // SAFETY: setsid is async-signal-safe.
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
        prevent_detached_pipe_inheritance()?;
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

    /// DM's public client at the target's Silicon Accounts, with this home's telemetry choice.
    pub fn public_client(&self, target: &SignInTarget) -> Result<accounts::PublicClient> {
        let telemetry = self.store.load().map(|c| c.telemetry()).unwrap_or(true);
        Ok(accounts::PublicClient::new(
            &target.accounts_url,
            &target.app_id,
            telemetry,
        )?)
    }

    /// Asks the DM service (`GET /api/v1/accounts`, no credential) which Silicon Accounts and
    /// app id it trusts, before a single-use code or token is spent. A service that trusts
    /// another Silicon Accounts or app id, or predates Silicon Accounts, is refused; an
    /// unreachable one is reported and signing in goes ahead.
    pub async fn check_service(target: &SignInTarget) -> Result<ServiceCheck> {
        let client = crate::Client::new(&target.api_url)?.with_source("cli");
        match client.accounts().await {
            Ok(discovery) => {
                let trusted = accounts::normalize_accounts_url(&discovery.accounts_url)
                    .unwrap_or_else(|_| discovery.accounts_url.clone());
                if trusted != target.accounts_url {
                    return Err(anyhow::Error::new(store::SessionError {
                        code: "accounts_mismatch",
                        message: format!(
                            "The DM service at {} trusts Silicon Accounts at {trusted}, but this dm signs in at {}.",
                            target.api_url, target.accounts_url
                        ),
                        hint: format!(
                            "Set ACCOUNTS_URL={trusted} (or pass --accounts-url), or point DM_API_URL at the DM service you meant."
                        ),
                    }));
                }
                if discovery.app_id != target.app_id {
                    return Err(anyhow::Error::new(store::SessionError {
                        code: "app_id_mismatch",
                        message: format!(
                            "The DM service at {} signs in as the app `{}`, but this dm uses `{}`.",
                            target.api_url, discovery.app_id, target.app_id
                        ),
                        hint: "Unset DM_APP_ID, or point DM_API_URL at the DM service you meant."
                            .into(),
                    }));
                }
                Ok(ServiceCheck::Matches(Box::new(discovery)))
            }
            Err(
                error @ crate::Error::Api {
                    status: 404 | 406, ..
                },
            ) => Err(anyhow::Error::new(store::SessionError {
                code: "service_not_accounts",
                message: format!(
                    "The DM service at {} does not sign in with Silicon Accounts ({}): it runs a DM release before 0.15.",
                    target.api_url,
                    error.message()
                ),
                hint: "Point DM_API_URL (or --api-url) at a DM 0.15 service; nothing was spent."
                    .into(),
            })),
            Err(error) => Ok(ServiceCheck::Unreachable(error.to_string())),
        }
    }
}

impl LocalRuntime {
    /// Stores a new sign-in in the target profile and starts the relay.
    ///
    /// - A profile signed in as another account (or bound to another DM service) is replaced
    ///   only when it has no queued requests, so nothing queued is ever sent as someone else.
    /// - A profile from before Silicon Accounts releases its queued requests when the same
    ///   public id signs in, and fails them (code `account_changed`) otherwise.
    /// - The replaced sign-in's refresh token is revoked.
    ///
    /// If the relay cannot start, the sign-in is kept and the result says so.
    pub async fn save_sign_in(
        &self,
        target: &SignInTarget,
        session: &accounts::Session,
        method: &str,
        launch: &DaemonCommand,
    ) -> Result<Value> {
        let account = session
            .account
            .clone()
            .context("Silicon Accounts did not say which account signed in")?;
        let key = store::session_key(&target.profile);
        let lock = self.store.profile_lock(&key).await?;
        let queue = queue::Queue::open(&self.store)?;
        let pending = queue.pending_count(&key)?;
        let mut released = None;
        let replaced = self.store.update(|config| {
            let existing = config.profiles.get(&key).cloned();
            if let Some(existing) = &existing {
                let other_account = existing
                    .account
                    .as_ref()
                    .is_some_and(|previous| previous.uuid != account.uuid);
                let other_service = existing.base_url.trim_end_matches('/')
                    != target.api_url.trim_end_matches('/');
                if pending > 0 && existing.legacy.is_none() && (other_account || other_service) {
                    let who = existing
                        .account
                        .as_ref()
                        .map_or_else(|| "another account".to_owned(), |a| a.id.clone());
                    return Err(anyhow::Error::new(store::SessionError {
                        code: "profile_in_use",
                        message: format!(
                            "The DM profile `{}` has {pending} queued request(s) from {who} at {}; signing in as {} at {} would send them as someone else, so nothing changed.",
                            target.profile, existing.base_url, account.id, target.api_url
                        ),
                        hint: format!(
                            "Sign in to another profile (dm --profile <name> login), or sign in as {who} again so the queue drains."
                        ),
                    }));
                }
                if let Some(legacy) = &existing.legacy {
                    released = Some(legacy.actor_id.as_deref() == Some(account.id.as_str()));
                }
            }
            let replaced = existing.as_ref().and_then(|p| {
                p.tokens.as_ref().map(|t| {
                    (
                        p.accounts_url.clone(),
                        p.app_id.clone(),
                        t.refresh_token.clone(),
                    )
                })
            });
            let mut profile = existing.unwrap_or_else(|| {
                store::Profile::new(
                    &target.profile,
                    &target.api_url,
                    &target.accounts_url,
                    &target.app_id,
                )
            });
            profile.name.clone_from(&target.profile);
            profile.base_url.clone_from(&target.api_url);
            profile.accounts_url.clone_from(&target.accounts_url);
            profile.app_id.clone_from(&target.app_id);
            profile.account = Some(account.clone().into());
            profile.apply_session(session);
            profile.signed_in_at = Some(store::now());
            profile.method = Some(method.into());
            profile.legacy = None;
            config.profiles.insert(key.clone(), profile);
            Ok(replaced)
        })?;
        let mut queued = json!({"pending": pending});
        match released {
            Some(true) => queued["released"] = json!(pending),
            Some(false) => {
                let failed = queue.fail_pending(
                    &key,
                    &json!({"code":"account_changed","message":format!(
                        "This request was queued before DM moved to Silicon Accounts by a sign-in with another id; the profile is now signed in as {}, so it was not sent. Resubmit it if {} should send it.",
                        account.id, account.id
                    )}),
                )?;
                queued["failed_account_changed"] = json!(failed);
            }
            None => {}
        }
        drop(lock);
        let mut result =
            signed_in_status(store::profile(&self.store.load()?, &target.profile)?, false);
        if let Some((accounts_url, app_id, refresh)) = replaced
            && refresh != session.refresh_token
        {
            let revoked = match accounts::PublicClient::new(&accounts_url, &app_id, true) {
                Ok(client) => client.revoke(&refresh).await.is_ok(),
                Err(_) => false,
            };
            result["replaced_sign_in_revoked"] = json!(revoked);
        }
        result["queue"] = queued;
        result["relay"] = match self.start_with(launch, None).await {
            Ok(status) => status,
            Err(error) => {
                json!({"running": false, "error": format!("{error:#}"), "next": "dm daemon start"})
            }
        };
        Ok(result)
    }

    /// `dm login status`: refreshes the sign-in when needed and confirms it with
    /// `GET /api/v1/me` (`verified: true`, current id and name). A service that cannot be
    /// reached leaves the stored sign-in reported unverified with a warning; a sign-in Silicon
    /// Accounts ended reports `authenticated: false`.
    pub async fn login_status(&self, name: &str) -> Result<Value> {
        let key = store::session_key(name);
        let signed_out = |error: &anyhow::Error| {
            error.downcast_ref::<store::SessionError>().map(|reason| {
                if reason.code == "not_signed_in" {
                    json!({"authenticated": false})
                } else {
                    json!({"authenticated": false, "reason": reason.code, "message": reason.message, "hint": reason.hint})
                }
            })
        };
        for attempt in 0..2 {
            let (config, profile) = match self.store.fresh_profile(&key).await {
                Ok(fresh) => fresh,
                Err(error) => {
                    if let Some(status) = signed_out(&error) {
                        return Ok(status);
                    }
                    let config = self.store.load()?;
                    let stored = store::profile(&config, name)?;
                    let mut status = signed_in_status(stored, false);
                    status["warning"] =
                        json!(format!("The sign-in could not be refreshed: {error:#}"));
                    return Ok(status);
                }
            };
            match store::client(&config, &profile)?.me().await {
                Ok(me) => {
                    let updated = self.store.update(|config| {
                        if let Some(stored) = config.profiles.get_mut(&key)
                            && let Some(account) = stored.account.as_mut()
                            && me.account.uuid.as_deref() == Some(account.uuid.as_str())
                        {
                            account.id.clone_from(&me.account.id);
                            if me.account.display_name.is_some() {
                                account.display_name.clone_from(&me.account.display_name);
                            }
                            if let Some(custodian) = &me.custodian {
                                account.custodian =
                                    custodian.uuid.as_ref().map(|uuid| accounts::AccountRef {
                                        uuid: uuid.clone(),
                                        id: custodian.id.clone(),
                                    });
                            }
                        }
                        Ok(config.profiles.get(&key).cloned())
                    })?;
                    let profile = updated.unwrap_or(profile);
                    let mut status = signed_in_status(&profile, true);
                    if !me.custodian_of.is_empty() {
                        status["custodian_of"] = json!(
                            me.custodian_of
                                .iter()
                                .map(|s| json!({"uuid": s.uuid, "id": s.id}))
                                .collect::<Vec<_>>()
                        );
                    }
                    status["delivery"] = json!(me.delivery.transport);
                    return Ok(status);
                }
                Err(error) if error.unauthorized() && attempt == 0 => {
                    // Refused before its advertised expiry (signed out elsewhere, or a
                    // cutoff): refresh once; Silicon Accounts decides whether it still holds.
                    let refused = profile.access_token().map(str::to_owned);
                    self.store.update(|config| {
                        if let Some(stored) = config.profiles.get_mut(&key)
                            && stored.access_token().map(str::to_owned) == refused
                        {
                            stored.expires_at = 0;
                        }
                        Ok(())
                    })?;
                }
                Err(error) if error.unauthorized() => {
                    return Ok(json!({
                        "authenticated": false,
                        "reason": "refused_by_service",
                        "message": format!("The DM service refused this profile's fresh sign-in: {}", error.message()),
                        "hint": store::sign_in_hint(name),
                    }));
                }
                Err(error) => {
                    let mut status = signed_in_status(&profile, false);
                    status["warning"] = json!(format!(
                        "The DM service could not confirm the sign-in ({}); this is the stored sign-in.",
                        error
                    ));
                    return Ok(status);
                }
            }
        }
        unreachable!("the second attempt always returns")
    }

    /// Signs the profile out: revokes its refresh token at Silicon Accounts (other machines
    /// stay signed in) and forgets the tokens. Queued requests stay queued for the next
    /// sign-in of the same account. A failed revoke still signs out locally and says so.
    pub async fn logout(&self, name: &str) -> Result<Value> {
        let key = store::session_key(name);
        let lock = self.store.profile_lock(&key).await?;
        let config = self.store.load()?;
        let Some(profile) = config
            .profiles
            .get(&key)
            .filter(|p| p.tokens.is_some())
            .cloned()
        else {
            return Ok(json!({"signed_out": false, "reason": "not_signed_in", "profile": name}));
        };
        let refresh = profile
            .tokens
            .as_ref()
            .map(|t| t.refresh_token.clone())
            .unwrap_or_default();
        let revoked = match accounts::PublicClient::new(
            &profile.accounts_url,
            &profile.app_id,
            config.telemetry(),
        ) {
            Ok(client) => client.revoke(&refresh).await,
            Err(error) => Err(error),
        };
        self.store.update(|config| {
            if let Some(stored) = config.profiles.get_mut(&key)
                && stored.tokens.as_ref().map(|t| &t.refresh_token) == Some(&refresh)
            {
                stored.clear_sign_in(None);
            }
            Ok(())
        })?;
        drop(lock);
        let pending = queue::Queue::open(&self.store)?.pending_count(&key)?;
        let account = profile.account.as_ref();
        let mut result = json!({
            "signed_out": true,
            "revoked": revoked.is_ok(),
            "profile": name,
            "uuid": account.map(|a| a.uuid.as_str()),
            "id": account.map(|a| a.id.as_str()),
            "kind": account.map(|a| a.kind.as_str()),
            "pending_requests_retained": pending,
        });
        if let Err(error) = revoked {
            result["warning"] = json!(format!(
                "Signed out on this machine, but Silicon Accounts did not confirm the revoke: {error}"
            ));
        }
        Ok(result)
    }

    /// Refreshes the profile's sign-in now (normally automatic) and reports it.
    pub async fn refresh(&self, name: &str) -> Result<Value> {
        let key = store::session_key(name);
        let config = self.store.load()?;
        let current = store::profile(&config, name)?.clone();
        self.store.update(|config| {
            if let Some(stored) = config.profiles.get_mut(&key)
                && stored.access_token() == current.access_token()
            {
                stored.expires_at = 0;
            }
            Ok(())
        })?;
        let (_, profile) = self.store.fresh_profile(&key).await?;
        let mut status = signed_in_status(&profile, false);
        status["refreshed"] = json!(profile.access_token() != current.access_token());
        Ok(status)
    }

    /// Every profile of this home, without secrets.
    pub fn profiles(&self) -> Result<Value> {
        let config = self.store.load()?;
        let profiles: Vec<Value> = config
            .profiles
            .values()
            .map(|profile| {
                let account = profile.account.as_ref();
                let mut entry = json!({
                    "name": profile.name,
                    "signed_in": profile.signed_in(),
                    "uuid": account.map(|a| a.uuid.as_str()),
                    "id": account.map(|a| a.id.as_str()),
                    "kind": account.map(|a| a.kind.as_str()),
                    "api_url": profile.base_url,
                    "accounts_url": profile.accounts_url,
                    "method": profile.method,
                });
                if let Some(legacy) = &profile.legacy {
                    entry["signed_in_before_silicon_accounts_as"] = json!(legacy.actor_id);
                    if legacy.testing_environment_id.is_some() {
                        entry["retired_test_environment"] = json!(true);
                    }
                }
                if let Some(ended) = &profile.ended {
                    entry["ended"] = json!({"reason": ended.reason, "message": ended.message, "at": rfc3339(ended.at)});
                }
                entry
            })
            .collect();
        Ok(json!({"default_profile": config.default_profile, "profiles": profiles}))
    }
}

#[cfg(test)]
#[path = "runtime_tests.rs"]
mod tests;
