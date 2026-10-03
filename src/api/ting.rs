//! Explicit recipient enrollment for Ting delivery.

use axum::{Json, extract::State};
use serde::Deserialize;
use serde_json::Value;

use crate::{
    AppResult,
    api::extract::{ApiJson, Authenticated, Idempotency},
    application::state::AppState,
    infrastructure::ting_enrollment,
};

/// Enrollment always targets the authenticated actor and selected organization.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistrationInput {}

/// Explicitly registers or reconnects this recipient's consented Ting grant.
///
/// # Errors
/// Rejects missing consent, uncertain earlier attempts, changed retry input and
/// unavailable upstream services. Never stores actor tokens or OBO proofs.
pub async fn register_delivery(
    State(state): State<AppState>,
    Authenticated(context): Authenticated,
    Idempotency(key): Idempotency,
    ApiJson(_input): ApiJson<RegistrationInput>,
) -> AppResult<Json<Value>> {
    ting_enrollment::register_cached(&state, &context, &key)
        .await
        .map(Json)
}

/// Starts separately reviewed Ting endpoint permission.
pub async fn authorize(
    State(state): State<AppState>,
    Authenticated(auth): Authenticated,
    Idempotency(key): Idempotency,
    ApiJson(_): ApiJson<RegistrationInput>,
) -> AppResult<Json<Value>> {
    state
        .ting_authorization()?
        .begin(state.identity.as_ref(), &auth, key.as_str())
        .await
        .map(Json)
}
/// One-time code returned after IAM consent; never an ordinary login token.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompletionInput {
    authorization_id: uuid::Uuid,
    authorization_code: String,
}
/// Stores separately issued endpoint tokens and then enrolls the recipient.
pub async fn complete(
    State(state): State<AppState>,
    Authenticated(auth): Authenticated,
    Idempotency(key): Idempotency,
    ApiJson(input): ApiJson<CompletionInput>,
) -> AppResult<Json<Value>> {
    state
        .ting_authorization()?
        .complete(
            state.identity.as_ref(),
            &auth,
            input.authorization_id,
            &input.authorization_code,
        )
        .await?;
    ting_enrollment::register_cached(&state, &auth, &key).await?;
    Ok(Json(serde_json::json!({"status":"authorized"})))
}
/// Reads this account's local delegation status.
pub async fn status(
    State(state): State<AppState>,
    Authenticated(auth): Authenticated,
) -> AppResult<Json<Value>> {
    state.ting_authorization()?.status(&auth).await.map(Json)
}
/// Stops DM's use of stored Ting permission; manage grant revocation in IAM.
pub async fn disconnect(
    State(state): State<AppState>,
    Authenticated(auth): Authenticated,
    Idempotency(_): Idempotency,
    ApiJson(_): ApiJson<RegistrationInput>,
) -> AppResult<Json<Value>> {
    state
        .ting_authorization()?
        .disconnect(&auth)
        .await
        .map(Json)
}
