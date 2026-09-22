//! Explicit DM consent and direct configuration of Ting's receiver runtime.
use anyhow::{Context, Result, bail, ensure};
use clap::Subcommand;
use serde_json::{Value, json};
use silicon_dm_client::runtime::{DeliveryLoginOptions, DeliveryTestCredentials, LocalRuntime};
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Subcommand)]
pub enum Command {
    /// Log in separately to Ting with a Ting-bound SLT; never a DM token.
    #[command(
        after_help = "Secrets are accepted only through files, stdin, or the private DM_TING_TEST_APP_SECRET / DM_TING_TEST_ENVIRONMENT_KEY environment pair. At most one input can read stdin. This command neither registers DM consent nor attaches a destination."
    )]
    Login {
        /// File containing a fresh Ting-bound SLT; '-' reads hidden stdin.
        #[arg(long, default_value = "-")]
        token_file: PathBuf,
        /// Ting API origin, never DM's API origin.
        #[arg(
            long,
            env = "DM_TING_API_URL",
            default_value = "https://backend.ting.teamofsilicons.com"
        )]
        base_url: String,
        /// Ting audience's test app secret, paired with the IAM environment key and --test.
        #[arg(long)]
        ting_app_secret_file: Option<PathBuf>,
        /// Shared IAM test environment key; requires Ting's audience app secret and --test.
        #[arg(long)]
        ting_environment_key_file: Option<PathBuf>,
    },
    /// Verify this bound Ting login and ask its system daemon for status.
    Status,
    /// Explicitly register this DM recipient's IAM-consented grant in Ting.
    Register,
    /// Explicitly reconnect the retained Ting destination; do not create another hook.
    Reconnect,
    /// Stop forwarding and end only this profile's separate Ting login.
    Logout,
}

const APP_SECRET_ENV: &str = "DM_TING_TEST_APP_SECRET";
const ENVIRONMENT_KEY_ENV: &str = "DM_TING_TEST_ENVIRONMENT_KEY";

pub fn validate(command: &Command, testing: bool) -> Result<()> {
    let Command::Login {
        ting_app_secret_file,
        ting_environment_key_file,
        ..
    } = command
    else {
        return Ok(());
    };
    let app_env = std::env::var_os(APP_SECRET_ENV).is_some();
    let key_env = std::env::var_os(ENVIRONMENT_KEY_ENV).is_some();
    ensure!(
        !(ting_app_secret_file.is_some() && app_env),
        "choose --ting-app-secret-file or {APP_SECRET_ENV}, not both"
    );
    ensure!(
        !(ting_environment_key_file.is_some() && key_env),
        "choose --ting-environment-key-file or {ENVIRONMENT_KEY_ENV}, not both"
    );
    let app = ting_app_secret_file.is_some() || app_env;
    let key = ting_environment_key_file.is_some() || key_env;
    ensure!(
        app == key,
        "Ting testing requires both its app secret and the IAM environment key"
    );
    ensure!(
        testing == app,
        "Ting testing credentials require a selected DM --test environment (or DM app-secret selection); production login must omit them"
    );
    Ok(())
}

fn secret(file: Option<&std::path::Path>, environment: &str) -> Result<Option<String>> {
    if let Some(file) = file {
        return super::read_secret(file).map(Some);
    }
    match std::env::var(environment) {
        Ok(value) if !value.trim().is_empty() => Ok(Some(value.trim().to_owned())),
        Ok(_) => bail!("{environment} was empty"),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(_) => bail!("{environment} must contain UTF-8 text"),
    }
}

pub async fn run(
    command: Command,
    name: &str,
    test: Option<Uuid>,
    key: &str,
    explicit_key: bool,
) -> Result<Value> {
    validate(&command, test.is_some())?;
    let runtime = LocalRuntime::from_environment()?;
    match command {
        Command::Login {
            token_file,
            base_url,
            ting_app_secret_file,
            ting_environment_key_file,
        } => {
            let app_secret = secret(ting_app_secret_file.as_deref(), APP_SECRET_ENV)?;
            let environment_key =
                secret(ting_environment_key_file.as_deref(), ENVIRONMENT_KEY_ENV)?;
            let token = super::read_secret(&token_file)?;
            let retry_key = if explicit_key {
                key.to_owned()
            } else {
                format!("dm-ting-login-{}", blake3::hash(token.as_bytes()))
            };
            let testing = match (app_secret.as_deref(), environment_key.as_deref()) {
                (Some(app_secret), Some(environment_key)) => Some(DeliveryTestCredentials {
                    app_secret,
                    environment_key,
                }),
                _ => None,
            };
            let mut result = runtime
                .delivery_login(&DeliveryLoginOptions {
                    profile: name,
                    testing_environment_id: test,
                    ting_api_url: &base_url,
                    short_lived_token: &token,
                    idempotency_key: &retry_key,
                    testing,
                })
                .await?;
            result["idempotency_key"] = json!(retry_key);
            eprintln!(
                "Logged in to Ting. Configure a generic endpoint with dm webhook URL --all-apps. DM consent remains explicit: dm delivery register."
            );
            Ok(result)
        }
        Command::Status => runtime.delivery_status(name, test).await,
        Command::Reconnect => runtime.delivery_reconnect(name, test).await,
        Command::Logout => runtime.delivery_logout(name, test).await,
        Command::Register => {
            // Print the safe retry key before I/O so an uncertain response cannot
            // tempt a caller to silently re-enroll with another generated key.
            eprintln!("Delivery registration idempotency key: {key}");
            let session = super::store::session_key(name, test);
            let (config, profile) = super::store::fresh_profile(&session).await?;
            let mut client = super::store::client(&config, &profile)?;
            let info = client.iam().await?;
            ensure!(
                info.testing_environment_id == test,
                "DM discovery returned another environment; delivery consent was not changed"
            );
            if test.is_some() {
                let generation = info
                    .testing_generation
                    .filter(|generation| *generation > 0)
                    .context("DM did not disclose the selected testing generation")?;
                client = client.with_testing_generation(generation)?;
            } else {
                ensure!(
                    info.testing_generation.is_none(),
                    "DM returned an unexpected test generation for production"
                );
            }
            let subscription = client.register_delivery(key).await?;
            Ok(json!({"subscription":subscription,"idempotency_key":key}))
        }
    }
}
