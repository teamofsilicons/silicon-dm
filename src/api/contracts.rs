//! Local API lifecycle accounting, negotiation, and compatibility discovery.
use crate::{AppResult, application::state::AppState};
use axum::{
    Json,
    extract::{Request, State},
    http::{HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse as _, Response},
};
use serde_json::{Value, json};

/// The HTTP contract this build speaks (`x-dm-contract-version`).
pub(crate) const HTTP_CONTRACT_VERSION: i32 = 4;

pub(super) fn is_retired_socket(path: &str) -> bool {
    matches!(path, "/api/v1/ws" | "/api/v1/ws/shared")
}

/// How updates reach clients: Ting when configured, otherwise `/api/v1/sync` (with a
/// long-poll), which works either way.
pub(super) fn delivery(state: &AppState) -> Value {
    let ting = state.settings.ting.base_url.as_ref();
    json!({
        "transport": if ting.is_some() { "ting" } else { "sync" },
        "ting_enabled": ting.is_some(),
        "ting_api_base_url": ting,
        "enrolment": if ting.is_some() { "automatic on sign-in (User verification proof, scope tings.subscribe)" } else { "none: Ting is not configured for this DM" },
        "sync_path": "/api/v1/sync",
        "sync_long_poll_seconds": super::sync::MAX_WAIT_SECONDS,
        "presence_path": "/api/v1/presence/devices/{device_id}",
        "receipts_path": "/api/v1/conversations/{conversation_id}/messages/{message_id}/receipts",
        "dm_websocket_supported": false
    })
}

fn retirement_response(delivery: &Value) -> Response {
    (StatusCode::GONE, Json(json!({
        "error":{
            "code":"delivery_moved",
            "message":"DM WebSocket delivery has retired. Receive updates with GET /api/v1/sync?wait=30 (or through Ting where configured). Use DM HTTP routes for messages, receipts, and presence."
        },
        "delivery":delivery
    }))).into_response()
}

pub(super) async fn retired(State(state): State<AppState>) -> Response {
    retirement_response(&delivery(&state))
}

fn selected(request: &Request) -> Result<(&'static str, i32), &'static str> {
    let (family, current, header) = ("http", HTTP_CONTRACT_VERSION, "x-dm-contract-version");
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
        Err(message)=>return (StatusCode::NOT_ACCEPTABLE,Json(json!({"error":{"code":"unsupported_contract","message":message},"compatible":{"http":[HTTP_CONTRACT_VERSION]}}))).into_response(),
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
        .insert("x-dm-contract-version", HeaderValue::from_static("4"));
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
        json!({"service":"silicon-dm","service_version":env!("CARGO_PKG_VERSION"),"contracts":rows,"compatibility":[{"http":HTTP_CONTRACT_VERSION}],"identity":{"provider":"silicon-accounts","accounts_url":state.settings.accounts.issuer,"app_id":state.settings.accounts.app_id,"bearer":"Silicon Accounts access token issued to the app","proof_scopes":crate::config::PROOF_SCOPES},"delivery":delivery(&state),"retired_routes":["/api/v1/ws","/api/v1/ws/shared","/api/v1/auth/*","/api/v1/iam","/api/v1/testing-environments*","/api/v1/delivery/authorization*","/api/v1/delivery/registration","/api/v1/telemetry"],"features":{"groups":{"minimum_client":"0.15.0","id_format":"g:{name-slug} (groups created before contract 4 keep a three-part id such as g:tos:product-design)","ids_immutable":true,"legacy_uuid_aliases":true,"owners":"co-equal","invitations":"accept or decline","guide":"https://docs.dm.teamofsilicons.com/groups/"},"accounts":{"actor_refs":"{type, id: current c:/si: id, uuid}","custodians":"read their Silicons' conversations","silicon_allow_list":"/api/v1/allow-list"}},"policy":{"breaking_changes":"breaking wire changes require a coordinated client upgrade; contract 4 moved identity to Silicon Accounts; unsupported explicit HTTP versions are rejected","additive_changes":"optional fields only","sunset_after_idle_days":7,"deprecation_required":true},"docs":"https://docs.dm.teamofsilicons.com/contracts/"}),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn negotiation_rejects_unsupported_and_duplicate_versions()
    -> Result<(), Box<dyn std::error::Error>> {
        let (path, header, good) = ("/api/v1/me", "x-dm-contract-version", "4");
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
        let delivery = json!({"sync_path": "/api/v1/sync", "dm_websocket_supported": false});
        for path in ["/api/v1/ws", "/api/v1/ws/shared"] {
            assert!(is_retired_socket(path));
            let delivery = delivery.clone();
            let router = Router::new()
                .route(
                    path,
                    get(move || async move { retirement_response(&delivery) }),
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
            assert_eq!(body["data"]["error"]["code"], "delivery_moved");
            assert_eq!(body["data"]["delivery"]["sync_path"], "/api/v1/sync");
            assert_eq!(body["data"]["delivery"]["dm_websocket_supported"], false);
        }
        assert!(!is_retired_socket("/api/v1/sync"));
        Ok(())
    }
}
