//! Files of the one relay shared by every DM home of an operating-system user.
//!
//! `host.lock` is held by the running relay, `host.json` says where it listens
//! and carries its private attach token, and `stores.json` lists the homes it
//! serves so a restarted relay resumes every home's durable queue.
use super::store;
use anyhow::{Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};
use uuid::Uuid;

/// Ports tried after the preferred one before asking the OS for any free port.
const PORT_FALLBACKS: u16 = 100;

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct HostRecord {
    pub pid: u32,
    pub port: u16,
    pub version: String,
    /// Authorizes attaching homes and stopping the relay; never a home's bearer.
    pub token: String,
}

pub(crate) enum HostState {
    Stopped,
    /// Locked, but the record is not yet published.
    Starting,
    Running(HostRecord),
}

fn write_atomic(directory: &Path, name: &str, bytes: &[u8]) -> Result<()> {
    let temporary = directory.join(format!("{name}.{}.tmp", Uuid::new_v4()));
    let mut file = store::secure_open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temporary, directory.join(name))?;
    #[cfg(unix)]
    fs::File::open(directory)?.sync_all()?;
    Ok(())
}

pub(crate) fn lock_file(home: &Path) -> Result<fs::File> {
    store::secure_open(&home.join("host.lock"))
}

/// Probing takes the lock for an instant; a starting relay retries around it.
pub(crate) fn state(home: &Path) -> Result<HostState> {
    let lock = lock_file(home)?;
    if lock.try_lock_exclusive().is_ok() {
        FileExt::unlock(&lock)?;
        return Ok(HostState::Stopped);
    }
    let path = home.join("host.json");
    Ok(
        match fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        {
            Some(record) => HostState::Running(record),
            None => HostState::Starting,
        },
    )
}

pub(crate) async fn acquire(home: &Path) -> Result<fs::File> {
    let lock = lock_file(home)?;
    for _ in 0..20 {
        if lock.try_lock_exclusive().is_ok() {
            return Ok(lock);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    anyhow::bail!(
        "the shared DM relay is already running for this operating-system user ({}); run dm daemon status",
        home.display()
    )
}

pub(crate) fn publish(home: &Path, record: &HostRecord) -> Result<()> {
    write_atomic(home, "host.json", &serde_json::to_vec_pretty(record)?)
}

pub(crate) fn retract(home: &Path, pid: u32) {
    let path = home.join("host.json");
    let ours = fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<HostRecord>(&bytes).ok())
        .is_some_and(|record| record.pid == pid);
    if ours {
        let _ = fs::remove_file(path);
    }
}

#[derive(Default, Serialize, Deserialize)]
struct Registry {
    homes: Vec<PathBuf>,
}

fn update_registry<T>(home: &Path, mutate: impl FnOnce(&mut Registry) -> T) -> Result<T> {
    let lock = store::secure_open(&home.join("stores.lock"))?;
    lock.lock_exclusive()?;
    let path = home.join("stores.json");
    let mut registry: Registry = match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .with_context(|| format!("unreadable shared relay registry {}", path.display()))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Registry::default(),
        Err(error) => return Err(error.into()),
    };
    let before = serde_json::to_vec(&registry)?;
    let result = mutate(&mut registry);
    if serde_json::to_vec(&registry)? != before {
        write_atomic(home, "stores.json", &serde_json::to_vec_pretty(&registry)?)?;
    }
    FileExt::unlock(&lock)?;
    Ok(result)
}

/// Records a DM home so the shared relay serves it now and after restarts.
pub(crate) fn register(home: &Path, directory: &Path) -> Result<PathBuf> {
    let directory = fs::canonicalize(directory)?;
    update_registry(home, |registry| {
        if !registry.homes.contains(&directory) {
            registry.homes.push(directory.clone());
        }
    })?;
    Ok(directory)
}

/// Registered homes that still exist; deleted homes are forgotten.
pub(crate) fn registered(home: &Path) -> Result<Vec<PathBuf>> {
    update_registry(home, |registry| {
        registry.homes.retain(|directory| directory.is_dir());
        registry.homes.clone()
    })
}

fn unavailable(error: &std::io::Error) -> bool {
    // Windows reports excluded or reserved ports as permission failures.
    matches!(
        error.kind(),
        std::io::ErrorKind::AddrInUse | std::io::ErrorKind::PermissionDenied
    )
}

/// Binds loopback at the preferred port, else the next free one, else any port.
pub(crate) async fn bind(preferred: u16) -> Result<tokio::net::TcpListener> {
    let preferred = preferred.max(1);
    let last = preferred.saturating_add(PORT_FALLBACKS);
    for port in preferred..=last {
        match tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).await {
            Ok(listener) => return Ok(listener),
            Err(error) if unavailable(&error) => continue,
            Err(error) => {
                return Err(error).with_context(|| format!("cannot listen on 127.0.0.1:{port}"));
            }
        }
    }
    tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .with_context(|| {
            format!("ports {preferred}-{last} are in use and no other loopback port is available")
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_busy_preferred_port_falls_back_to_the_next_free_one() -> Result<()> {
        let occupied = std::net::TcpListener::bind("127.0.0.1:0")?;
        let busy = occupied.local_addr()?.port();
        let listener = bind(busy).await?;
        assert_ne!(listener.local_addr()?.port(), busy);
        // The top of the range cannot overflow into an invalid port.
        let listener = bind(u16::MAX).await?;
        assert_ne!(listener.local_addr()?.port(), 0);
        Ok(())
    }

    #[test]
    fn registry_deduplicates_and_forgets_deleted_homes() -> Result<()> {
        let root = std::env::temp_dir().join(format!("dm-host-registry-{}", Uuid::new_v4()));
        let home = root.join("relay");
        let (first, second) = (root.join("first"), root.join("second"));
        for directory in [&home, &first, &second] {
            fs::create_dir_all(directory)?;
        }
        register(&home, &first)?;
        register(&home, &first)?;
        register(&home, &second)?;
        assert_eq!(registered(&home)?.len(), 2);
        fs::remove_dir_all(&second)?;
        assert_eq!(registered(&home)?, vec![fs::canonicalize(&first)?]);
        fs::remove_dir_all(root)?;
        Ok(())
    }
}
