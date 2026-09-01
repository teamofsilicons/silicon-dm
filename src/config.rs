//! Typed, validated runtime configuration.

use std::{
    env,
    net::SocketAddr,
    num::{NonZeroU32, NonZeroUsize},
    str::FromStr,
    time::Duration,
};

use secrecy::{ExposeSecret as _, SecretString};
use thiserror::Error;
use url::Url;

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_HTTP_BODY_BYTES: usize = 128 * 1024 * 1024;

/// Complete process settings.
#[derive(Clone, Debug)]
pub struct Settings {
    /// Runtime safety mode.
    pub environment: RuntimeEnvironment,
    /// Inbound HTTP settings.
    pub server: ServerSettings,
    /// PostgreSQL settings.
    pub database: DatabaseSettings,
    /// IAM integration settings.
    pub iam: IamSettings,
    /// Briefcase and Giphy settings.
    pub providers: ProviderSettings,
    /// Realtime protocol policy.
    pub realtime: RealtimeSettings,
    /// Delivery-worker policy.
    pub worker: WorkerSettings,
    /// Tracing filter directive.
    pub log_filter: String,
}

/// Minimal settings required by the migration binary.
#[derive(Clone, Debug)]
pub struct MigrationSettings {
    /// Runtime safety mode.
    pub environment: RuntimeEnvironment,
    /// PostgreSQL settings.
    pub database: DatabaseSettings,
    /// Tracing filter directive.
    pub log_filter: String,
}

/// Deployment safety mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeEnvironment {
    /// Developer workstation.
    Development,
    /// Automated tests.
    Test,
    /// Deployed service.
    Production,
}

/// Inbound server policy.
#[derive(Clone, Debug)]
pub struct ServerSettings {
    /// Listener address.
    pub bind_addr: SocketAddr,
    /// Canonical externally visible API base URL.
    pub public_base_url: Url,
    /// Maximum ordinary REST request duration.
    pub request_timeout: Duration,
    /// Maximum accepted HTTP body size.
    pub max_body_bytes: usize,
    /// Graceful-shutdown deadline.
    pub shutdown_timeout: Duration,
}

/// PostgreSQL connection-pool policy.
#[derive(Clone, Debug)]
pub struct DatabaseSettings {
    /// Connection URL.
    pub url: SecretString,
    /// Maximum connections per process.
    pub max_connections: NonZeroU32,
    /// Minimum idle connections per process.
    pub min_connections: u32,
    /// Pool acquisition deadline.
    pub acquire_timeout: Duration,
    /// Per-statement deadline.
    pub statement_timeout: Duration,
}

/// IAM authentication settings.
#[derive(Clone, Debug)]
pub struct IamSettings {
    /// IAM API base URL.
    pub base_url: Url,
    /// DM application identifier.
    pub app_id: String,
    /// DM application secret.
    pub app_secret: SecretString,
    /// Outbound IAM deadline.
    pub request_timeout: Duration,
}

/// External content-provider settings.
#[derive(Clone, Debug)]
pub struct ProviderSettings {
    /// Briefcase API base URL.
    pub briefcase_base_url: Url,
    /// IAM application audience accepted by Briefcase.
    pub briefcase_iam_audience: String,
    /// Giphy API base URL.
    pub giphy_api_base_url: Url,
    /// Required Giphy API key.
    pub giphy_api_key: SecretString,
    /// Shared provider request deadline.
    pub request_timeout: Duration,
    /// Trending-GIF cache lifetime.
    pub trending_cache_ttl: Duration,
}

/// WebSocket protocol settings.
#[derive(Clone, Debug)]
pub struct RealtimeSettings {
    /// Server ping interval.
    pub heartbeat_interval: Duration,
    /// Maximum time without a matching pong.
    pub heartbeat_timeout: Duration,
    /// Bounded outbound frames buffered per WebSocket.
    pub outbound_capacity: NonZeroUsize,
    /// Expiry for activity such as typing when no refresh arrives.
    pub activity_ttl: Duration,
}

