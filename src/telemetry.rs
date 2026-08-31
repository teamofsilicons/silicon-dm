//! Structured tracing initialization.

use tracing_subscriber::{EnvFilter, layer::SubscriberExt as _, util::SubscriberInitExt as _};

use crate::config::RuntimeEnvironment;

/// Installs the process-global tracing subscriber.
///
/// # Errors
///
/// Returns an error for an invalid filter or a subscriber that was already set.
pub fn init(environment: RuntimeEnvironment, filter: &str) -> anyhow::Result<()> {
    let env_filter = EnvFilter::try_new(filter)?;
    let registry = tracing_subscriber::registry().with(env_filter);

    if environment == RuntimeEnvironment::Production {
        registry
            .with(tracing_subscriber::fmt::layer().json())
            .try_init()?;
    } else {
        registry
            .with(tracing_subscriber::fmt::layer().compact())
            .try_init()?;
    }
    Ok(())
}
