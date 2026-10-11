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
const MAX_HTTP_BODY_BYTES: usize = 3 * 1024 * 1024 * 1024;

/// Complete process settings.
#[derive(Clone, Debug)]
pub struct Settings {
    /// Runtime safety mode.
    pub environment: RuntimeEnvironment,
    /// Inbound HTTP settings.
    pub server: ServerSettings,
    /// PostgreSQL settings.
    pub database: DatabaseSettings,
    /// Silicon Accounts: sign-in, token verification, lookups, proofs and webhooks.
    pub accounts: AccountsSettings,
    /// Key material for the sync-cursor MAC and at-rest encryption of stored proofs.
    pub data_key: DataKey,
    /// Ting delivery; disabled unless `DM_TING_URL` is set.
    pub ting: TingSettings,
    /// Giphy settings.
    pub providers: ProviderSettings,
    /// Presence lease policy (named after the retired realtime protocol it came from).
    pub realtime: RealtimeSettings,
    /// Delivery-worker policy.
    pub worker: WorkerSettings,
    /// Optional Postmark notification transport for durable bug reports.
    pub reporting: Option<ReportingSettings>,
    /// Tracing filter directive.
    pub log_filter: String,
}

/// Server-only Postmark credentials.
#[derive(Clone, Debug)]
pub struct ReportingSettings {
    /// Postmark email endpoint; production requires HTTPS.
    pub endpoint: Url,
    /// Postmark server token, never returned to a client.
    pub token: SecretString,
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

/// Silicon Accounts settings. DM is the app `dm`; Carbons and Silicons sign in with
/// Silicon Accounts and send its access tokens (`aud` = the app id) as bearer tokens.
#[derive(Clone, Debug)]
pub struct AccountsSettings {
    /// Public origin of Silicon Accounts (`ACCOUNTS_URL`), exactly the `iss` of its tokens.
    pub issuer: String,
    /// Origin DM calls server to server (`ACCOUNTS_API_URL`, default `ACCOUNTS_URL`).
    pub api_url: Url,
    /// This app's id (`DM_APP_ID`, default `dm`).
    pub app_id: String,
    /// This app's secret (`DM_APP_SECRET`); never leaves the server.
    pub app_secret: SecretString,
    /// The app webhook signing secret (`DM_ACCOUNTS_WEBHOOK_SECRET`, `whsec_…`).
    pub webhook_secret: SecretString,
    /// Deadline of one call to Silicon Accounts.
    pub request_timeout: Duration,
    /// Apps whose User verification proofs DM accepts, per scope (`DM_PROOF_ISSUERS`).
    pub proof_issuers: ProofIssuers,
}

/// The proof scopes DM honours on its routes (see the API reference).
pub const PROOF_SCOPES: [&str; 5] = [
    "dm.conversations.read",
    "dm.messages.read",
    "dm.messages.write",
    "dm.receipts.write",
    "dm.drafts.write",
];

/// Per-scope allow-list of issuing apps for inbound User verification proofs.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProofIssuers(std::collections::BTreeMap<String, std::collections::BTreeSet<String>>);

impl ProofIssuers {
    /// Parses `scope=app,scope=app,…` (repeat a scope to allow several apps). Empty = deny all.
    ///
    /// # Errors
    /// Names the entry, the unknown scope or the malformed app id.
    pub fn parse(value: &str) -> Result<Self, String> {
        let mut issuers =
            std::collections::BTreeMap::<String, std::collections::BTreeSet<String>>::new();
        for entry in value
            .split(',')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
        {
            let Some((scope, app)) = entry.split_once('=') else {
                return Err(format!(
                    "entry `{entry}` is not scope=app (for example dm.messages.read=interface)"
                ));
            };
            let (scope, app) = (scope.trim(), app.trim());
            if !PROOF_SCOPES.contains(&scope) {
                return Err(format!(
                    "scope `{scope}` is not one DM honours; use one of {}",
                    PROOF_SCOPES.join(", ")
                ));
            }
            if !valid_app_id(app) {
                return Err(format!(
                    "`{app}` in `{entry}` is not an app id (lowercase letters, digits, - and _, starting with a letter)"
                ));
            }
            issuers
                .entry(scope.to_owned())
                .or_default()
                .insert(app.to_owned());
        }
        Ok(Self(issuers))
    }

