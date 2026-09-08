//! Public IAM application-session bridge. Application secrets stay server-side.

use axum::{
    Json,
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use secrecy::SecretString;
use serde::Deserialize;
use serde_json::json;

use crate::{
    AppResult,
    api::extract::{ApiJson, Authenticated, Idempotency},
    application::state::AppState,
};

/// Login accepts only the short-lived token issued by IAM for DM.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoginInput {
    /// Single-use IAM application SLT; no password or direct IAM credentials.
    pub slt: SecretString,
}

/// Continues an existing IAM application session.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefreshInput {
    /// Current rotating application refresh token.
    pub refresh_token: SecretString,
}

/// Revokes a DM application token through IAM.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogoutInput {
    /// Refresh token for complete family logout, or access token for that access token.
    pub token: SecretString,
}

/// Exchanges an IAM SLT with user-selected organization grants using the selected plane's server-side app secret.
///
/// # Errors
/// Rejects invalid SLTs, IAM failures, or unavailable membership storage.
pub async fn login(
    State(state): State<AppState>,
    Idempotency(key): Idempotency,
    ApiJson(input): ApiJson<LoginInput>,
) -> AppResult<Response> {
    let session = state.identity.login(&input.slt, key.as_str()).await?;
    Ok((no_store(), Json(session)).into_response())
}

/// Rotates a refresh token; retry with exactly the same body and idempotency key.
///
/// # Errors
/// Rejects invalid refresh credentials, IAM failures, or unavailable storage.
pub async fn refresh(
    State(state): State<AppState>,
    Idempotency(key): Idempotency,
    ApiJson(input): ApiJson<RefreshInput>,
) -> AppResult<Response> {
    let session = state
        .identity
        .refresh(&input.refresh_token, key.as_str())
        .await?;
    Ok((no_store(), Json(session)).into_response())
}

/// Revokes the supplied token and immediately rechecks local realtime connections.
///
/// # Errors
/// Propagates IAM revocation and durable invalidation failures.
pub async fn logout(
    State(state): State<AppState>,
    Idempotency(key): Idempotency,
    ApiJson(input): ApiJson<LogoutInput>,
) -> AppResult<Response> {
    state.identity.logout(&input.token, key.as_str()).await?;
    sqlx::query("UPDATE iam_authorization_revision SET revision = revision + 1 WHERE singleton")
        .execute(state.store.pool())
        .await?;
    state.realtime.invalidate_authorization();
    Ok((StatusCode::NO_CONTENT, no_store()).into_response())
}

/// Reports the current IAM identity and disclosed effective organization authority.
pub async fn me(Authenticated(context): Authenticated) -> Response {
    (
        no_store(),
        Json(json!({
            "actor": context.actor,
            "organization_id": context.organization_id,
            "principal_id": context.principal_id,
            "session_id": context.session_id,
            "org_role": context.org_role,
            "capabilities": context.capabilities,
        })),
    )
        .into_response()
}

fn no_store() -> HeaderMap {
    HeaderMap::from_iter([
        (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
        (header::PRAGMA, HeaderValue::from_static("no-cache")),
    ])
}
