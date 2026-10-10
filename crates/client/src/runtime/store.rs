//! Local DM state of one home: its profiles (each one Silicon Accounts sign-in of one
//! account), the shared relay's port and this home's relay bearer.
//!
//! `config.json` lives in `$SILICON_DM_HOME`, else the directory set with `dm config home`,
//! else `$SILICON_HOME/.silicon-dm`, else `~/.silicon-dm`. Directories are 0700 and files
//! 0600; symbolic links are refused; every write replaces the file atomically under
//! `config.lock`. Refresh tokens rotate on every use and presenting a used one ends the whole
//! sign-in, so a refresh runs under a per-profile file lock (`refresh-<profile>.lock`),
//! re-reads the file after taking it, and stores the new pair before anything uses it.
//!
//! Profiles written before DM moved to Silicon Accounts are read, never trusted: they load
//! as signed out with a `legacy` note (the old id), and their queued requests wait for the
//! profile's next sign-in (sent only if the same id signs in again).
use crate::{ActorType, Client, accounts};
use anyhow::{Context, Result, bail};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

/// The profile format this release writes.
pub const PROFILE_FORMAT: u32 = 2;
/// Where `dm` signs in unless `ACCOUNTS_URL` or `--accounts-url` says otherwise.
pub const DEFAULT_ACCOUNTS_URL: &str = accounts::DEFAULT_ACCOUNTS_URL;
/// The production DM service unless `DM_API_URL` or `--api-url` says otherwise.
pub const DEFAULT_API_URL: &str = crate::DEFAULT_API_URL;
const DEFAULT_RELAY_PORT: u16 = 19780;

/// A sign-in problem the CLI reports with a code, the exact reason and the next step.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct SessionError {
    pub code: &'static str,
    pub message: String,
    pub hint: String,
}
impl SessionError {
    fn not_signed_in(profile: &str) -> Self {
        Self {
            code: "not_signed_in",
            message: format!("The DM profile `{profile}` is not signed in."),
            hint: sign_in_hint(profile),
        }
    }
}
/// How to sign a profile in, for Carbons and for Silicons.
pub fn sign_in_hint(profile: &str) -> String {
    let flag = if profile == "default" {
        String::new()
    } else {
        format!("--profile {profile} ")
    };
    format!(
        "Carbon: dm {flag}login (approve the code it prints). Silicon: silicon-accounts login --app dm -q | dm {flag}login --slt-stdin"
    )
}

/// The account a profile is signed in as (kept after sign-out, for `profiles list` and to
/// decide whether queued requests may be sent after the next sign-in).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileAccount {
    pub uuid: String,
    pub kind: ActorType,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custodian: Option<accounts::AccountRef>,
}
impl ProfileAccount {
    pub fn actor(&self) -> crate::Actor {
        crate::Actor::new(self.kind, self.id.clone()).with_uuid(self.uuid.clone())
    }
}
impl From<accounts::SessionAccount> for ProfileAccount {
    fn from(account: accounts::SessionAccount) -> Self {
        Self {
            uuid: account.uuid,
            kind: account.kind,
            id: account.id,
            display_name: account.display_name,
            custodian: account.custodian,
        }
    }
}

/// The current token pair. Deliberately not `Debug`.
#[derive(Clone, Serialize, Deserialize)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// Lifetime of the access token in seconds (30 minutes).
    pub expires_in: u64,
    /// Unix seconds when the refresh family ends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_expires_at: Option<u64>,
}

/// A sign-in from before Silicon Accounts that this profile held.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LegacySignIn {
    /// The public id it used (`c:x`, `si:x`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_id: Option<String>,
    /// Set for a profile of a retired test environment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub testing_environment_id: Option<Uuid>,
    pub migrated_at: u64,
}

/// Why the last sign-in ended without `dm logout`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EndedSignIn {
    pub reason: String,
    pub message: String,
    pub at: u64,
}