    /// Whether `app` may present proofs carrying `scope`.
    #[must_use]
    pub fn allows(&self, scope: &str, app: &str) -> bool {
        self.0.get(scope).is_some_and(|apps| apps.contains(app))
    }

    /// Whether any issuer is configured at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The configured allow-list, for discovery and diagnostics.
    #[must_use]
    pub fn entries(
        &self,
    ) -> &std::collections::BTreeMap<String, std::collections::BTreeSet<String>> {
        &self.0
    }
}

/// 256-bit key material (`DM_DATA_KEY`, base64). Purpose keys are derived from it, so
/// rotating the Accounts app secret never invalidates cursors or stored ciphertext.
#[derive(Clone)]
pub struct DataKey([u8; 32]);

impl std::fmt::Debug for DataKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("DataKey(redacted)")
    }
}

impl DataKey {
    /// Wraps raw key bytes (tests and tools).
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Parses standard or URL-safe base64 of exactly 32 bytes.
    ///
    /// # Errors
    /// Says what is wrong with the value, never the value itself.
    pub fn parse(value: &str) -> Result<Self, String> {
        use base64::Engine as _;
        let value = value.trim();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(value)
            .or_else(|_| {
                base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(value.trim_end_matches('='))
            })
            .map_err(|_| {
                "must be base64 (generate one with `openssl rand -base64 32`)".to_owned()
            })?;
        let bytes: [u8; 32] = decoded.try_into().map_err(|bytes: Vec<u8>| {
            format!("must decode to exactly 32 bytes, got {}", bytes.len())
        })?;
        Ok(Self(bytes))
    }

    /// A 256-bit key for one purpose (BLAKE3 key derivation with a fixed context).
    #[must_use]
    pub fn derive(&self, context: &str) -> [u8; 32] {
        blake3::derive_key(context, &self.0)
    }
}

/// Ting's app id in Silicon Accounts unless `DM_TING_APP_ID` names another.
pub const DEFAULT_TING_APP_ID: &str = "ting";

/// Ting delivery transport. Off unless `DM_TING_URL` is set.
#[derive(Clone, Debug)]
pub struct TingSettings {
    /// Ting API origin, without a path prefix or credentials. `None` disables delivery
    /// through Ting: messages are still stored and clients sync over `/api/v1/sync`.
    pub base_url: Option<Url>,
    /// Ting's app id in Silicon Accounts: the receiving app of the proofs DM sends it
    /// (`DM_TING_APP_ID`, default `ting`).
    pub app_id: String,
    /// Maximum wait for a connection greeting or send acceptance.
    pub request_timeout: Duration,
}

/// External content-provider settings.
#[derive(Clone, Debug)]
pub struct ProviderSettings {
    /// Giphy API base URL.
    pub giphy_api_base_url: Url,
    /// Required Giphy API key.
    pub giphy_api_key: SecretString,
    /// Shared provider request deadline.
    pub request_timeout: Duration,
    /// Trending-GIF cache lifetime.
    pub trending_cache_ttl: Duration,
}

