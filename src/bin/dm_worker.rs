//! Silicon DM durable delivery worker process.

use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    dotenvy::dotenv().ok();
    let settings = match silicon_dm::Settings::from_env() {
        Ok(settings) => settings,
        Err(error) => {
            eprintln!("DM worker configuration is invalid: {error}");
            return ExitCode::FAILURE;
        }
    };
    if silicon_dm::telemetry::init(settings.environment, &settings.log_filter).is_err() {
        eprintln!("DM worker telemetry initialization failed");
        return ExitCode::FAILURE;
    }

    match Box::pin(silicon_dm::bootstrap::run_worker(settings)).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(error = %error, "DM delivery worker failed");
            ExitCode::FAILURE
        }
    }
}
