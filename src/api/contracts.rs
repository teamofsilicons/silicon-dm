//! Local API lifecycle accounting, negotiation, and compatibility discovery.
use crate::{AppResult, application::state::AppState, config::TingSettings};
use axum::{
    Json,
    extract::{Request, State},
    http::{HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse as _, Response},
};
use serde_json::{Value, json};

pub(super) fn is_retired_socket(path: &str) -> bool {
    matches!(path, "/api/v1/ws" | "/api/v1/ws/shared")
}

pub(super) fn delivery(settings: &TingSettings) -> Value {
    json!({
        "transport":"ting",
        "app_id":"ting",
        "api_base_url":settings.base_url,
        "browser_origin":"https://ting.teamofsilicons.com",
        "receiver_authentication":"ting_session",
        "publisher_authority":"originating_dm_session",
        "registration_path":"/api/v1/delivery/registration",
        "sync_path":"/api/v1/sync",
        "presence_path":"/api/v1/presence/devices/{device_id}",
        "receipts_path":"/api/v1/conversations/{conversation_id}/messages/{message_id}/receipts",
        "dm_websocket_supported":false
    })
}

fn retirement_response(settings: &TingSettings) -> Response {
    (StatusCode::GONE, Json(json!({
        "error":{
            "code":"delivery_moved_to_ting",
            "message":"DM WebSocket delivery has retired. Enroll with POST /api/v1/delivery/registration, receive updates through Ting, and recover through GET /api/v1/sync. Use DM HTTP routes for messages, receipts, and presence."
        },
        "delivery":delivery(settings)
    }))).into_response()
}

pub(super) async fn retired(State(state): State<AppState>) -> Response {
    retirement_response(&state.settings.ting)
}