/// Presence lease settings.
#[derive(Clone, Debug)]
pub struct RealtimeSettings {
    /// Expected presence renewal interval.
    pub heartbeat_interval: Duration,
    /// Presence lease lifetime without a renewal.
    pub heartbeat_timeout: Duration,
    /// Retained for configuration compatibility; DM no longer serves sockets.
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

/// Environment variables DM no longer reads: those of the IAM/Honeycomb era, and the
/// telemetry settings (DM records no telemetry since 0.16).
pub const OBSOLETE_VARIABLES: [&str; 15] = [
    "DM_IAM_BASE_URL",
    "DM_IAM_APP_ID",
    "DM_IAM_APP_SECRET",
    "DM_IAM_WEBHOOK_SECRET",
    "DM_IAM_WEBHOOK_KEY_VERSION",
    "DM_IAM_REQUEST_TIMEOUT_SECONDS",
    "DM_TEST_DATABASE_URL",
    "DM_TEST_DATABASE_MAX_CONNECTIONS",
    "DM_TEST_KEY_ENCRYPTION_KEY",
    "DM_HONEYCOMB_SERVICE_TOKEN",
    "DM_HONEYCOMB_BASE_URL",
    "DM_TING_BASE_URL",
    "DM_TELEMETRY_ENABLED",
    "DM_TELEMETRY_HOME",
    "DM_SPACE_STATION_TABLE_KEY",
];

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
        let issuer = required("ACCOUNTS_URL")?.trim_end_matches('/').to_owned();
        let issuer_url: Url = issuer
            .parse()
            .map_err(|error| invalid("ACCOUNTS_URL", format!("is not a URL ({error})")))?;
        let accounts = AccountsSettings {
            api_url: match optional("ACCOUNTS_API_URL") {
                Some(value) => value.parse().map_err(|error| {
                    invalid("ACCOUNTS_API_URL", format!("is not a URL ({error})"))
                })?,
                None => issuer_url,
            },
            issuer,
            app_id: optional("DM_APP_ID").unwrap_or_else(|| "dm".to_owned()),
            app_secret: SecretString::from(required("DM_APP_SECRET")?),
            webhook_secret: SecretString::from(required("DM_ACCOUNTS_WEBHOOK_SECRET")?),
            request_timeout: duration_seconds("DM_ACCOUNTS_REQUEST_TIMEOUT_SECONDS", 5)?,
            proof_issuers: ProofIssuers::parse(&optional("DM_PROOF_ISSUERS").unwrap_or_default())
                .map_err(|reason| invalid("DM_PROOF_ISSUERS", reason))?,
        };
        let data_key = DataKey::parse(&required("DM_DATA_KEY")?)
            .map_err(|reason| invalid("DM_DATA_KEY", reason))?;
        let providers = ProviderSettings {
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
            accounts,
            data_key,
            ting: TingSettings {
                base_url: optional("DM_TING_URL")
                    .map(|value| {
                        value.parse().map_err(|error| {
                            invalid("DM_TING_URL", format!("is not a URL ({error})"))
                        })
                    })
                    .transpose()?,
                app_id: optional("DM_TING_APP_ID")
                    .unwrap_or_else(|| DEFAULT_TING_APP_ID.to_owned()),
                request_timeout: duration_seconds("DM_TING_REQUEST_TIMEOUT_SECONDS", 15)?,
            },
            providers,
            realtime,
            worker,
            reporting: optional("DM_POSTMARK_SERVER_TOKEN")
                .map(|token| {
                    Ok::<_, SettingsError>(ReportingSettings {
                        token: SecretString::from(token),
                        endpoint: parse_or(
                            "DM_POSTMARK_EMAIL_URL",
                            "https://api.postmarkapp.com/email",
                        )?,
                    })
                })
                .transpose()?,
            log_filter: parse_or("DM_LOG_FILTER", "info,silicon_dm=debug")?,
        };
        settings.validate()?;
        Ok(settings)
    }

    /// Obsolete variables that are set in this environment. They are
    /// ignored; the process logs them once at start so an operator can remove them.
    #[must_use]
    pub fn obsolete_variables_present() -> Vec<&'static str> {
        OBSOLETE_VARIABLES
            .iter()
            .copied()
            .filter(|name| optional(name).is_some())
            .collect()
    }

    fn validate(&self) -> Result<(), SettingsError> {
        if let Some(reporting) = &self.reporting {
            validate_url(
                "DM_POSTMARK_EMAIL_URL",
                &reporting.endpoint,
                self.environment,
            )?;
        }
        validate_url(
            "DM_PUBLIC_BASE_URL",
            &self.server.public_base_url,
            self.environment,
        )?;
        let issuer: Url = self
            .accounts
            .issuer
            .parse()
            .map_err(|error| invalid("ACCOUNTS_URL", format!("is not a URL ({error})")))?;
        validate_url("ACCOUNTS_URL", &issuer, self.environment)?;
        validate_url("ACCOUNTS_API_URL", &self.accounts.api_url, self.environment)?;
        validate_app_id("DM_APP_ID", &self.accounts.app_id)?;
        if !self
            .accounts
            .webhook_secret
            .expose_secret()
            .starts_with("whsec_")
        {
            return Err(invalid(
                "DM_ACCOUNTS_WEBHOOK_SECRET",
                "must be the app's webhook secret from Silicon Accounts (it starts with whsec_)",
            ));
        }
        self.validate_ting()?;
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
                "must be between 1 and 3221225472 bytes",
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
                "must be 30 seconds (the presence renewal interval clients use)",
            ));
        }
        if self.realtime.heartbeat_timeout != HEARTBEAT_TIMEOUT {
            return Err(invalid(
                "DM_HEARTBEAT_TIMEOUT_SECONDS",
                "must be 120 seconds (the presence lease clients expect)",
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

    /// `DM_TING_APP_ID` names another app than DM, and `DM_TING_URL` (when set) is an
    /// origin this environment may use.
    fn validate_ting(&self) -> Result<(), SettingsError> {
        validate_app_id("DM_TING_APP_ID", &self.ting.app_id)?;
        if self.ting.app_id == self.accounts.app_id {
            return Err(invalid(
                "DM_TING_APP_ID",
                "must be Ting's app id, not DM's own (Silicon Accounts never issues an app a proof for itself)",
            ));
        }
        if let Some(ting) = &self.ting.base_url {
            validate_url("DM_TING_URL", ting, self.environment)?;
            if ting.path() != "/" {
                return Err(invalid(
                    "DM_TING_URL",
                    "must be an origin without a path, query or fragment",
                ));
            }
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
    if environment == RuntimeEnvironment::Production && !secure {
        return Err(invalid(name, "must use HTTPS in production"));
    }
    if !secure && !is_loopback(url) {
        return Err(invalid(
            name,
            "must use HTTPS; plain HTTP is allowed only for this machine (localhost, *.localhost or a loopback address)",
        ));
    }
    Ok(())
}

/// Whether a URL names this machine (where plain HTTP to a local stack is acceptable).
#[must_use]
pub fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(domain)) => domain == "localhost" || domain.ends_with(".localhost"),
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        None => false,
    }
}

/// Whether `value` has the shape of a Silicon Apps / Silicon Accounts app id.
#[must_use]
pub fn valid_app_id(value: &str) -> bool {
    (1..=80).contains(&value.len())
        && value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase())
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
}

