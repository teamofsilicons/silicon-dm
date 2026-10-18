//! Durable DM-to-Ting handoff processing, independent of connected DM clients.
//!
//! With Ting configured, pending handoffs are sent with DM's App verification proof; with
//! Ting not configured (an explicit polling-only deployment), they are
//! marked discarded with a reason, and clients sync over `/api/v1/sync`. Maintenance keeps
//! the outbox to its retention window: events and finished handoffs older than
//! `SYNC_RETENTION` go; a handoff still waiting to be sent never does.

use std::{future::pending, time::Duration};

use sqlx::postgres::PgListener;
use time::OffsetDateTime;
use tokio::time::{Instant, MissedTickBehavior, interval, interval_at, timeout_at};
use tokio_util::sync::CancellationToken;

use crate::{
    AppError, AppResult,
    application::state::AppState,
    infrastructure::{
        postgres::{DISCARD_TING_NOT_CONFIGURED, SYNC_RETENTION, TingDeliveryContext},
        ting::{SharedTingPublisher, TingFailure},
    },
};

const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(60);
const DISCARD_BATCH: i64 = 500;

/// Persists handoff acceptance only after Ting confirms durable storage.
/// PostgreSQL notifications reduce latency; durable polling remains authoritative.
pub struct TingDeliveryWorker {
    state: AppState,
    publisher: Option<SharedTingPublisher>,
    context: TingDeliveryContext,
}

impl TingDeliveryWorker {
    /// A worker that sends through `publisher`, or (`None`, Ting not configured) marks
    /// handoffs discarded.
    #[must_use]
    pub fn new(state: AppState, publisher: Option<SharedTingPublisher>) -> Self {
        let context = state.ting_delivery_context();
        Self {
            state,
            publisher,
            context,
        }
    }