/// Durable delivery settings.
#[derive(Clone, Debug)]
pub struct WorkerSettings {
    /// Maximum deliveries processed in one claim.
    pub batch_size: NonZeroUsize,
    /// Idle polling interval.
    pub poll_interval: Duration,
    /// Claim lease duration.
    pub lease_duration: Duration,
    /// Attempts before dead-letter state.
    pub max_attempts: u16,
    /// Maximum retry delay.
    pub max_retry_delay: Duration,
}

/// Configuration load or validation error with secrets redacted.
#[derive(Debug, Error)]
pub enum SettingsError {
    /// Required environment variable is absent.
    #[error("required environment variable {0} is missing")]
    Missing(&'static str),
    /// Environment variable is malformed or violates safety policy.
    #[error("invalid environment variable {name}: {reason}")]
    Invalid {
        /// Variable name.
        name: &'static str,
        /// Redacted reason.
        reason: String,
    },
}

impl Settings {
    /// Loads and validates the process environment.
    ///
    /// # Errors
    ///
    /// Returns a redacted error for absent, malformed, or unsafe values.
    pub fn from_env() -> Result<Self, SettingsError> {
        let environment = parse_or("DM_ENVIRONMENT", "development")?;
        let server = ServerSettings {
            bind_addr: parse_or("DM_BIND_ADDR", "127.0.0.1:8080")?,
            public_base_url: parse_required("DM_PUBLIC_BASE_URL")?,
            request_timeout: duration_seconds("DM_REQUEST_TIMEOUT_SECONDS", 15)?,
            max_body_bytes: parse_or("DM_MAX_HTTP_BODY_BYTES", "134217728")?,
            shutdown_timeout: duration_seconds("DM_SHUTDOWN_TIMEOUT_SECONDS", 30)?,
        };
        let database = database_settings()?;
        let iam = IamSettings {
            base_url: parse_required("DM_IAM_BASE_URL")?,
            app_id: required("DM_IAM_APP_ID")?,
            app_secret: SecretString::from(required("DM_IAM_APP_SECRET")?),
            request_timeout: duration_seconds("DM_IAM_REQUEST_TIMEOUT_SECONDS", 5)?,
        };
        let providers = ProviderSettings {
            briefcase_base_url: parse_required("DM_BRIEFCASE_BASE_URL")?,
            briefcase_iam_audience: parse_or("DM_BRIEFCASE_IAM_AUDIENCE", "silicon-briefcase")?,
            giphy_api_base_url: parse_or("DM_GIPHY_API_BASE_URL", "https://api.giphy.com/v1/gifs")?,
            giphy_api_key: SecretString::from(required("DM_GIPHY_API_KEY")?),
            request_timeout: duration_seconds("DM_PROVIDER_REQUEST_TIMEOUT_SECONDS", 10)?,
            trending_cache_ttl: duration_seconds("DM_TRENDING_GIF_CACHE_SECONDS", 300)?,
        };
        let realtime = RealtimeSettings {
            heartbeat_interval: duration_seconds("DM_HEARTBEAT_INTERVAL_SECONDS", 30)?,
            heartbeat_timeout: duration_seconds("DM_HEARTBEAT_TIMEOUT_SECONDS", 120)?,
            outbound_capacity: parse_or("DM_WS_OUTBOUND_CAPACITY", "256")?,
            activity_ttl: duration_seconds("DM_PRESENCE_ACTIVITY_TTL_SECONDS", 10)?,
        };
        let worker = WorkerSettings {
            batch_size: parse_or("DM_DELIVERY_BATCH_SIZE", "100")?,
            poll_interval: duration_milliseconds("DM_DELIVERY_POLL_INTERVAL_MILLISECONDS", 250)?,
            lease_duration: duration_seconds("DM_DELIVERY_LEASE_SECONDS", 30)?,
            max_attempts: parse_or("DM_DELIVERY_MAX_ATTEMPTS", "20")?,
            max_retry_delay: duration_seconds("DM_DELIVERY_MAX_RETRY_SECONDS", 300)?,
        };
        let settings = Self {
            environment,
            server,
            database,
            iam,
            providers,
            realtime,
            worker,
            log_filter: parse_or("DM_LOG_FILTER", "info,silicon_dm=debug")?,
        };
        settings.validate()?;
        Ok(settings)
    }

