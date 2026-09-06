//! Optional updater execution with caller-owned policy and explicit build targets.
use serde::{Deserialize, Serialize};
/// CLI update controls, shared by Rust consumers and the command line interface.
pub enum UpdateCommand {
    Status,
    Enable,
    Disable,
    Check,
    Install,
}
use super::store;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{path::PathBuf, time::Duration};

async fn release() -> Result<String> {
    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .user_agent(concat!("silicon-dm-cli/", env!("CARGO_PKG_VERSION")))
        .build()?
        .get("https://crates.io/api/v1/crates/silicon-dm-cli")
        .send()
        .await?;
    let value = response.error_for_status()?.json::<Value>().await?;
    let version = value
        .pointer("/crate/max_stable_version")
        .and_then(Value::as_str)
        .context("no stable CLI version published")?;
    semver::Version::parse(version)?;
    Ok(version.into())
}
fn installed_binary() -> Result<bool> {
    let cargo_root = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cargo")
        });
    let path = cargo_root
        .join("bin")
        .join(if cfg!(windows) { "dm.exe" } else { "dm" });
    Ok(path.exists() && path.canonicalize()? == std::env::current_exe()?.canonicalize()?)
}
async fn install(version: &str) -> Result<Value> {
    if !installed_binary()? {
        return Ok(
            json!({"installed":false,"available_version":version,"reason":"This executable is a development/custom build. Install from crates.io into Cargo's bin directory to enable replacement.","command":format!("cargo install silicon-dm-cli --version {version} --locked")}),
        );
    }
    let status = tokio::process::Command::new("cargo")
        .args([
            "install",
            "silicon-dm-cli",
            "--version",
            version,
            "--locked",
            "--force",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(std::io::stderr()))
        .stderr(std::process::Stdio::inherit())
        .status()
        .await
        .context("cargo must be installed to update the CLI")?;
    if !status.success() {
        bail!("CLI update failed; the existing executable remains usable")
    }
    Ok(
        json!({"installed":true,"version":version,"next":"New invocations use this version. Restart the daemon when convenient; pending work is persisted."}),
    )
}
pub async fn command(
    state: &store::Store,
    command: UpdateCommand,
    current_version: &str,
) -> Result<Value> {
    match command {
        UpdateCommand::Status => {
            let config = state.load()?;
            Ok(
                json!({"auto_update":config.auto_update,"last_update_check":config.last_update_check,"interval_seconds":3600,"current_version":current_version,"can_replace_running_binary":installed_binary()?}),
            )
        }
        UpdateCommand::Enable => {
            state.update(|c| {
                c.auto_update = true;
                Ok(())
            })?;
            Ok(json!({"auto_update":true}))
        }
        UpdateCommand::Disable => {
            state.update(|c| {
                c.auto_update = false;
                Ok(())
            })?;
            Ok(json!({"auto_update":false}))
        }
        UpdateCommand::Check => {
            let latest = release().await?;
            state.update(|c| {
                c.last_update_check = store::now();
                Ok(())
            })?;
            Ok(
                json!({"package":"silicon-dm-cli","current_version":current_version,"latest_version":latest,"library":"Linked Rust clients must update their dependency and rebuild; call silicon_dm_client::check_update for library release information."}),
            )
        }
        UpdateCommand::Install => {
            let latest = release().await?;
            install(&latest).await
        }
    }
}
pub async fn automatic(state: &store::Store, current_version: &str) {
    let check = state
        .update(|config| {
            let due =
                config.auto_update && store::now().saturating_sub(config.last_update_check) >= 3600;
            if due {
                config.last_update_check = store::now()
            }
            Ok(due)
        })
        .unwrap_or(false);
    if !check {
        return;
    }
    // Registry outages and unpublished packages never make an otherwise successful command fail.
    if let Ok(latest) = release().await
        && let (Ok(latest_version), Ok(current)) = (
            semver::Version::parse(&latest),
            semver::Version::parse(current_version),
        )
        && latest_version > current
    {
        match install(&latest).await {
            Ok(result) => eprintln!("DM update: {result}"),
            Err(_) => eprintln!("DM update deferred; retry with dm updates install."),
        }
    }
}

/// Serializable caller-owned hourly policy. No SDK global state is read/written.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UpdatePolicy {
    pub enabled: bool,
    pub last_checked_at: u64,
}
impl Default for UpdatePolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            last_checked_at: 0,
        }
    }
}
impl UpdatePolicy {
    /// Claim the next check before awaiting network I/O. Persist the updated
    /// policy if checks must remain hourly across application restarts.
    pub fn claim_due(&mut self, now: u64) -> bool {
        if !self.enabled || now.saturating_sub(self.last_checked_at) < 3600 {
            return false;
        }
        self.last_checked_at = now;
        true
    }
}
/// Explicit application manifest whose SDK dependency may be updated/rebuilt.
pub struct DependencyTarget {
    pub manifest_path: PathBuf,
}
/// Run after the caller's command finishes. Updates only the explicitly supplied
/// Cargo project. Running library code is unchanged until the application restarts.
/// The caller persists `policy` even if the registry or build fails.
pub async fn after_command(policy: &mut UpdatePolicy, target: &DependencyTarget) -> Result<Value> {
    if !policy.claim_due(store::now()) {
        return Ok(json!({"checked":false,"reason":"disabled_or_not_due"}));
    }
    let info = crate::check_update().await?;
    let latest = semver::Version::parse(&info.latest_version)?;
    let current = semver::Version::parse(&info.current_version)?;
    if latest <= current {
        return Ok(json!({"checked":true,"updated":false,"latest_version":info.latest_version}));
    }
    if !target.manifest_path.is_absolute() || !target.manifest_path.is_file() {
        bail!("dependency update target must be an existing absolute Cargo.toml path");
    }
    let manifest = target.manifest_path.canonicalize()?;
    let update = tokio::process::Command::new("cargo")
        .args(["update", "--manifest-path"])
        .arg(&manifest)
        .args([
            "--package",
            "silicon-dm-client",
            "--precise",
            &info.latest_version,
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(std::io::stderr()))
        .stderr(std::process::Stdio::inherit())
        .status()
        .await?;
    if !update.success() {
        bail!("SDK dependency update failed; inspect the manifest's compatible version range");
    }
    let build = tokio::process::Command::new("cargo")
        .args(["build", "--release", "--locked", "--manifest-path"])
        .arg(&manifest)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(std::io::stderr()))
        .stderr(std::process::Stdio::inherit())
        .status()
        .await?;
    if !build.success() {
        bail!(
            "SDK dependency was updated but the application rebuild failed; the currently running application is unchanged"
        );
    }
    Ok(json!({"checked":true,"updated":true,"version":info.latest_version,"restart_required":true}))
}
