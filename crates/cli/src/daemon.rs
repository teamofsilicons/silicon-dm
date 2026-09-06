use anyhow::Result;
use serde_json::Value;
use silicon_dm_client::runtime::{DaemonCommand, LocalRuntime};
pub async fn start(port: Option<u16>) -> Result<Value> {
    LocalRuntime::from_environment()?
        .start_with(
            &DaemonCommand {
                executable: std::env::current_exe()?,
                arguments: vec!["daemon".into(), "run".into()],
            },
            port,
        )
        .await
}
pub async fn run() -> Result<()> {
    LocalRuntime::from_environment()?.run().await
}