    fn validate(&self) -> Result<(), SettingsError> {
        validate_url(
            "DM_PUBLIC_BASE_URL",
            &self.server.public_base_url,
            self.environment,
        )?;
        validate_url("DM_IAM_BASE_URL", &self.iam.base_url, self.environment)?;
        validate_app_id("DM_IAM_APP_ID", &self.iam.app_id)?;
        validate_url(
            "DM_BRIEFCASE_BASE_URL",
            &self.providers.briefcase_base_url,
            self.environment,
        )?;
        validate_app_id(
            "DM_BRIEFCASE_IAM_AUDIENCE",
            &self.providers.briefcase_iam_audience,
        )?;
        validate_url(
            "DM_GIPHY_API_BASE_URL",
            &self.providers.giphy_api_base_url,
            self.environment,
        )?;
        if self.database.min_connections > self.database.max_connections.get() {
            return Err(invalid(
                "DM_DATABASE_MIN_CONNECTIONS",
                "must not exceed DM_DATABASE_MAX_CONNECTIONS",
            ));
        }
        if self.server.max_body_bytes == 0 || self.server.max_body_bytes > MAX_HTTP_BODY_BYTES {
            return Err(invalid(
                "DM_MAX_HTTP_BODY_BYTES",
                "must be between 1 and 134217728 bytes",
            ));
        }
        validate_database_transport(&self.database.url, self.environment)?;
        if self.realtime.heartbeat_timeout <= self.realtime.heartbeat_interval {
            return Err(invalid(
                "DM_HEARTBEAT_TIMEOUT_SECONDS",
                "must be greater than the heartbeat interval",
            ));
        }
        if self.realtime.heartbeat_interval != HEARTBEAT_INTERVAL {
            return Err(invalid(
                "DM_HEARTBEAT_INTERVAL_SECONDS",
                "must be 30 seconds for WebSocket protocol version 2",
            ));
        }
        if self.realtime.heartbeat_timeout != HEARTBEAT_TIMEOUT {
            return Err(invalid(
                "DM_HEARTBEAT_TIMEOUT_SECONDS",
                "must be 120 seconds for WebSocket protocol version 2",
            ));
        }
        if self.worker.max_attempts == 0 {
            return Err(invalid("DM_DELIVERY_MAX_ATTEMPTS", "must be positive"));
        }
        if self.worker.batch_size.get() > 1_000 {
            return Err(invalid("DM_DELIVERY_BATCH_SIZE", "must not exceed 1000"));
        }
        if self.worker.lease_duration.as_millis() > i64::MAX as u128 {
            return Err(invalid(
                "DM_DELIVERY_LEASE_SECONDS",
                "is too large for PostgreSQL millisecond arithmetic",
            ));
        }
        if time::Duration::try_from(self.worker.max_retry_delay).is_err() {
            return Err(invalid(
                "DM_DELIVERY_MAX_RETRY_SECONDS",
                "is outside the supported retry timestamp range",
            ));
        }
        Ok(())
    }
}

impl MigrationSettings {
    /// Loads settings needed by `dm-migrate`.
    ///
    /// # Errors
    ///
    /// Returns a redacted configuration error.
    pub fn from_env() -> Result<Self, SettingsError> {
        let environment = parse_or("DM_ENVIRONMENT", "development")?;
        let database = database_settings()?;
        validate_database_transport(&database.url, environment)?;
        Ok(Self {
            environment,
            database,
            log_filter: parse_or("DM_LOG_FILTER", "info,silicon_dm=debug")?,
        })
    }
}

impl FromStr for RuntimeEnvironment {
    type Err = SettingsError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "development" => Ok(Self::Development),
            "test" => Ok(Self::Test),
            "production" => Ok(Self::Production),
            _ => Err(invalid(
                "DM_ENVIRONMENT",
                "must be development, test, or production",
            )),
        }
    }
}