#[derive(Clone, Serialize, Deserialize)]
struct ProfileWire {
    format: u32,
    name: String,
    base_url: String,
    accounts_url: String,
    app_id: String,
    #[serde(default)]
    account: Option<ProfileAccount>,
    #[serde(default)]
    tokens: Option<Tokens>,
    device_id: String,
    #[serde(default)]
    expires_at: u64,
    #[serde(default)]
    enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    signed_in_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    method: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    legacy: Option<LegacySignIn>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ended: Option<EndedSignIn>,
}

/// One local profile: a DM service, the Silicon Accounts that signs it in, and the
/// current sign-in.
#[derive(Clone)]
pub struct Profile {
    pub name: String,
    /// The DM API it talks to.
    pub base_url: String,
    /// The Silicon Accounts that issued the sign-in; refresh and sign-out go there.
    pub accounts_url: String,
    pub app_id: String,
    pub account: Option<ProfileAccount>,
    pub tokens: Option<Tokens>,
    /// Stable id of this machine's presence lease and receipts.
    pub device_id: String,
    /// Unix seconds when the access token expires.
    pub expires_at: u64,
    pub enabled: bool,
    pub signed_in_at: Option<u64>,
    /// `device` or `slt`.
    pub method: Option<String>,
    pub legacy: Option<LegacySignIn>,
    pub ended: Option<EndedSignIn>,
}
impl Serialize for Profile {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        ProfileWire {
            format: PROFILE_FORMAT,
            name: self.name.clone(),
            base_url: self.base_url.clone(),
            accounts_url: self.accounts_url.clone(),
            app_id: self.app_id.clone(),
            account: self.account.clone(),
            tokens: self.tokens.clone(),
            device_id: self.device_id.clone(),
            expires_at: self.expires_at,
            enabled: self.enabled,
            signed_in_at: self.signed_in_at,
            method: self.method.clone(),
            legacy: self.legacy.clone(),
            ended: self.ended.clone(),
        }
        .serialize(serializer)
    }
}
impl<'de> Deserialize<'de> for Profile {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        // Later formats are read the same way (unknown fields are ignored).
        if value
            .get("format")
            .and_then(Value::as_u64)
            .is_some_and(|format| format >= u64::from(PROFILE_FORMAT))
        {
            let wire: ProfileWire =
                serde_json::from_value(value).map_err(serde::de::Error::custom)?;
            return Ok(Self {
                name: wire.name,
                base_url: wire.base_url,
                accounts_url: wire.accounts_url,
                app_id: wire.app_id,
                account: wire.account,
                tokens: wire.tokens,
                device_id: wire.device_id,
                expires_at: wire.expires_at,
                enabled: wire.enabled,
                signed_in_at: wire.signed_in_at,
                method: wire.method,
                legacy: wire.legacy,
                ended: wire.ended,
            });
        }
        Ok(Self::from_legacy(&value))
    }
}
impl Profile {
    /// A profile with no sign-in yet.
    pub fn new(name: &str, base_url: &str, accounts_url: &str, app_id: &str) -> Self {
        Self {
            name: name.into(),
            base_url: base_url.into(),
            accounts_url: accounts_url.into(),
            app_id: app_id.into(),
            account: None,
            tokens: None,
            device_id: Uuid::new_v4().to_string(),
            expires_at: 0,
            enabled: false,
            signed_in_at: None,
            method: None,
            legacy: None,
            ended: None,
        }
    }
    /// Reads a profile written before Silicon Accounts (its tokens, maybe a test environment)
    /// as signed out, keeping only its old public id. Its tokens are dropped: they can never
    /// authenticate.
    fn from_legacy(value: &Value) -> Self {
        let text = |pointer: &str| {
            value
                .pointer(pointer)
                .and_then(Value::as_str)
                .filter(|v| !v.trim().is_empty())
                .map(str::to_owned)
        };
        let mut profile = Self::new(
            &text("/name").unwrap_or_else(|| "default".into()),
            &text("/base_url").unwrap_or_else(|| DEFAULT_API_URL.into()),
            DEFAULT_ACCOUNTS_URL,
            crate::APP_ID,
        );
        if let Some(device) = text("/device_id") {
            profile.device_id = device;
        }
        profile.legacy = Some(LegacySignIn {
            actor_id: text("/tokens/actor/id").or_else(|| text("/tokens/member/id")),
            testing_environment_id: text("/testing_environment_id")
                .and_then(|id| Uuid::parse_str(&id).ok()),
            migrated_at: now(),
        });
        profile
    }
    /// Whether the profile holds a usable sign-in.
    pub fn signed_in(&self) -> bool {
        self.enabled && self.tokens.is_some() && self.account.is_some()
    }
    pub fn access_token(&self) -> Option<&str> {
        self.tokens
            .as_ref()
            .map(|tokens| tokens.access_token.as_str())
    }
    fn same_sign_in(&self, other: &Self) -> bool {
        self.account.as_ref().map(|a| &a.uuid) == other.account.as_ref().map(|a| &a.uuid)
            && self.tokens.as_ref().map(|t| &t.refresh_token)
                == other.tokens.as_ref().map(|t| &t.refresh_token)
    }
    /// Stores a fresh sign-in (or a refreshed token pair of the same account).
    pub fn apply_session(&mut self, session: &accounts::Session) {
        if let Some(account) = &session.account {
            self.account = Some(account.clone().into());
        }
        self.tokens = Some(Tokens {
            access_token: session.access_token.clone(),
            refresh_token: session.refresh_token.clone(),
            scope: session.scope.clone(),
            expires_in: session.expires_in,
            refresh_expires_at: session.refresh_expires_at,
        });
        self.expires_at = session.expires_at;
        self.enabled = true;
        self.ended = None;
    }
    /// Forgets the tokens (signed out, or the sign-in ended); keeps who it was.
    pub fn clear_sign_in(&mut self, ended: Option<EndedSignIn>) {
        self.tokens = None;
        self.enabled = false;
        self.expires_at = 0;
        self.ended = ended;
    }
}

