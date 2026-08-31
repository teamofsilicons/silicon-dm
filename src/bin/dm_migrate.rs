//! Silicon DM schema migration process.

use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    dotenvy::dotenv().ok();
    let settings = match silicon_dm::config::MigrationSettings::from_env() {
        Ok(settings) => settings,
        Err(error) => {
            eprintln!("DM migration configuration is invalid: {error}");
            return ExitCode::FAILURE;
        }
    };
    if silicon_dm::telemetry::init(settings.environment, &settings.log_filter).is_err() {
        eprintln!("DM migration telemetry initialization failed");
        return ExitCode::FAILURE;
    }

    match silicon_dm::bootstrap::run_migrations(settings).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(error = %error, "DM migration failed");
            ExitCode::FAILURE
        }
    }
}