fn selected(request: &Request) -> Result<(&'static str, i32), &'static str> {
    let (family, current, header) = ("http", 3, "x-dm-contract-version");
    let mut values = request.headers().get_all(header).iter();
    let first = values.next();
    if values.next().is_some() {
        return Err("Provide exactly one contract version.");
    }
    if let Some(value) = first {
        let version = value.to_str().ok().and_then(|v| v.parse::<i32>().ok());
        if version != Some(current) {
            return Err(
                "Unsupported contract version. Read /api/v1/contracts for compatible versions.",
            );
        }
    }
    Ok((family, current))
}

pub(super) async fn negotiate(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    if is_retired_socket(request.uri().path()) {
        return retired(State(state)).await;
    }
    if !request.uri().path().starts_with("/api/v1/") {
        return next.run(request).await;
    }
    let (family,version)=match selected(&request) {
        Ok(selected)=>selected,
        Err(message)=>return (StatusCode::NOT_ACCEPTABLE,Json(json!({"error":{"code":"unsupported_contract","message":message},"compatible":{"http":[3]}}))).into_response(),
    };
    if !request.uri().path().ends_with("/contracts") {
        match admit(&state,family,version).await {
            Ok(true)=>{},
            Ok(false)=>return (StatusCode::GONE,Json(json!({"error":{"code":"contract_sunset","message":"This contract has been retired. Read /api/v1/contracts."}}))).into_response(),
            Err(error)=>return error.into_response(),
        }
    }
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert("x-dm-contract-version", HeaderValue::from_static("3"));
    response
}

/// Atomically admit a request, or sunset an already-deprecated unused contract.
/// # Errors
/// Returns storage failures without accepting untracked requests.
pub(crate) async fn admit(state: &AppState, family: &str, version: i32) -> AppResult<bool> {
    let mut tx = state.store.pool().begin().await?;
    let status: Option<String> = sqlx::query_scalar(
        "SELECT status FROM contract_versions WHERE family=$1 AND version=$2 FOR UPDATE",
    )
    .bind(family)
    .bind(version)
    .fetch_optional(&mut *tx)
    .await?;
    if status.as_deref() == Some("deprecated") {
        sqlx::query("UPDATE contract_versions SET status='sunset',sunset_at=clock_timestamp() WHERE family=$1 AND version=$2 AND GREATEST(COALESCE(last_request_at,introduced_at),deprecated_at)<=clock_timestamp()-INTERVAL '7 days'")
            .bind(family).bind(version).execute(&mut *tx).await?;
    }
    let changed=sqlx::query("UPDATE contract_versions SET requests=requests+1,last_request_at=clock_timestamp() WHERE family=$1 AND version=$2 AND status!='sunset'")
        .bind(family).bind(version).execute(&mut *tx).await?.rows_affected();
    tx.commit().await?;
    Ok(changed == 1)
}

pub(super) async fn describe(State(state): State<AppState>) -> AppResult<Json<serde_json::Value>> {
    // This also makes retirement progress when only discovery is requested.
    sqlx::query("UPDATE contract_versions SET status='sunset',sunset_at=clock_timestamp() WHERE status='deprecated' AND GREATEST(COALESCE(last_request_at,introduced_at),deprecated_at)<=clock_timestamp()-INTERVAL '7 days'").execute(state.store.pool()).await?;
    let rows: Vec<serde_json::Value> =
        sqlx::query_scalar("SELECT to_jsonb(c) FROM contract_versions c ORDER BY family,version")
            .fetch_all(state.store.pool())
            .await?;
    Ok(Json(
        json!({"service":"silicon-dm","service_version":env!("CARGO_PKG_VERSION"),"contracts":rows,"compatibility":[{"http":3}],"delivery":delivery(&state.settings.ting),"retired_routes":["/api/v1/ws","/api/v1/ws/shared"],"features":{"groups":{"minimum_client":"0.7.0","id_format":"g:{organization}:{creation-name-slug}","ids_immutable":true,"legacy_uuid_aliases":true,"guide":"https://docs.dm.teamofsilicons.com/groups/"}},"policy":{"breaking_changes":"breaking wire changes require a coordinated client upgrade; retired DM socket routes return 410 delivery_moved_to_ting; unsupported explicit HTTP versions are rejected","additive_changes":"optional fields only","sunset_after_idle_days":7,"deprecation_required":true},"docs":"https://docs.dm.teamofsilicons.com/contracts/"}),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn negotiation_rejects_unsupported_and_duplicate_versions()
    -> Result<(), Box<dyn std::error::Error>> {
        let (path, header, good) = ("/api/v1/iam", "x-dm-contract-version", "3");
        let req = Request::builder()
            .uri(path)
            .header(header, good)
            .body(axum::body::Body::empty())?;
        assert!(selected(&req).is_ok());
        let req = Request::builder()
            .uri(path)
            .header(header, "999")
            .body(axum::body::Body::empty())?;
        assert!(selected(&req).is_err());
        let req = Request::builder()
            .uri(path)
            .header(header, good)
            .header(header, good)
            .body(axum::body::Body::empty())?;
        assert!(selected(&req).is_err());
        Ok(())
    }

    #[tokio::test]
    async fn retired_sockets_return_actionable_http_error_envelopes()
    -> Result<(), Box<dyn std::error::Error>> {
        use axum::{Router, body::Body, routing::get};
        use tower::ServiceExt as _;
        let settings = TingSettings {
            base_url: "https://backend.ting.teamofsilicons.com".parse()?,
            request_timeout: std::time::Duration::from_secs(5),
        };
        for path in ["/api/v1/ws", "/api/v1/ws/shared"] {
            assert!(is_retired_socket(path));
            let settings = settings.clone();
            let router = Router::new()
                .route(
                    path,
                    get(move || async move { retirement_response(&settings) }),
                )
                .layer(axum::middleware::from_fn(silicon_dm_protocol::responses));
            let response = router
                .oneshot(Request::builder().uri(path).body(Body::empty())?)
                .await?;
            assert_eq!(response.status(), StatusCode::GONE);
            assert!(!response.headers().contains_key("x-dm-protocol-version"));
            let body: Value = serde_json::from_slice(
                &axum::body::to_bytes(response.into_body(), 16 * 1024).await?,
            )?;
            assert_eq!(body["type"], "error");
            assert_eq!(body["data"]["error"]["code"], "delivery_moved_to_ting");
            assert_eq!(body["data"]["delivery"]["sync_path"], "/api/v1/sync");
            assert_eq!(body["data"]["delivery"]["dm_websocket_supported"], false);
        }
        assert!(!is_retired_socket("/api/v1/sync"));
        Ok(())
    }
}