/// One home's settings.
#[derive(Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_profile_name")]
    pub default_profile: String,
    #[serde(default)]
    pub profiles: BTreeMap<String, Profile>,
    /// Port of the shared relay serving this home. The relay rewrites it when it
    /// attaches the home, and prefers it when it starts, so it stays stable.
    #[serde(default = "default_relay_port")]
    pub relay_port: u16,
    /// This home's private bearer; the shared relay routes requests by it.
    #[serde(default = "new_relay_token")]
    pub relay_token: String,
    #[serde(default = "enabled")]
    pub telemetry_enabled: bool,
    // Settings of retired features (test environment keys, webhook secrets, update checks)
    // are ignored when read and dropped at the next write.
}
fn default_profile_name() -> String {
    "default".into()
}
fn default_relay_port() -> u16 {
    DEFAULT_RELAY_PORT
}
fn new_relay_token() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}
fn enabled() -> bool {
    true
}
impl Default for Config {
    fn default() -> Self {
        Self {
            default_profile: default_profile_name(),
            profiles: BTreeMap::new(),
            relay_port: DEFAULT_RELAY_PORT,
            relay_token: new_relay_token(),
            telemetry_enabled: true,
        }
    }
}
impl Config {
    /// Whether diagnostics may be sent: this home's setting and `DM_TELEMETRY_ENABLED`.
    pub fn telemetry(&self) -> bool {
        self.telemetry_enabled && std::env::var("DM_TELEMETRY_ENABLED").as_deref() != Ok("false")
    }
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// The internal key of a profile in `config.json` and the local queue. The `:production`
/// suffix dates from test environments; it is kept so requests queued by earlier releases
/// stay attached to their profile.
pub fn session_key(name: &str) -> String {
    format!("{name}:production")
}
/// `1-64` letters, digits, underscores or hyphens.
pub fn validate_profile_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 64
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
    {
        bail!("profile names must use 1-64 letters, digits, underscores or hyphens")
    }
    Ok(())
}
/// The signed-in profile `name`, or a [`SessionError`] saying why it is not usable.
pub fn profile<'a>(config: &'a Config, name: &str) -> Result<&'a Profile> {
    let key = session_key(name);
    let Some(profile) = config.profiles.get(&key) else {
        return Err(SessionError::not_signed_in(name).into());
    };
    if profile.signed_in() {
        return Ok(profile);
    }
    Err(signed_out_reason(profile).into())
}
fn signed_out_reason(profile: &Profile) -> SessionError {
    if let Some(ended) = &profile.ended {
        return SessionError {
            code: "sign_in_ended",
            message: format!(
                "The DM profile `{}` was signed out by Silicon Accounts: {}",
                profile.name, ended.message
            ),
            hint: sign_in_hint(&profile.name),
        };
    }
    if let Some(legacy) = &profile.legacy {
        return SessionError {
            code: "sign_in_required",
            message: format!(
                "The DM profile `{}` was signed in{} before DM moved to Silicon Accounts; that sign-in no longer works.",
                profile.name,
                legacy
                    .actor_id
                    .as_deref()
                    .map(|id| format!(" as {id}"))
                    .unwrap_or_default()
            ),
            hint: format!(
                "{} Requests it queued are sent after the same id signs in again.",
                sign_in_hint(&profile.name)
            ),
        };
    }
    SessionError::not_signed_in(&profile.name)
}
/// A DM client for the profile's current access token.
pub fn client(config: &Config, profile: &Profile) -> Result<Client> {
    let token = profile
        .access_token()
        .ok_or_else(|| signed_out_reason(profile))?;
    Ok(Client::new(&profile.base_url)?
        .with_telemetry(config.telemetry())
        .with_source("cli")
        .with_token(token))
}
pub fn relay(config: &Config) -> Result<crate::relay::RelayClient> {
    Ok(crate::relay::RelayClient::new(
        &format!("http://127.0.0.1:{}/", config.relay_port),
        &config.relay_token,
    )?)
}

