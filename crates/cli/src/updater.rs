use crate::Updates;
use anyhow::Result;
use serde_json::Value;
use silicon_dm_client::runtime::{
    store::Store,
    updates::{self, UpdateCommand},
};
pub async fn command(command: Updates) -> Result<Value> {
    let command = match command {
        Updates::Status => UpdateCommand::Status,
        Updates::Enable => UpdateCommand::Enable,
        Updates::Disable => UpdateCommand::Disable,
        Updates::Check => UpdateCommand::Check,
        Updates::Install => UpdateCommand::Install,
    };
    updates::command(
        &Store::from_environment()?,
        command,
        env!("CARGO_PKG_VERSION"),
    )
    .await
}
pub async fn automatic() {
    if let Ok(state) = Store::from_environment() {
        updates::automatic(&state, env!("CARGO_PKG_VERSION")).await;
    }
}
