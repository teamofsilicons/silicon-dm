//! Authenticated IAM webhook receipt and per-plane realtime revalidation.

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
};
use secrecy::{ExposeSecret as _, SecretString};
use serde::Deserialize;
use silicon_iam_client::models::WebhookEvent;

use crate::{AppError, AppResult, application::state::AppState};

/// Receives the exact bytes signed by IAM at `POST /webhook/`.
/// Signature, timestamp, key version, event ID, and testing envelope are verified
/// before any event is persisted, acted on, or acknowledged.
///
/// # Errors
/// Rejects unauthenticated deliveries and propagates atomic projection failures.
pub async fn receive(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> AppResult<StatusCode> {
    if body.len() > silicon_iam_client::webhook::DEFAULT_MAX_WEBHOOK_BODY_BYTES {
        return Err(AppError::Unauthorized);
    }
    if let Ok(hint) = serde_json::from_slice::<TestingRoutingHint>(&body) {
        // This bounded, secret-redacted value selects candidate verifiers only.
        // The registry verifies the exact outer signature and SDK key binding
        // before building a runtime or returning any authoritative event.
        let key = &hint.test.testing_key;
        if key.expose_secret().len() != 32
            || !key
                .expose_secret()
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric())
        {
            return Err(AppError::Unauthorized);
        }
        let registry = state.testing.as_ref().ok_or(AppError::Unauthorized)?;
        let verified = registry
            .verified_webhook_states(&state, &headers, &body, key)
            .await?;
        if verified.is_empty() {
            return Err(AppError::Unauthorized);
        }
        for (environment, event) in verified {
            accept(&environment, &event).await?;
        }
        return Ok(StatusCode::NO_CONTENT);
    }
    let event = state
        .identity
        .verify_webhook(&headers, &body)
        .map_err(|_| AppError::Unauthorized)?;
    accept(&state, &event).await
}

#[derive(Deserialize)]
struct TestingRoutingHint {
    test: TestingKeyHint,
}

#[derive(Deserialize)]
struct TestingKeyHint {
    testing_key: SecretString,
}

async fn accept(state: &AppState, event: &WebhookEvent) -> AppResult<StatusCode> {
    // A clean/delete/rotation cannot cross a verified callback's commit.
    let _fence = match (
        &state.testing,
        state.testing_environment,
        state.testing_generation,
    ) {
        (Some(registry), Some(id), Some(generation)) => {
            Some(registry.request_fence(id, generation).await?)
        }
        _ => None,
    };
    let mut tx = state.store.pool().begin().await?;
    let inserted = sqlx::query("INSERT INTO iam_webhook_receipts (event_id, event_type, occurred_at) VALUES ($1, $2, $3) ON CONFLICT (event_id) DO NOTHING")
        .bind(event.event_id).bind(&event.event_type).bind(event.occurred_at)
        .execute(&mut *tx).await?.rows_affected();
    if inserted != 0 {
        crate::infrastructure::postgres::PostgresStore::project_iam_webhook(&mut tx, event).await?;
        sqlx::query(
            "UPDATE iam_authorization_revision SET revision = revision + 1 WHERE singleton",
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    // Duplicate delivery also signals: a prior process might have died after commit.
    state.realtime.invalidate_authorization();
    Ok(StatusCode::NO_CONTENT)
}

/// Current durable IAM invalidation generation for this selected database plane.
/// Other backend processes observe webhook-driven changes during their replay tick.
pub(crate) async fn authorization_revision(state: &AppState) -> AppResult<i64> {
    Ok(
        sqlx::query_scalar("SELECT revision FROM iam_authorization_revision WHERE singleton")
            .fetch_one(state.store.pool())
            .await?,
    )
}
