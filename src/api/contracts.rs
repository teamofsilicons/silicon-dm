//! Local API lifecycle accounting, negotiation, and compatibility discovery.
use crate::{AppResult, application::state::AppState};
use axum::{
    Json,
    extract::{Request, State},
    http::{HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse as _, Response},
};
use serde_json::json;

fn selected(request: &Request) -> Result<(&'static str, i32), &'static str> {
    let path = request.uri().path();
    let (family, current, header) = if path.ends_with("/ws/shared") {
        ("shared", 1, "x-dm-protocol-version")
    } else if path.ends_with("/ws") {
        ("websocket", 4, "x-dm-protocol-version")
    } else {
        ("http", 2, "x-dm-contract-version")
    };
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
    if !request.uri().path().starts_with("/api/v1/") {
        return next.run(request).await;
    }
    let (family,version)=match selected(&request) {
        Ok(selected)=>selected,
        Err(message)=>return (StatusCode::NOT_ACCEPTABLE,Json(json!({"error":{"code":"unsupported_contract","message":message},"compatible":{"http":[2],"websocket":[4],"shared":[1]}}))).into_response(),
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
        .insert("x-dm-contract-version", HeaderValue::from_static("2"));
    response.headers_mut().insert(
        "x-dm-protocol-version",
        HeaderValue::from_static(if family == "websocket" { "4" } else { "1" }),
    );
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
        json!({"service":"silicon-dm","service_version":env!("CARGO_PKG_VERSION"),"contracts":rows,"compatibility":[{"http":2,"websocket":4,"shared":1,"minimum_client":"0.8.0"}],"features":{"groups":{"minimum_client":"0.7.0","id_format":"g:{organization}:{creation-name-slug}","ids_immutable":true,"legacy_uuid_aliases":true,"guide":"https://docs.dm.teamofsilicons.com/groups/"}},"policy":{"breaking_changes":"new contract version; existing consumers retain their negotiated shape","additive_changes":"optional fields only","sunset_after_idle_days":7,"deprecation_required":true},"docs":"https://docs.dm.teamofsilicons.com/contracts/"}),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn negotiation_rejects_unsupported_and_duplicate_versions()
    -> Result<(), Box<dyn std::error::Error>> {
        for (path, header, good) in [
            ("/api/v1/iam", "x-dm-contract-version", "2"),
            ("/api/v1/ws", "x-dm-protocol-version", "4"),
            ("/api/v1/ws/shared", "x-dm-protocol-version", "1"),
        ] {
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
        }
        Ok(())
    }
}
