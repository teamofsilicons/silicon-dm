//! `POST /webhook/`: Silicon Accounts app events for DM.
//!
//! The signature is checked over the exact raw body (5-minute tolerance), each event is
//! applied at most once (dedupe on `event_id`), and out-of-order deliveries never move
//! the cache backwards (ids and custodians by `occurred_at`, profiles by version).

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
};
use time::OffsetDateTime;

use crate::{
    AppResult,
    application::{
        ports::{AccountEvent, VerifiedEvent},
        state::AppState,
    },
    domain::{ActorId, ActorRef, ActorType},
    infrastructure::postgres::Applied,
};

/// Receives one signed delivery.
///
/// # Errors
/// 401 for a missing, stale or wrong signature; 400 for a body that is not an event;
/// storage failures (answered non-2xx so Accounts retries).
pub async fn receive(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> AppResult<StatusCode> {
    let event = state.identity.verify_webhook(&headers, &body)?;
    if state.store.webhook_event_seen(&event.event_id).await? {
        return Ok(StatusCode::NO_CONTENT);
    }
    let occurred_at = event.occurred_at.unwrap_or_else(OffsetDateTime::now_utc);
    let (account, outcome) = apply(&state, &event, occurred_at).await?;
    state
        .store
        .record_webhook_event(
            &event.event_id,
            &event.event_type,
            account.as_deref(),
            event.occurred_at,
            outcome,
        )
        .await?;
    crate::telemetry::record(
        &state,
        "backend",
        "accounts.webhook_applied",
        serde_json::json!({"event_type": event.event_type, "outcome": outcome, "success": true}),
    );
    Ok(StatusCode::NO_CONTENT)
}

async fn apply(
    state: &AppState,
    event: &VerifiedEvent,
    occurred_at: OffsetDateTime,
) -> AppResult<(Option<String>, &'static str)> {
    let store = &state.store;
    let outcome = |applied: Applied| match applied {
        Applied::Changed => "applied",
        Applied::Stale => "stale",
    };
    Ok(match &event.event {
        AccountEvent::IdChanged { uuid, kind, new_id } => (
            Some(uuid.clone()),
            outcome(
                store
                    .apply_id_change(uuid, *kind, new_id, occurred_at)
                    .await?,
            ),
        ),
        AccountEvent::Updated {
            uuid,
            account,
            version,
        } => match account {
            Some(account) => {
                let applied = store.apply_profile_update(account, *version).await?;
                if applied == Applied::Changed && account.custodian.is_some() {
                    custodian_conversation(state, uuid).await;
                }
                (Some(uuid.clone()), outcome(applied))
            }
            None => (Some(uuid.clone()), "ignored"),
        },
        AccountEvent::CustodianChanged { uuid, to } => match to {
            Some((custodian, custodian_id)) => {
                let applied = store
                    .apply_custodian_change(uuid, custodian, custodian_id, occurred_at)
                    .await?;
                if applied == Applied::Changed {
                    custodian_conversation(state, uuid).await;
                }
                (Some(uuid.clone()), outcome(applied))
            }
            None => (Some(uuid.clone()), "ignored"),
        },
        AccountEvent::SignedOut { uuid, reason } => {
            if reason.as_deref() == Some("app_revoked") {
                // DM itself revoked one refresh family (one machine's `dm logout`, or the
                // website's sign-out): the account's other sign-ins stay valid.
                (Some(uuid.clone()), "ignored")
            } else {
                let applied = store.revoke_sessions_before(uuid, occurred_at).await?;
                store.end_stored_proofs(uuid).await?;
                (Some(uuid.clone()), outcome(applied))
            }
        }
        AccountEvent::AccessRemoved { uuid } => {
            let applied = store.revoke_sessions_before(uuid, occurred_at).await?;
            store.end_stored_proofs(uuid).await?;
            (Some(uuid.clone()), outcome(applied))
        }
        AccountEvent::Deleted { uuid } => {
            store.delete_account(uuid, occurred_at).await?;
            (Some(uuid.clone()), "applied")
        }
        AccountEvent::Ping => (None, "applied"),
        AccountEvent::Unknown => {
            tracing::info!(event_type = %event.event_type, "ignoring an Accounts event type DM does not act on");
            (None, "ignored")
        }
    })
}

/// A custodian change creates the Silicon ↔ new custodian conversation (best effort; the
/// next sign-in of either account retries).
async fn custodian_conversation(state: &AppState, silicon: &str) {
    let actor = ActorRef {
        actor_type: ActorType::Silicon,
        id: ActorId::from_storage_key(silicon.to_owned()),
    };
    if let Err(error) = state
        .directory()
        .ensure_custodian_conversations(&actor)
        .await
    {
        tracing::warn!(
            code = error.code(),
            "creating the custodian conversation failed"
        );
    }
}
