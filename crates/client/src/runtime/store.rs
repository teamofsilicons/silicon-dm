use crate::{Client, Tokens};
use anyhow::{Context, Result, bail};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
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

#[derive(Clone, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    pub base_url: String,
    pub tokens: Tokens,
    #[serde(default)]
    /// Legacy outgoing command-response callback only; never incoming DM delivery.
    pub webhook_url: Option<String>,
    pub device_id: String,
    pub expires_at: u64,
    #[serde(default)]
    pub refresh_started_at: Option<u64>,
    #[serde(default)]
    pub testing_environment_id: Option<Uuid>,
    /// Local opt-in state; logging out retains durable inbox/outbox records.
    #[serde(default = "enabled")]
    pub enabled: bool,
}
fn enabled() -> bool {
    true
}
#[derive(Clone, Serialize, Deserialize)]
pub struct TestKey {
    pub key: String,
    pub base_url: String,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Config {
    pub default_profile: String,
    pub profiles: BTreeMap<String, Profile>,
    pub testing_keys: BTreeMap<Uuid, TestKey>,
    #[serde(default)]
    pub testing_names: BTreeMap<Uuid, String>,
    #[serde(default)]
    pub webhook_secrets: BTreeMap<String, String>,
    /// Port of the shared relay serving this home. The relay rewrites it when it
    /// attaches the home, and prefers it when it starts, so it stays stable.
    pub relay_port: u16,
    /// This home's private bearer; the shared relay routes requests by it.
    pub relay_token: String,
    #[serde(default)]
    pub auto_update: bool,
    #[serde(default = "enabled")]
    pub telemetry_enabled: bool,
    #[serde(default)]
    pub last_update_check: u64,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            default_profile: "default".into(),
            profiles: BTreeMap::new(),
            testing_keys: BTreeMap::new(),
            testing_names: BTreeMap::new(),
            webhook_secrets: BTreeMap::new(),
            relay_port: 19780,
            relay_token: format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple()),
            auto_update: false,
            telemetry_enabled: true,
            last_update_check: 0,
        }
    }
}
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn home_directory() -> Result<PathBuf> {
    let home = PathBuf::from(
        std::env::var_os("SILICON_HOME")
            .or_else(|| std::env::var_os("HOME"))
            .context("SILICON_HOME or HOME must be set")?,
    );
    if !home.is_absolute() || !home.is_dir() {
        bail!(
            "home must be an existing absolute directory: {}",
            home.display()
        );
    }
    Ok(home)
}
pub fn default_directory() -> Result<PathBuf> {
    let dir = if let Some(directory) = std::env::var_os("SILICON_DM_HOME") {
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
        bail!("refusing a symlink at ~/.silicon-dm")
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
/// One relay serves every DM home of this operating-system user, so its
/// directory ignores SILICON_HOME and HOME unless explicitly overridden.
pub fn relay_home_directory() -> Result<PathBuf> {
    let directory = match std::env::var_os("SILICON_DM_RELAY_HOME") {
        Some(directory) => PathBuf::from(directory),
        None => ting_client::real_home()
            .map_err(|_| anyhow::anyhow!("cannot resolve this user's home for the shared DM relay; set SILICON_DM_RELAY_HOME"))?
            .join(".silicon-dm")
            .join("relay"),
    };
    if !directory.is_absolute() {
        bail!("SILICON_DM_RELAY_HOME must be an absolute path")
    }
    ting_client::private_dir(&directory).map_err(|_| {
        anyhow::anyhow!(
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
impl Store {
    pub fn load(&self) -> Result<Config> {
        let root = self.directory();
        let path = root.join("config.json");
        if !path.exists() {
            return self.update(|c| Ok(c.clone()));
        }
        if path.is_symlink() {
            bail!("refusing symlink for credential file")
        }
        Ok(serde_json::from_slice(&fs::read(path)?)?)
    }
}
impl Store {
    pub fn update<T>(&self, mutate: impl FnOnce(&mut Config) -> Result<T>) -> Result<T> {
        let root = self.directory();
        let lock = secure_open(&root.join("config.lock"))?;
        lock.lock_exclusive()?;
        let path = root.join("config.json");
        if path.is_symlink() {
            bail!("refusing symlink for credential file")
        }
        let mut config = if path.exists() {
            serde_json::from_slice(&fs::read(&path)?)?
        } else {
            Config::default()
        };
        let result = mutate(&mut config)?;
        let temporary = root.join(format!("config.{}.tmp", Uuid::new_v4()));
        let mut file = secure_open(&temporary)?;
        file.write_all(&serde_json::to_vec_pretty(&config)?)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, &path)?;
        #[cfg(unix)]
        File::open(&root)?.sync_all()?;
        FileExt::unlock(&lock)?;
        Ok(result)
    }
}
pub fn session_key(name: &str, test: Option<Uuid>) -> String {
    format!(
        "{name}:{}",
        test.map_or("production".into(), |id| id.to_string())
    )
}
pub fn profile<'a>(config: &'a Config, name: &str, test: Option<Uuid>) -> Result<&'a Profile> {
    config.profiles.get(&session_key(name,test)).filter(|p|p.enabled).with_context(||format!("profile {name} is not logged in for {}; run dm {}--profile {name} login --token-file -",test.map_or("production".into(),|id|id.to_string()),test.map_or(String::new(),|id|format!("--test {id} "))))
}
pub fn client(config: &Config, profile: &Profile) -> Result<Client> {
    let mut client = Client::new(&profile.base_url)?
        .with_telemetry(
            config.telemetry_enabled
                && std::env::var("DM_TELEMETRY_ENABLED").as_deref() != Ok("false"),
        )
        .with_source("cli")
        .with_auth(
            &profile.tokens.access_token,
            &profile.tokens.organization_id,
        );
    if let Ok(limit) = std::env::var("DM_CLIENT_MAX_FRAME_BYTES") {
        client = client.with_websocket_limit(
            limit
                .parse()
                .context("DM_CLIENT_MAX_FRAME_BYTES must be an integer")?,
        )?;
    }
    if let Some(id) = profile.testing_environment_id {
        let key = config
            .testing_keys
            .get(&id)
            .context("test key is missing; run dm environments import-key")?;
        if Client::new(&key.base_url).is_err()
            || key.base_url.trim_end_matches('/') != profile.base_url.trim_end_matches('/')
        {
            bail!("testing key belongs to a different backend URL")
        }
        client = client.with_test_key(&key.key)?;
    }
    Ok(client)
}
pub fn relay(config: &Config) -> Result<crate::relay::RelayClient> {
    Ok(crate::relay::RelayClient::new(
        &format!("http://127.0.0.1:{}/", config.relay_port),
        &config.relay_token,
    )?)
}
impl Store {
    // Serialize login, refresh, and logout across processes without blocking a
    // Tokio executor thread while another request is awaiting the backend.
    pub(crate) async fn profile_lock(&self, key: &str) -> Result<File> {
        let lock_name = format!(
            "refresh-{}.lock",
            key.bytes().map(|b| format!("{b:02x}")).collect::<String>()
        );
        let lock = secure_open(&self.directory().join(lock_name))?;
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
    pub async fn fresh_profile(&self, key: &str) -> Result<(Config, Profile)> {
        self.fresh_profile_within(key, 60, None).await
    }
    /// Refreshes when the access token expires within `margin` seconds. The
    /// relay refreshes ahead of expiry with a wide margin so a send after a long
    /// idle period never waits for a refresh, and passes its pool so the refresh
    /// reuses the open connection.
    pub(crate) async fn fresh_profile_within(
        &self,
        key: &str,
        margin: u64,
        http: Option<&reqwest::Client>,
    ) -> Result<(Config, Profile)> {
        let config = self.load()?;
        let profile = config
            .profiles
            .get(key)
            .filter(|p| p.enabled)
            .context("profile is logged out")?
            .clone();
        if profile.expires_at > now() + margin && profile.refresh_started_at.is_none() {
            return Ok((config, profile));
        }
        let lock = self.profile_lock(key).await?;
        for _ in 0..2 {
            let config = self.load()?;
            let profile = config
                .profiles
                .get(key)
                .filter(|p| p.enabled)
                .context("profile is logged out")?
                .clone();
            if profile.expires_at > now() + margin && profile.refresh_started_at.is_none() {
                return Ok((config, profile));
            }
            let started_at = profile.refresh_started_at.unwrap_or_else(now);
            self.update(|c| {
                if let Some(current) = c.profiles.get_mut(key)
                    && current.tokens.refresh_token == profile.tokens.refresh_token
                {
                    current.refresh_started_at = Some(started_at);
                }
                Ok(())
            })?;
            // The key is stable across a lost response and process restart.
            let refresh_key = format!(
                "dm-refresh-{}",
                blake3::hash(profile.tokens.refresh_token.as_bytes())
            );
            let mut refresher = client(&config, &profile)?;
            if let Some(http) = http {
                refresher = refresher.with_http_client(http.clone());
            }
            let tokens = refresher
                .refresh(&profile.tokens.refresh_token, &refresh_key)
                .await?;
            let expires_at = started_at.saturating_add(tokens.expires_in.max(0) as u64);
            self.update(|c| {
                if let Some(current) = c.profiles.get_mut(key)
                    && current.tokens.refresh_token == profile.tokens.refresh_token
                {
                    current.expires_at = expires_at;
                    current.tokens = tokens.clone();
                    current.refresh_started_at = None;
                }
                Ok(())
            })?;
            let config = self.load()?;
            let profile = config.profiles.get(key).context("profile removed")?.clone();
            if profile.expires_at > now() + 60 {
                FileExt::unlock(&lock)?;
                return Ok((config, profile));
            }
        }
        bail!("refreshed access token has no usable lifetime; retry the command")
    }
}

/// Explicit owner of local relay state. The core HTTP client never opens it.
#[derive(Clone)]
pub struct Store {
    root: PathBuf,
    relay_home: Option<PathBuf>,
}
impl Store {
    pub fn new(directory: impl Into<PathBuf>) -> Result<Self> {
        let root = directory.into();
        if !root.is_absolute() || root.is_symlink() {
            bail!("relay state directory must be absolute and must not be a symlink");
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
        ting_client::private_dir(&directory).map_err(|_| {
            anyhow::anyhow!(
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

    /// A Ting-owned private profile tied to the selected DM profile and generation.
    /// Ting stores its opaque session here; its daemon owns webhook queues and URLs.
    pub(crate) fn ting_profile(
        &self,
        session: &str,
        generation: Option<i64>,
    ) -> Result<ting_client::Profile> {
        let scope = blake3::hash(format!("{session}:{generation:?}").as_bytes());
        let directory = self.directory().join("ting");
        ting_client::private_dir(&directory)?;
        let directory = directory.join(scope.to_hex().as_str());
        ting_client::private_dir(&directory)?;
        Ok(ting_client::Profile {
            dir: fs::canonicalize(directory)?,
        })
    }
}