fn database_settings() -> Result<DatabaseSettings, SettingsError> {
    Ok(DatabaseSettings {
        url: SecretString::from(required("DM_DATABASE_URL")?),
        max_connections: parse_or("DM_DATABASE_MAX_CONNECTIONS", "16")?,
        min_connections: parse_or("DM_DATABASE_MIN_CONNECTIONS", "1")?,
        acquire_timeout: duration_seconds("DM_DATABASE_ACQUIRE_TIMEOUT_SECONDS", 5)?,
        statement_timeout: duration_seconds("DM_DATABASE_STATEMENT_TIMEOUT_SECONDS", 10)?,
    })
}

fn required(name: &'static str) -> Result<String, SettingsError> {
    optional(name).ok_or(SettingsError::Missing(name))
}

fn optional(name: &'static str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn parse_required<T>(name: &'static str) -> Result<T, SettingsError>
where
    T: FromStr,
    T::Err: std::fmt::Display,
{
    required(name)?
        .parse()
        .map_err(|error| invalid(name, error))
}

fn parse_or<T>(name: &'static str, default: &str) -> Result<T, SettingsError>
where
    T: FromStr,
    T::Err: std::fmt::Display,
{
    optional(name)
        .as_deref()
        .unwrap_or(default)
        .parse()
        .map_err(|error| invalid(name, error))
}

fn duration_seconds(name: &'static str, default: u64) -> Result<Duration, SettingsError> {
    let seconds = parse_or(name, &default.to_string())?;
    if seconds == 0 {
        return Err(invalid(name, "must be positive"));
    }
    Ok(Duration::from_secs(seconds))
}

fn duration_milliseconds(name: &'static str, default: u64) -> Result<Duration, SettingsError> {
    let milliseconds = parse_or(name, &default.to_string())?;
    if milliseconds == 0 {
        return Err(invalid(name, "must be positive"));
    }
    Ok(Duration::from_millis(milliseconds))
}

fn validate_url(
    name: &'static str,
    url: &Url,
    environment: RuntimeEnvironment,
) -> Result<(), SettingsError> {
    if !matches!(url.scheme(), "http" | "https")
        || url.cannot_be_a_base()
        || url.host_str().is_none()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid(
            name,
            "must be an absolute HTTP(S) base URL without a query or fragment",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(invalid(name, "must not contain embedded credentials"));
    }
    let secure = url.scheme() == "https";
    let local = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "::1"));
    if environment == RuntimeEnvironment::Production && !secure {
        return Err(invalid(name, "must use HTTPS in production"));
    }
    if !secure && !local {
        return Err(invalid(
            name,
            "plain HTTP is allowed only for local development",
        ));
    }
    Ok(())
}

fn validate_app_id(name: &'static str, value: &str) -> Result<(), SettingsError> {
    if !(3..=80).contains(&value.len())
        || !value.starts_with(|character: char| character.is_ascii_lowercase())
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
    {
        return Err(invalid(
            name,
            "must match IAM application identifier syntax",
        ));
    }
    Ok(())
}

fn validate_database_transport(
    database_url: &SecretString,
    environment: RuntimeEnvironment,
) -> Result<(), SettingsError> {
    let parsed = Url::parse(database_url.expose_secret()).map_err(|_| {
        invalid(
            "DM_DATABASE_URL",
            "must be a valid PostgreSQL connection URL",
        )
    })?;
    if !matches!(parsed.scheme(), "postgres" | "postgresql") || parsed.host_str().is_none() {
        return Err(invalid(
            "DM_DATABASE_URL",
            "must be a network PostgreSQL connection URL",
        ));
    }
    if environment == RuntimeEnvironment::Production {
        let ssl_modes = parsed
            .query_pairs()
            .filter_map(|(name, value)| (name == "sslmode").then_some(value))
            .collect::<Vec<_>>();
        if ssl_modes.as_slice() != ["verify-full"] {
            return Err(invalid(
                "DM_DATABASE_URL",
                "must specify exactly one sslmode=verify-full in production",
            ));
        }
    }
    Ok(())
}

