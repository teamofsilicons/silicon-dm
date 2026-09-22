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
    remember_session(&state, &session);
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
    remember_session(&state, &session);
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
            "member": context.actor,
            "organization_id": context.organization_id,
            // DM clients historically require this string. It now contains
            // the same canonical actor handle, never an IAM identity UUID.
            "principal_id": context.actor.id,
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

/// Returns public IAM application discovery without requiring a session.
/// # Errors
/// Rejects an unavailable selected sandbox or storage failure.
pub async fn iam(State(state): State<AppState>) -> AppResult<Response> {
    let environment = match (&state.testing, state.testing_environment) {
        (Some(registry), Some(id)) => Some(registry.selected_metadata(id).await?),
        _ => None,
    };
    Ok((
        no_store(),
        Json(json!({
            "app_id": state.settings.iam.app_id,
            "iam_base_url": state.settings.iam.base_url,
            "api_base_url": state.settings.server.public_base_url,
            "testing_environment_id": state.testing_environment,
            "testing_generation": state.testing_generation,
            "testing_environment": environment,
            "delivery": super::contracts::delivery(&state.settings.ting),
        })),
    )
        .into_response())
}

// Deliver the new refresh token immediately. Optional capture must not consume
// the HTTP response deadline after IAM has already rotated the token family.
fn remember_session(state: &AppState, session: &crate::application::auth::ApplicationSession) {
    let state = state.clone();
    // Only the access token crosses into the background job, never the refresh token.
    let token = SecretString::from(session.access_token.clone());
    let actor = session.actor.clone();
    let organizations = session.organization_ids.clone();
    tokio::spawn(async move {
        let capture = async {
            // The response's request fence may already be gone. Reacquire it so
            // clean/delete cannot race a write into an obsolete generation.
            let _fence = match (state.testing_environment, state.testing_generation) {
                (None, None) => None,
                (Some(id), Some(generation)) => Some(
                    state
                        .testing
                        .as_ref()
                        .ok_or(crate::AppError::Unauthorized)?
                        .request_fence(id, generation)
                        .await?,
                ),
                _ => return Err(crate::AppError::Unauthorized),
            };
            let cache = state.ting_credentials()?;
            for organization_id in &organizations {
                let context = state
                    .identity
                    .authenticate(crate::application::ports::AuthenticationRequest::Bearer {
                        token: &token,
                        organization_id,
                    })
                    .await?;
                if context.actor != actor || context.organization_id != *organization_id {
                    return Err(crate::AppError::Forbidden);
                }
                cache.remember(&context).await?;
            }
            Ok::<(), crate::AppError>(())
        };
        match tokio::time::timeout(std::time::Duration::from_secs(30), capture).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => tracing::warn!(
                code = error.code(),
                "Ting authority will be captured on the next DM request"
            ),
            Err(_) => {
                tracing::warn!("Ting authority capture timed out; the next DM request can retry");
            }
        }
    });
}