fn home_directory() -> Result<PathBuf> {
    let home = PathBuf::from(
        std::env::var_os("SILICON_HOME")
            .filter(|value| !value.is_empty())
            .or_else(|| std::env::var_os("HOME").filter(|value| !value.is_empty()))
            .context("SILICON_HOME or HOME must be set to keep DM's local state")?,
    );
    if !home.is_absolute() || !home.is_dir() {
        bail!(
            "home must be an existing absolute directory: {}",
            home.display()
        );
    }
    Ok(home)
}
/// Where this home's state lives, without creating anything. `None` when no home is set.
pub fn peek_directory() -> Option<PathBuf> {
    if let Some(directory) = std::env::var_os("SILICON_DM_HOME").filter(|v| !v.is_empty()) {
        let directory = PathBuf::from(directory);
        return directory.is_absolute().then_some(directory);
    }
    let home = home_directory().ok()?;
    let pointer = home.join(".silicon-dm").join("home_dir");
    if pointer.is_file()
        && !pointer.is_symlink()
        && let Ok(value) = fs::read_to_string(&pointer)
    {
        let configured = PathBuf::from(value.trim());
        if configured.is_absolute() {
            return Some(configured);
        }
    }
    Some(home.join(".silicon-dm"))
}
pub fn default_directory() -> Result<PathBuf> {
    let dir = if let Some(directory) = std::env::var_os("SILICON_DM_HOME").filter(|v| !v.is_empty())
    {
        PathBuf::from(directory)
    } else if let Some(directory) = configured_directory()? {
        directory
    } else {
        home_directory()?.join(".silicon-dm")
    };
    if !dir.is_absolute() {
        bail!("SILICON_DM_HOME must be an absolute path")
    }
    if dir.is_symlink() {
        bail!("refusing a symlink at {}", dir.display())
    }
    fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    Ok(dir)
}
fn configured_directory() -> Result<Option<PathBuf>> {
    let home = home_directory()?;
    let pointer = home.join(".silicon-dm").join("home_dir");
    if !pointer.exists() {
        return Ok(None);
    }
    if pointer.is_symlink() {
        bail!("refusing symlink for local DM home configuration");
    }
    let value = fs::read_to_string(pointer)?.trim().to_owned();
    if value.is_empty() {
        bail!("configured DM home directory is empty");
    }
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        bail!("configured DM home directory must be absolute");
    }
    Ok(Some(path))
}
pub fn set_home_directory(home: impl AsRef<Path>) -> Result<PathBuf> {
    let requested = home.as_ref();
    let owned;
    let home = if requested.is_absolute() {
        requested
    } else {
        owned = std::env::current_dir()?.join(requested);
        &owned
    };
    if !home.exists() {
        bail!("home directory does not exist");
    }
    if !home.is_dir() {
        bail!("not a directory: {}", home.display());
    }
    if home.is_symlink() {
        bail!("refusing a symlink for home directory");
    }
    let directory = home.join(".silicon-dm");
    fs::create_dir_all(&directory)?;
    #[cfg(unix)]
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
    let base = home_directory()?.join(".silicon-dm");
    fs::create_dir_all(&base)?;
    #[cfg(unix)]
    fs::set_permissions(&base, fs::Permissions::from_mode(0o700))?;
    let pointer = base.join("home_dir");
    let temporary = base.join(format!("home_dir.{}.tmp", Uuid::new_v4()));
    let mut file = secure_open(&temporary)?;
    file.write_all(directory.to_string_lossy().as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    drop(file);
    fs::rename(temporary, pointer)?;
    // Windows cannot open directories through File::open; the file was synced above.
    #[cfg(unix)]
    File::open(base)?.sync_all()?;
    Ok(directory)
}
/// This operating-system user's real home, from the account database (not `HOME`).
pub(crate) fn real_home() -> Result<PathBuf> {
    #[cfg(unix)]
    {
        // SAFETY: getpwuid returns a pointer into static storage or null; it is read at once.
        let entry = unsafe { libc::getpwuid(libc::getuid()) };
        if entry.is_null() {
            bail!("this user has no account database entry");
        }
        // SAFETY: pw_dir of a non-null entry is a NUL-terminated string.
        let directory = unsafe { std::ffi::CStr::from_ptr((*entry).pw_dir) }
            .to_str()
            .context("this user's home is not UTF-8")?;
        if directory.is_empty() {
            bail!("this user's account has no home directory");
        }
        Ok(PathBuf::from(directory))
    }
    #[cfg(not(unix))]
    {
        std::env::var_os("USERPROFILE")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .context("USERPROFILE is not set")
    }
}
/// Creates a private (0700, owned by this user, not a symbolic link) directory.
pub(crate) fn private_dir(path: &Path) -> Result<()> {
    if path.is_symlink() {
        bail!("refusing a symlink at {}", path.display());
    }
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = fs::symlink_metadata(path)?;
        // SAFETY: geteuid has no preconditions.
        if metadata.uid() != unsafe { libc::geteuid() } {
            bail!("{} belongs to another user", path.display());
        }
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}
/// One relay serves every DM home of this operating-system user, so its
/// directory ignores SILICON_HOME and HOME unless explicitly overridden.
pub fn relay_home_directory() -> Result<PathBuf> {
    let directory = match std::env::var_os("SILICON_DM_RELAY_HOME").filter(|v| !v.is_empty()) {
        Some(directory) => PathBuf::from(directory),
        None => real_home()
            .context("cannot resolve this user's home for the shared DM relay; set SILICON_DM_RELAY_HOME")?
            .join(".silicon-dm")
            .join("relay"),
    };
    if !directory.is_absolute() {
        bail!("SILICON_DM_RELAY_HOME must be an absolute path")
    }
    private_dir(&directory).with_context(|| {
        format!(
            "cannot create the private shared relay directory {}",
            directory.display()
        )
    })?;
    Ok(directory)
}
pub fn secure_open(path: &Path) -> Result<File> {
    if path.is_symlink() {
        bail!("refusing symlink for local DM state")
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    options.mode(0o600);
    let file = options.open(path)?;
    #[cfg(unix)]
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    Ok(file)
}

/// Explicit owner of one home's local state. The core HTTP client never opens it.
#[derive(Clone)]
pub struct Store {
    root: PathBuf,
    relay_home: Option<PathBuf>,
}
impl Store {
    pub fn new(directory: impl Into<PathBuf>) -> Result<Self> {
        let root = directory.into();
        if !root.is_absolute() || root.is_symlink() {
            bail!("DM state directory must be absolute and must not be a symlink");
        }
        fs::create_dir_all(&root)?;
        #[cfg(unix)]
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
        Ok(Self {
            root,
            relay_home: None,
        })
    }
    /// Uses a caller-owned shared relay directory instead of this user's default.
    /// Homes sharing it share one relay process.
    pub fn with_relay_home(mut self, directory: impl Into<PathBuf>) -> Result<Self> {
        let directory = directory.into();
        if !directory.is_absolute() {
            bail!("shared relay directory must be absolute");
        }
        private_dir(&directory).with_context(|| {
            format!(
                "cannot create the private shared relay directory {}",
                directory.display()
            )
        })?;
        self.relay_home = Some(directory);
        Ok(self)
    }
    pub fn relay_home(&self) -> Result<PathBuf> {
        self.relay_home
            .clone()
            .map_or_else(relay_home_directory, Ok)
    }
    pub fn from_environment() -> Result<Self> {
        Self::new(default_directory()?)
    }
    pub fn directory(&self) -> PathBuf {
        self.root.clone()
    }

    pub fn load(&self) -> Result<Config> {
        let path = self.root.join("config.json");
        if path.is_symlink() {
            bail!("refusing symlink for credential file")
        }
        match fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(config) => Ok(config),
                // Recovered under the lock (another process may be doing the same).
                Err(_) => self.update(|c| Ok(c.clone())),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.update(|c| Ok(c.clone()))
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Changes `config.json` under its lock and replaces it atomically. A file that cannot be
    /// read as DM state is kept as `config.json.unreadable-<time>` and replaced by defaults,
    /// so a damaged file never blocks signing in again.
    pub fn update<T>(&self, mutate: impl FnOnce(&mut Config) -> Result<T>) -> Result<T> {
        let root = &self.root;
        let lock = secure_open(&root.join("config.lock"))?;
        lock.lock_exclusive()?;
        let path = root.join("config.json");
        if path.is_symlink() {
            bail!("refusing symlink for credential file")
        }
        let mut config = match fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(config) => config,
                Err(error) => {
                    let aside = root.join(format!("config.json.unreadable-{}", now()));
                    fs::rename(&path, &aside)?;
                    eprintln!(
                        "DM state {} could not be read ({error}); it was kept as {} and DM starts from a fresh state: sign in again.",
                        path.display(),
                        aside.display()
                    );
                    Config::default()
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Config::default(),
            Err(error) => return Err(error.into()),
        };
        let result = mutate(&mut config)?;
        let temporary = root.join(format!("config.{}.tmp", Uuid::new_v4()));
        let mut file = secure_open(&temporary)?;
        file.write_all(&serde_json::to_vec_pretty(&config)?)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, &path)?;
        #[cfg(unix)]
        File::open(root)?.sync_all()?;
        FileExt::unlock(&lock)?;
        Ok(result)
    }

    // Serialize sign-in, refresh, and sign-out across processes without blocking a
    // Tokio executor thread while another holder is awaiting Silicon Accounts.
    pub(crate) async fn profile_lock(&self, key: &str) -> Result<File> {
        let lock_name = format!(
            "refresh-{}.lock",
            key.bytes().map(|b| format!("{b:02x}")).collect::<String>()
        );
        let lock = secure_open(&self.root.join(lock_name))?;
        loop {
            match lock.try_lock_exclusive() {
                Ok(()) => break,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await
                }
                Err(error) => return Err(error.into()),
            }
        }
        Ok(lock)
    }
    /// The profile at `key` with an access token valid for at least another minute.
    pub async fn fresh_profile(&self, key: &str) -> Result<(Config, Profile)> {
        self.fresh_profile_within(key, 60).await
    }
    /// Refreshes when the access token expires within `margin` seconds, one process at a
    /// time. The relay refreshes ahead of expiry with a wide margin so a send after a long
    /// idle period never waits for a refresh. If Silicon Accounts is briefly unreachable, a
    /// token with more than 10 seconds left is used anyway. A refresh token Silicon Accounts
    /// no longer accepts ends the sign-in: the tokens are forgotten and the reason kept.
    pub async fn fresh_profile_within(&self, key: &str, margin: u64) -> Result<(Config, Profile)> {
        let config = self.load()?;
        let profile = signed_in_at(&config, key)?;
        if profile.expires_at > now() + margin {
            return Ok((config, profile));
        }
        let lock = self.profile_lock(key).await?;
        let config = self.load()?;
        let current = signed_in_at(&config, key)?;
        if current.expires_at > now() + margin || !current.same_sign_in(&profile) {
            // Another process refreshed, signed in again or signed out meanwhile.
            drop(lock);
            let config = self.load()?;
            let fresh = signed_in_at(&config, key)?;
            return Ok((config, fresh));
        }
        let refresh_token = current
            .tokens
            .as_ref()
            .map(|tokens| tokens.refresh_token.clone())
            .unwrap_or_default();
        let client = accounts::PublicClient::new(
            &current.accounts_url,
            &current.app_id,
            config.telemetry(),
        )?;
        let outcome = client.refresh(&refresh_token).await;
        let result = match outcome {
            Ok(session) => {
                if let (Some(new), Some(old)) = (&session.account, &current.account)
                    && new.uuid != old.uuid
                {
                    bail!(
                        "Silicon Accounts refreshed the sign-in of another account ({}, not {}); the profile was left unchanged",
                        new.id,
                        old.id
                    );
                }
                self.update(|c| {
                    if let Some(stored) = c.profiles.get_mut(key)
                        && stored.same_sign_in(&current)
                    {
                        stored.apply_session(&session);
                    }
                    Ok(())
                })?;
                let config = self.load()?;
                let fresh = signed_in_at(&config, key)?;
                Ok((config, fresh))
            }
            Err(error) if error.sign_in_ended() => {
                let ended = EndedSignIn {
                    reason: "refresh_refused".into(),
                    message: error.message().to_owned(),
                    at: now(),
                };
                self.update(|c| {
                    if let Some(stored) = c.profiles.get_mut(key)
                        && stored.same_sign_in(&current)
                    {
                        stored.clear_sign_in(Some(ended.clone()));
                    }
                    Ok(())
                })?;
                Err(anyhow::Error::new(SessionError {
                    code: "sign_in_ended",
                    message: format!(
                        "The DM profile `{}` is signed out: Silicon Accounts no longer accepts its sign-in ({}).",
                        current.name,
                        error.message()
                    ),
                    hint: sign_in_hint(&current.name),
                }))
            }
            Err(error) if error.is_retryable() && current.expires_at > now() + 10 => {
                Ok((config, current))
            }
            Err(error) => Err(anyhow::Error::new(error)),
        };
        drop(lock);
        result
    }
}
fn signed_in_at(config: &Config, key: &str) -> Result<Profile> {
    let name = key.strip_suffix(":production").unwrap_or(key);
    profile(config, name).cloned()
}

/// Reads a home's state without creating or changing anything (for discovery commands that
/// must work in an empty home). `Ok(None)`: no home or no state yet.
pub fn peek(directory: &Path) -> Result<Option<Config>> {
    let path = directory.join("config.json");
    if path.is_symlink() {
        bail!("refusing symlink for credential file")
    }
    match fs::read(&path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes).with_context(|| {
            format!("DM state {} could not be read; the next dm command keeps it aside and starts fresh", path.display())
        })?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}