fn invalid(name: &'static str, reason: impl std::fmt::Display) -> SettingsError {
    SettingsError::Invalid {
        name,
        reason: reason.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::{process::Command, str::FromStr as _};

    use secrecy::SecretString;

    use url::Url;

    use super::{
        RuntimeEnvironment, Settings, SettingsError, validate_app_id, validate_database_transport,
        validate_url,
    };

    #[test]
    fn giphy_api_key_is_required() {
        const CHILD_MARKER: &str = "DM_CONFIG_GIPHY_REQUIRED_TEST_CHILD";
        const TEST_NAME: &str = "config::tests::giphy_api_key_is_required";

        if std::env::var_os(CHILD_MARKER).is_some() {
            assert!(matches!(
                Settings::from_env(),
                Err(SettingsError::Missing("DM_GIPHY_API_KEY"))
            ));
            return;
        }

        let Ok(test_binary) = std::env::current_exe() else {
            panic!("the current test executable should be available");
        };
        let Ok(output) = Command::new(test_binary)
            .args(["--exact", TEST_NAME, "--nocapture"])
            .env_clear()
            .env(CHILD_MARKER, "1")
            .env("DM_ENVIRONMENT", "test")
            .env("DM_PUBLIC_BASE_URL", "http://localhost:8080")
            .env("DM_DATABASE_URL", "postgres://user:secret@localhost/dm")
            .env("DM_IAM_BASE_URL", "http://localhost:8081")
            .env("DM_IAM_APP_ID", "silicon-dm")
            .env("DM_IAM_APP_SECRET", "test-secret")
            .env("DM_BRIEFCASE_BASE_URL", "http://localhost:8082")
            .env_remove("DM_GIPHY_API_KEY")
            .output()
        else {
            panic!("the isolated configuration test process should start");
        };

        assert!(
            output.status.success(),
            "isolated configuration test failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn runtime_environment_is_explicit() {
        assert!(matches!(
            RuntimeEnvironment::from_str("production"),
            Ok(RuntimeEnvironment::Production)
        ));
        assert!(RuntimeEnvironment::from_str("prod").is_err());
    }

    #[test]
    fn production_database_requires_full_tls_verification() {
        let insecure = SecretString::from("postgres://user:secret@db.example/dm".to_owned());
        let secure = SecretString::from(
            "postgres://user:secret@db.example/dm?sslmode=verify-full".to_owned(),
        );
        assert!(validate_database_transport(&insecure, RuntimeEnvironment::Production).is_err());
        assert!(validate_database_transport(&secure, RuntimeEnvironment::Production).is_ok());
        assert!(validate_database_transport(&insecure, RuntimeEnvironment::Development).is_ok());
    }

    #[test]
    fn service_base_urls_require_http_without_query_parameters() {
        assert!(Url::parse("ftp://localhost/service").is_ok_and(|url| {
            validate_url("DM_IAM_BASE_URL", &url, RuntimeEnvironment::Development).is_err()
        }));
        assert!(
            Url::parse("http://localhost/service?token=secret").is_ok_and(|url| {
                validate_url("DM_IAM_BASE_URL", &url, RuntimeEnvironment::Development).is_err()
            })
        );
        assert!(Url::parse("http://localhost/service").is_ok_and(|url| {
            validate_url("DM_IAM_BASE_URL", &url, RuntimeEnvironment::Development).is_ok()
        }));
    }

    #[test]
    fn iam_application_identifiers_follow_the_published_contract() {
        assert!(validate_app_id("DM_IAM_APP_ID", "silicon-dm").is_ok());
        assert!(validate_app_id("DM_IAM_APP_ID", "Silicon-DM").is_err());
        assert!(validate_app_id("DM_IAM_APP_ID", "ab").is_err());
    }
}