fn validate_app_id(name: &'static str, value: &str) -> Result<(), SettingsError> {
    if !valid_app_id(value) {
        return Err(invalid(
            name,
            "must be the app id registered with Silicon Accounts, such as dm (lowercase letters, digits, - and _, starting with a letter)",
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
        DataKey, ProofIssuers, RuntimeEnvironment, Settings, SettingsError, is_loopback,
        validate_app_id, validate_database_transport, validate_url,
    };

    const CHILD_SCENARIO: &str = "DM_CONFIG_TEST_SCENARIO";

    /// Runs `scenario` of `test_name` in a child process with exactly `environment`, so
    /// the parent's variables never leak into configuration loading.
    fn run_child(test_name: &str, scenario: &str, environment: &[(&str, &str)]) {
        let Ok(test_binary) = std::env::current_exe() else {
            panic!("the current test executable should be available");
        };
        let mut command = Command::new(test_binary);
        command
            .args(["--exact", test_name, "--nocapture"])
            .env_clear()
            .env(CHILD_SCENARIO, scenario);
        for (name, value) in environment {
            command.env(name, value);
        }
        let Ok(output) = command.output() else {
            panic!("the isolated configuration test process should start");
        };
        assert!(
            output.status.success(),
            "isolated configuration scenario {scenario} failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn base_environment() -> Vec<(&'static str, &'static str)> {
        vec![
            ("DM_ENVIRONMENT", "test"),
            ("DM_PUBLIC_BASE_URL", "http://localhost:4121/api/v1"),
            ("DM_DATABASE_URL", "postgres://user:secret@localhost/dm"),
            ("ACCOUNTS_URL", "http://localhost:9590"),
            ("DM_APP_SECRET", "sa_app_test"),
            ("DM_ACCOUNTS_WEBHOOK_SECRET", "whsec_test"),
            (
                "DM_DATA_KEY",
                "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=",
            ),
            ("DM_GIPHY_API_KEY", "giphy"),
        ]
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one table of boot scenarios, each run in a child process"
    )]
    fn configuration_scenarios_load_and_fail_exactly() {
        const TEST_NAME: &str = "config::tests::configuration_scenarios_load_and_fail_exactly";
        if let Ok(scenario) = std::env::var(CHILD_SCENARIO) {
            let loaded = Settings::from_env();
            match scenario.as_str() {
                "complete" => {
                    let settings = loaded.unwrap_or_else(|error| panic!("{error}"));
                    assert_eq!(settings.accounts.app_id, "dm");
                    assert_eq!(settings.accounts.issuer, "http://localhost:9590");
                    assert_eq!(settings.accounts.api_url.as_str(), "http://localhost:9590/");
                    assert!(settings.ting.base_url.is_none(), "Ting is off by default");
                    assert_eq!(settings.ting.app_id, "ting");
                    assert!(
                        settings.accounts.proof_issuers.is_empty(),
                        "no issuer by default"
                    );
                }
                "missing_giphy" => assert!(matches!(
                    loaded,
                    Err(SettingsError::Missing("DM_GIPHY_API_KEY"))
                )),
                "missing_secret" => {
                    assert!(matches!(
                        loaded,
                        Err(SettingsError::Missing("DM_APP_SECRET"))
                    ));
                }
                "missing_accounts_url" => {
                    assert!(matches!(
                        loaded,
                        Err(SettingsError::Missing("ACCOUNTS_URL"))
                    ));
                }
                "public_http_accounts" => {
                    let error = loaded
                        .err()
                        .map(|error| error.to_string())
                        .unwrap_or_default();
                    assert!(
                        error.starts_with(
                            "invalid environment variable ACCOUNTS_URL: must use HTTPS"
                        ),
                        "{error}"
                    );
                }
                "bad_webhook_secret" => {
                    let error = loaded
                        .err()
                        .map(|error| error.to_string())
                        .unwrap_or_default();
                    assert_eq!(
                        error,
                        "invalid environment variable DM_ACCOUNTS_WEBHOOK_SECRET: must be the app's webhook secret from Silicon Accounts (it starts with whsec_)"
                    );
                }
                "bad_data_key" => {
                    let error = loaded
                        .err()
                        .map(|error| error.to_string())
                        .unwrap_or_default();
                    assert_eq!(
                        error,
                        "invalid environment variable DM_DATA_KEY: must decode to exactly 32 bytes, got 3"
                    );
                }
                "ting_app_is_dm" => {
                    let error = loaded
                        .err()
                        .map(|error| error.to_string())
                        .unwrap_or_default();
                    assert!(
                        error.starts_with("invalid environment variable DM_TING_APP_ID: must be Ting's app id, not DM's own"),
                        "{error}"
                    );
                }
                "bad_proof_issuers" => {
                    let error = loaded
                        .err()
                        .map(|error| error.to_string())
                        .unwrap_or_default();
                    assert!(
                        error.contains("scope `dm.everything` is not one DM honours"),
                        "{error}"
                    );
                }
                "with_options" => {
                    let settings = loaded.unwrap_or_else(|error| panic!("{error}"));
                    assert_eq!(settings.accounts.api_url.as_str(), "http://127.0.0.1:9589/");
                    assert!(
                        settings
                            .accounts
                            .proof_issuers
                            .allows("dm.messages.read", "interface")
                    );
                    assert!(
                        !settings
                            .accounts
                            .proof_issuers
                            .allows("dm.messages.write", "interface")
                    );
                    assert_eq!(
                        settings.ting.base_url.map(|url| url.to_string()).as_deref(),
                        Some("http://127.0.0.1:4122/")
                    );
                    assert_eq!(settings.ting.app_id, "ting-staging");
                    assert_eq!(
                        Settings::obsolete_variables_present(),
                        vec!["DM_IAM_APP_SECRET", "DM_SPACE_STATION_TABLE_KEY"]
                    );
                }
                other => panic!("unknown scenario {other}"),
            }
            return;
        }
        let base = base_environment();
        let without = |name: &str| -> Vec<(&str, &str)> {
            base.iter()
                .copied()
                .filter(|(key, _)| *key != name)
                .collect()
        };
        let with = |name: &'static str, value: &'static str| -> Vec<(&'static str, &'static str)> {
            let mut environment: Vec<_> = base
                .iter()
                .copied()
                .filter(|(key, _)| *key != name)
                .collect();
            environment.push((name, value));
            environment
        };
        run_child(TEST_NAME, "complete", &base);
        run_child(TEST_NAME, "missing_giphy", &without("DM_GIPHY_API_KEY"));
        run_child(TEST_NAME, "missing_secret", &without("DM_APP_SECRET"));
        run_child(TEST_NAME, "missing_accounts_url", &without("ACCOUNTS_URL"));
        run_child(
            TEST_NAME,
            "public_http_accounts",
            &with("ACCOUNTS_URL", "http://accounts.example.test"),
        );
        run_child(
            TEST_NAME,
            "bad_webhook_secret",
            &with("DM_ACCOUNTS_WEBHOOK_SECRET", "not-a-webhook-secret"),
        );
        run_child(TEST_NAME, "bad_data_key", &with("DM_DATA_KEY", "AAEC"));
        run_child(TEST_NAME, "ting_app_is_dm", &with("DM_TING_APP_ID", "dm"));
        run_child(
            TEST_NAME,
            "bad_proof_issuers",
            &with("DM_PROOF_ISSUERS", "dm.everything=interface"),
        );
        let mut options = base.clone();
        options.extend([
            ("ACCOUNTS_API_URL", "http://127.0.0.1:9589"),
            (
                "DM_PROOF_ISSUERS",
                "dm.messages.read=interface, dm.conversations.read=interface",
            ),
            ("DM_TING_URL", "http://127.0.0.1:4122"),
            ("DM_TING_APP_ID", "ting-staging"),
            ("DM_IAM_APP_SECRET", "left over"),
            ("DM_SPACE_STATION_TABLE_KEY", "left over"),
        ]);
        run_child(TEST_NAME, "with_options", &options);
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
            validate_url("ACCOUNTS_URL", &url, RuntimeEnvironment::Development).is_err()
        }));
        assert!(
            Url::parse("http://localhost/service?token=secret").is_ok_and(|url| {
                validate_url("ACCOUNTS_URL", &url, RuntimeEnvironment::Development).is_err()
            })
        );
        assert!(Url::parse("http://localhost/service").is_ok_and(|url| {
            validate_url("ACCOUNTS_URL", &url, RuntimeEnvironment::Development).is_ok()
        }));
    }

    #[test]
    fn plain_http_is_only_for_this_machine() -> Result<(), url::ParseError> {
        for local in [
            "http://localhost:9590",
            "http://dm.localhost",
            "http://127.0.0.1:9589",
            "http://[::1]:1",
        ] {
            assert!(is_loopback(&Url::parse(local)?), "{local}");
        }
        for remote in [
            "http://accounts.teamofsilicons.com",
            "http://10.0.0.2",
            "http://localhost.example.com",
        ] {
            assert!(!is_loopback(&Url::parse(remote)?), "{remote}");
        }
        assert!(
            validate_url(
                "ACCOUNTS_URL",
                &Url::parse("http://localhost:9590")?,
                RuntimeEnvironment::Production
            )
            .is_err(),
            "production always needs HTTPS"
        );
        Ok(())
    }

    #[test]
    fn app_ids_follow_the_published_contract() {
        assert!(validate_app_id("DM_APP_ID", "dm").is_ok());
        assert!(validate_app_id("DM_APP_ID", "Silicon-DM").is_err());
        assert!(validate_app_id("DM_APP_ID", "tos>dm").is_err());
    }

    #[test]
    fn proof_issuers_are_per_scope_and_deny_by_default() {
        let empty = ProofIssuers::parse("").unwrap_or_default();
        assert!(!empty.allows("dm.messages.read", "interface"));
        let parsed = ProofIssuers::parse("dm.messages.read=interface,dm.messages.read=other")
            .unwrap_or_default();
        assert!(parsed.allows("dm.messages.read", "interface"));
        assert!(parsed.allows("dm.messages.read", "other"));
        assert!(!parsed.allows("dm.drafts.write", "interface"));
        assert!(ProofIssuers::parse("dm.messages.read").is_err());
        assert!(ProofIssuers::parse("dm.messages.read=Bad App").is_err());
    }

    #[test]
    fn data_keys_need_exactly_32_bytes_and_derive_per_purpose() {
        let key = DataKey::parse("AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=")
            .unwrap_or_else(|reason| panic!("{reason}"));
        assert_ne!(key.derive("a"), key.derive("b"));
        assert_eq!(format!("{key:?}"), "DataKey(redacted)");
        assert!(DataKey::parse("not base64!").is_err());
    }
}
