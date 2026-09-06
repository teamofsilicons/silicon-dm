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
    pub webhook_url: String,
    pub device_id: String,
    pub expires_at: u64,
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
    pub relay_port: u16,
    pub relay_token: String,
    #[serde(default = "enabled")]
    pub auto_update: bool,
    #[serde(default)]
    pub last_update_check: u64,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            default_profile: "default".into(),
            profiles: BTreeMap::new(),
            testing_keys: BTreeMap::new(),
            relay_port: 19780,
            relay_token: format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple()),
            auto_update: true,
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
pub fn default_directory() -> Result<PathBuf> {
    let dir = if let Some(directory) = std::env::var_os("SILICON_DM_HOME") {
        PathBuf::from(directory)
    } else {
        PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?).join(".silicon-dm")
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
        fs::rename(&temporary, &path)?;
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
    config.profiles.get(&session_key(name,test)).filter(|p|p.enabled).with_context(||format!("profile {name} is not logged in for {}; run dm {}--profile {name} login --webhook URL --token-file -",test.map_or("production".into(),|id|id.to_string()),test.map_or(String::new(),|id|format!("--test {id} "))))
}
pub fn client(config: &Config, profile: &Profile) -> Result<Client> {
    let mut client = Client::new(&profile.base_url)?.with_auth(
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
        let config = self.load()?;
        let profile = config
            .profiles
            .get(key)
            .filter(|p| p.enabled)
            .context("profile is logged out")?
            .clone();
        if profile.expires_at > now() + 60 {
            return Ok((config, profile));
        }
        let lock = self.profile_lock(key).await?;
        let config = self.load()?;
        let profile = config
            .profiles
            .get(key)
            .filter(|p| p.enabled)
            .context("profile is logged out")?
            .clone();
        if profile.expires_at > now() + 60 {
            return Ok((config, profile));
        }
        // The key is stable across a lost response and process restart.
        let refresh_key = format!(
            "dm-refresh-{}",
            blake3::hash(profile.tokens.refresh_token.as_bytes())
        );
        let tokens = client(&config, &profile)?
            .refresh(&profile.tokens.refresh_token, &refresh_key)
            .await?;
        let expires_at = now().saturating_add(tokens.expires_in.max(0) as u64);
        self.update(|c| {
            if let Some(current) = c.profiles.get_mut(key)
                && current.tokens.refresh_token == profile.tokens.refresh_token
            {
                current.expires_at = expires_at;
                current.tokens = tokens.clone();
            }
            Ok(())
        })?;
        FileExt::unlock(&lock)?;
        let config = self.load()?;
        let profile = config.profiles.get(key).context("profile removed")?.clone();
        Ok((config, profile))
    }
}

/// Explicit owner of local relay state. The core HTTP client never opens it.
#[derive(Clone)]
pub struct Store {
    root: PathBuf,
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
        Ok(Self { root })
    }
    pub fn from_environment() -> Result<Self> {
        Self::new(default_directory()?)
    }
    pub fn directory(&self) -> PathBuf {
        self.root.clone()
    }
}