    /// Runs until shutdown. A pass that fails on the database is logged and tried again on
    /// the next tick; publisher failures leave handoffs retryable and never fail a DM message
    /// or fabricate a receipt.
    ///
    /// # Errors
    /// Returns a configuration failure (a setting that can never work).
    pub async fn run(&self, cancellation: CancellationToken) -> AppResult<()> {
        if self.publisher.is_none() {
            tracing::info!(
                "Ting delivery is disabled (DM_TING_URL is not set): handoffs are kept and marked discarded; clients sync with GET /api/v1/sync"
            );
        }
        let Some(mut listener) = cancellation
            .run_until_cancelled(self.open_delivery_listener())
            .await
        else {
            return Ok(());
        };
        let settings = &self.state.settings.worker;
        let mut poll = interval(settings.poll_interval.max(Duration::from_millis(10)));
        poll.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut maintenance =
            interval_at(Instant::now() + MAINTENANCE_INTERVAL, MAINTENANCE_INTERVAL);
        maintenance.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            let maintain = tokio::select! {
                biased;
                () = cancellation.cancelled() => return Ok(()),
                _ = maintenance.tick() => true,
                _ = poll.tick() => false,
                notification = receive_delivery_wakeup(&mut listener) => {
                    if notification.is_err() {
                        tracing::warn!(failure = "delivery_listener",
                            "PostgreSQL Ting wakeup listener disconnected; polling remains active");
                        listener = None;
                    }
                    false
                }
            };
            let result = if maintain {
                let Some(result) = cancellation.run_until_cancelled(self.maintain_once()).await
                else {
                    return Ok(());
                };
                if listener.is_none() {
                    let Some(reconnected) = cancellation
                        .run_until_cancelled(self.open_delivery_listener())
                        .await
                    else {
                        return Ok(());
                    };
                    listener = reconnected;
                }
                result
            } else {
                let Some(result) = cancellation.run_until_cancelled(self.process_once()).await
                else {
                    return Ok(());
                };
                result.map(|_| ())
            };
            match result {
                Ok(()) => {}
                // A setting that can never work stops the worker (and its process) at once.
                Err(error @ AppError::Validation(_)) => return Err(error),
                // A database hiccup (a statement timeout, a pool that is busy for a moment, a
                // failover) must not stop delivery for good, nor take the API process that
                // runs this worker down with it: the next tick tries again.
                Err(error) => {
                    tracing::warn!(code = error.code(), "Ting worker pass failed; retrying");
                }
            }
        }
    }

    async fn open_delivery_listener(&self) -> Option<PgListener> {
        match self.state.store.subscribe_delivery_wakeups().await {
            Ok(listener) => listener,
            Err(error) => {
                tracing::warn!(
                    code = error.code(),
                    failure = "delivery_listener",
                    "PostgreSQL Ting wakeup listener is unavailable; polling remains active"
                );
                None
            }
        }
    }

    /// Expires presence and retained delivery/idempotency state; sunsets idle contracts.
    ///
    /// # Errors
    /// Reports storage failures.
    pub async fn maintain_once(&self) -> AppResult<()> {
        let store = &self.state.store;
        sqlx::query("UPDATE contract_versions SET status='sunset',sunset_at=clock_timestamp() WHERE status='deprecated' AND GREATEST(COALESCE(last_request_at,introduced_at),deprecated_at)<=clock_timestamp()-INTERVAL '7 days'")
            .execute(store.pool()).await?;
        let expired_http_leases = store.expire_http_presence_leases().await?;
        let compacted = store.compact_delivery_state().await?;
        let before = OffsetDateTime::now_utc()
            - time::Duration::try_from(SYNC_RETENTION).map_err(AppError::internal)?;
        let pruned = store.prune_sync_history(before).await?;
        if expired_http_leases > 0 || compacted > 0 || pruned.total() > 0 {
            tracing::info!(
                expired_http_leases,
                compacted_rows = compacted,
                pruned_deliveries = pruned.deliveries,
                pruned_conversation_events = pruned.conversation_events,
                pruned_ting_handoffs = pruned.ting_handoffs,
                "Ting worker maintenance completed"
            );
        }
        Ok(())
    }

    /// Runs one bounded batch. Only one row is leased at a time, so slow sends
    /// cannot consume the leases of other rows waiting in an in-memory batch.
    ///
    /// # Errors
    /// Returns durable-storage/configuration failures; lost leases are harmless
    /// and are recovered by the next owner with the unchanged event key/body.
    pub async fn process_once(&self) -> AppResult<usize> {
        let Some(publisher) = &self.publisher else {
            let discarded = self
                .state
                .store
                .discard_ting_handoffs(DISCARD_TING_NOT_CONFIGURED, DISCARD_BATCH)
                .await?;
            return Ok(usize::try_from(discarded).unwrap_or(usize::MAX));
        };
        let settings = &self.state.settings.worker;
        let mut processed = 0;
        let reserve = Duration::from_millis(500);
        let available = settings
            .lease_duration
            .checked_sub(reserve)
            .filter(|duration| !duration.is_zero())
            .ok_or_else(|| AppError::validation("Ting lease must exceed 500 milliseconds"))?;
        for _ in 0..settings.batch_size.get().min(100) {
            let deadline = Instant::now() + available;
            let Some(claim) = self
                .state
                .store
                .claim_ting_deliveries(
                    &self.state.instance_id,
                    1,
                    settings.lease_duration,
                    &self.context,
                )
                .await?
                .pop()
            else {
                break;
            };
            let authorized = self.state.store.ting_delivery_authorized(&claim).await?;
            let outcome = if Instant::now() >= deadline {
                Err(TingFailure::Timeout)
            } else if authorized {
                timeout_at(deadline, publisher.publish(&claim))
                    .await
                    .unwrap_or(Err(TingFailure::Timeout))
            } else {
                Err(TingFailure::Rejected(
                    "ting_recipient_no_longer_participant".into(),
                ))
            };
            let stored = match outcome {
                Ok(acceptance) => {
                    self.state
                        .store
                        .accept_ting_delivery(
                            claim.delivery_id,
                            claim.lease_id,
                            &acceptance.id,
                            acceptance.created_at,
                            acceptance.silent,
                        )
                        .await
                }
                Err(error) => {
                    if matches!(&error, TingFailure::Rejected(code) if code == "recipient_not_registered")
                    {
                        let state = self.state.clone();
                        let target = claim.target.clone();
                        tokio::spawn(async move {
                            if let Err(error) =
                                crate::infrastructure::ting_enrollment::reenrol(&state, &target)
                                    .await
                            {
                                tracing::warn!(
                                    code = error.code(),
                                    "re-enrolling a Ting recipient failed"
                                );
                            }
                        });
                    }
                    let delay = retry_delay(claim.attempt_count, settings.max_retry_delay);
                    let next = OffsetDateTime::now_utc()
                        + time::Duration::try_from(delay).map_err(AppError::internal)?;
                    tracing::warn!(delivery_id = %claim.delivery_id, code = error.code(),
                        "Ting handoff remains pending");
                    self.state
                        .store
                        .retry_ting_delivery(claim.delivery_id, claim.lease_id, next, error.code())
                        .await
                }
            };
            if let Err(error) = stored {
                if matches!(error, AppError::Conflict(_)) {
                    tracing::debug!(delivery_id = %claim.delivery_id,
                        "Ting lease changed before progress could be recorded");
                } else {
                    return Err(error);
                }
            }
            processed += 1;
        }
        Ok(processed)
    }
}

async fn receive_delivery_wakeup(listener: &mut Option<PgListener>) -> Result<(), sqlx::Error> {
    match listener {
        Some(listener) => listener.recv().await.map(|_| ()),
        None => pending().await,
    }
}

fn retry_delay(attempt: i64, maximum: Duration) -> Duration {
    let shift = u32::try_from(attempt).unwrap_or(31).min(31);
    Duration::from_secs(1_u64 << shift).min(maximum.max(Duration::from_secs(1)))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::retry_delay;

    #[test]
    fn refused_handoffs_back_off_exponentially_up_to_the_maximum() {
        let maximum = Duration::from_secs(300);
        assert_eq!(retry_delay(0, maximum), Duration::from_secs(1));
        assert_eq!(retry_delay(1, maximum), Duration::from_secs(2));
        assert_eq!(retry_delay(5, maximum), Duration::from_secs(32));
        assert_eq!(retry_delay(9, maximum), maximum);
        assert_eq!(retry_delay(i64::MAX, maximum), maximum);
        assert_eq!(
            retry_delay(-1, maximum),
            maximum,
            "a corrupt count never busy-loops"
        );
        assert_eq!(
            retry_delay(3, Duration::ZERO),
            Duration::from_secs(1),
            "never below a second"
        );
    }
}
