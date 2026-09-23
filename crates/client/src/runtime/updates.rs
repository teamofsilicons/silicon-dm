//! Compatibility controls for Honeycomb-owned CLI updates. No runtime replaces binaries.
use super::store;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;

/// Legacy CLI controls retained with explicit Honeycomb guidance.
pub enum UpdateCommand {
    Status,
    Enable,
    Disable,
    Check,
    Install,
}

pub async fn command(
    state: &store::Store,
    command: UpdateCommand,
    current_version: &str,
) -> Result<Value> {
    if matches!(command, UpdateCommand::Disable) {
        state.update(|config| {
            config.auto_update = false;
            Ok(())
        })?;
    }
    Ok(
        json!({"manager":"honeycomb","app_id":"dm","current_version":current_version,
        "auto_update":false,"can_replace_running_binary":false,
        "command":"honeycomb install 'dm'",
        "message":"Honeycomb manages CLI installation and updates. Configure update policy in Honeycomb. Rust dependencies are updated through the consuming project."}),
    )
}

/// Compatibility no-op; DM never checks or installs releases automatically.
pub async fn automatic(_state: &store::Store, _current_version: &str) {}

/// Legacy serialized policy; runtime dependency updates are disabled.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct UpdatePolicy {
    pub enabled: bool,
    pub last_checked_at: u64,
}
impl UpdatePolicy {
    pub fn claim_due(&mut self, _now: u64) -> bool {
        false
    }
}
/// Legacy API input retained for source compatibility; the file is never accessed.
pub struct DependencyTarget {
    pub manifest_path: PathBuf,
}
/// Compatibility no-op. Manage Rust dependency versions in the consuming project.
pub async fn after_command(
    _policy: &mut UpdatePolicy,
    _target: &DependencyTarget,
) -> Result<Value> {
    Ok(json!({"checked":false,"updated":false,"reason":"project_managed_dependency"}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn legacy_enabled_policy_cannot_update_a_project() {
        let mut policy = UpdatePolicy {
            enabled: true,
            last_checked_at: 0,
        };
        assert!(!policy.claim_due(u64::MAX));
        let result = after_command(
            &mut policy,
            &DependencyTarget {
                manifest_path: PathBuf::from("/does-not-exist/Cargo.toml"),
            },
        )
        .await
        .unwrap();
        assert_eq!(result["updated"], false);
        assert_eq!(policy.last_checked_at, 0);
    }
}
