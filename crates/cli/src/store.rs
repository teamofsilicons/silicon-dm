use anyhow::Result;
use silicon_dm_client::runtime::store::Store;
pub use silicon_dm_client::runtime::store::{
    Config, Profile, TestKey, client, profile, relay, session_key,
};
pub fn load() -> Result<Config> {
    Store::from_environment()?.load()
}
pub fn update<T>(mutate: impl FnOnce(&mut Config) -> Result<T>) -> Result<T> {
    Store::from_environment()?.update(mutate)
}
pub async fn fresh_profile(key: &str) -> Result<(Config, Profile)> {
    Store::from_environment()?.fresh_profile(key).await
}
