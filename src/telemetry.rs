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

use crate::{
    AppError, AppResult,
    api::extract::{ApiJson, Authenticated},
    application::state::AppState,
};
use axum::{
    extract::{MatchedPath, Request, State},
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::Response,
};
use secrecy::ExposeSecret as _;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};

/// Official Space Station client. Errors never affect application delivery.
#[derive(Clone, Default)]
pub struct Recorder(Option<Arc<space_station::SpaceClient>>);
impl Recorder {
    /// Build a private, nonblocking spool-backed exporter.
    ///
    /// # Errors
    /// Returns a redacted configuration error for an invalid ingest key.
    pub fn new(settings: &crate::config::TelemetrySettings) -> AppResult<Self> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        if !settings.enabled {
            return Ok(Self::default());
        }
        let Some(key) = &settings.table_key else {
            return Ok(Self::default());
        };
        let client = space_station::SpaceClient::builder(key.expose_secret())
            .home(&settings.home)
            .flush_timeout(Duration::from_millis(200))
            .on_error(|_| { /* exporter failures must not recursively generate diagnostics */ })
            .build()
            .map_err(|_| AppError::validation("invalid DM_SPACE_STATION_TABLE_KEY"))?;
        Ok(Self(Some(Arc::new(client))))
    }
}

/// Emit only the diagnostic schema's allowlisted fields; never include user content.
pub fn record(state: &AppState, source: &str, event: &str, fields: Value) {
    if !state.settings.telemetry.enabled {
        return;
    }
    let value = diagnostic(state, source, event, fields);
    if state.testing_environment.is_some() {
        // Separate sandbox storage: a test operation cannot produce a production event.
        static PENDING: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
        let limit = PENDING
            .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(128)))
            .clone();
        let Ok(permit) = limit.try_acquire_owned() else {
            return;
        };
        let selected = state.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let (Some(registry), Some(id), Some(generation)) = (
                &selected.testing,
                selected.testing_environment,
                selected.testing_generation,
            ) else {
                return;
            };
            let Ok(_fence) = registry.request_fence(id, generation).await else {
                return;
            };
            let _ = sqlx::query("INSERT INTO telemetry_events(event) VALUES($1)")
                .bind(value)
                .execute(selected.store.pool())
                .await;
        });
    } else if let Some(client) = &state.telemetry.0 {
        client.record(value);
    }
}
fn safe_fields(fields: Value) -> serde_json::Map<String, Value> {
    let mut safe = serde_json::Map::new();
    if let Value::Object(fields) = fields {
        for (key, value) in fields {
            let allowed = match key.as_str() {
                "duration_ms" | "status" | "count" | "sequence" | "attempt" => value.is_u64(),
                "success" => value.is_boolean(),
                "request_id" | "session_id" => value
                    .as_str()
                    .is_some_and(|s| uuid::Uuid::parse_str(s).is_ok()),
                "route" | "method" | "code" | "stage" => value.as_str().is_some_and(|s| {
                    s.len() <= 180
                        && s.bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"/{}_-.:".contains(&b))
                }),
                _ => false,
            };
            if allowed {
                safe.insert(key, value);
            }
        }
    }
    safe
}
fn diagnostic(state: &AppState, source: &str, event: &str, fields: Value) -> Value {
    let safe = safe_fields(fields);
    let source = if ["backend", "api", "sdk", "cli", "daemon", "web"].contains(&source) {
        source
    } else {
        "backend"
    };
    let event = if [
        "http.completed",
        "websocket.opened",
        "websocket.closed",
        "websocket.received",
        "delivery.sent",
        "delivery.acknowledged",
        "iam.webhook_applied",
        "command",
        "connection",
        "callback",
        "queue",
        "update",
        "page_view",
        "request",
        "web_error",
        "report.queued",
        "report.notification",
    ]
    .contains(&event)
    {
        event
    } else {
        "diagnostic"
    };
    json!({"schema_version":1,"app":"silicon-dm","version":env!("CARGO_PKG_VERSION"),
        "source":source,"event":event,"environment":if state.testing_environment.is_some(){"testing"}else{"production"},
        "testing_environment_id":state.testing_environment,"testing_generation":state.testing_generation,
        "instance_id":state.instance_id.as_ref(),"context":safe})
}
fn requested(headers: &HeaderMap) -> bool {
    headers.get("x-dm-telemetry").is_none_or(|v| v != "off")
}
/// Capture completion, latency and failure for every admitted API operation.
pub(crate) async fn observe(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    if !requested(request.headers()) {
        return next.run(request).await;
    }
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map_or("unmatched", MatchedPath::as_str)
        .to_owned();
    let method = request.method().as_str().to_owned();
    let source = match request
        .headers()
        .get("x-dm-source")
        .and_then(|s| s.to_str().ok())
    {
        Some("cli") => "cli",
        Some("daemon") => "daemon",
        Some("web") => "web",
        Some("sdk") => "sdk",
        _ => "api",
    };
    let request_id = request
        .headers()
        .get("x-request-id")
        .and_then(|s| s.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let start = Instant::now();
    let response = next.run(request).await;
    if !route.ends_with("/telemetry") {
        record(
            &state,
            source,
            "http.completed",
            json!({"route":route,"method":method,"request_id":request_id,"status":response.status().as_u16(),"success":response.status().is_success(),"duration_ms":u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX)}),
        );
    }
    response
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClientEvent {
    source: String,
    event: String,
    success: bool,
    duration_ms: u64,
}
pub(crate) async fn receive(
    State(state): State<AppState>,
    headers: HeaderMap,
    Authenticated(_): Authenticated,
    ApiJson(event): ApiJson<ClientEvent>,
) -> AppResult<StatusCode> {
    if !["cli", "sdk", "daemon", "web"].contains(&event.source.as_str())
        || ![
            "command",
            "connection",
            "callback",
            "queue",
            "update",
            "page_view",
            "request",
            "web_error",
        ]
        .contains(&event.event.as_str())
        || event.duration_ms > 604_800_000
    {
        return Err(AppError::validation(
            "unknown diagnostic source/event or duration above seven days",
        ));
    }
    if requested(&headers) {
        record(
            &state,
            &event.source,
            &event.event,
            json!({"success":event.success,"duration_ms":event.duration_ms}),
        );
    }
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod diagnostic_tests {
    use super::*;
    #[test]
    fn secrets_and_arbitrary_content_cannot_enter_diagnostic_context() {
        let value = json!({"message":"private message","token":"oat_secret","app_secret":"ask_secret","url":"https://callback.invalid/private","request_id":"ask_secret","status":503,"success":false,"duration_ms":29,"route":"/api/v1/conversations/{conversation_id}","stage":"ack","session_id":uuid::Uuid::nil()});
        let filtered = safe_fields(value);
        assert_eq!(filtered.len(), 6);
        assert!(
            !serde_json::to_string(&filtered).is_ok_and(|s| s.contains("secret")
                || s.contains("private message")
                || s.contains("callback.invalid"))
        );
        assert!(serde_json::from_value::<ClientEvent>(json!({"source":"cli","event":"command","success":false,"duration_ms":0,"token":"private"})).is_err());
    }
}
