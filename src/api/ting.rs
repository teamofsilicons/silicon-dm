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
