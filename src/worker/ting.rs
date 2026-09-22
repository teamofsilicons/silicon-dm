//! Durable DM-to-Ting handoff processing, independent of connected DM clients.

use std::{future::pending, sync::Arc, time::Duration};

use sqlx::{Postgres, Transaction, postgres::PgListener};
use time::OffsetDateTime;
use tokio::time::{Instant, MissedTickBehavior, interval, interval_at, timeout_at};
use tokio_util::sync::CancellationToken;

use crate::{
    AppError, AppResult,
    config::WorkerSettings,
    infrastructure::{
        postgres::{PostgresStore, TingDeliveryContext},
        ting::{SharedTingPublisher, TingFailure},
    },
    testing::TestingRegistry,
};

const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(60);

/// Persists handoff acceptance only after Ting confirms durable storage.
/// PostgreSQL notifications reduce latency; durable polling remains authoritative.
pub struct TingDeliveryWorker {
    store: PostgresStore,
    publisher: SharedTingPublisher,
    context: TingDeliveryContext,
    owner: Arc<str>,
    settings: WorkerSettings,
    testing: Option<Arc<TestingRegistry>>,
}

impl TingDeliveryWorker {
    /// Constructs a worker with explicit publisher authority and data context.
    #[must_use]
    pub fn new(
        store: PostgresStore,
        publisher: SharedTingPublisher,
        context: TingDeliveryContext,
        owner: Arc<str>,
        settings: WorkerSettings,
    ) -> Self {
        Self {
            store,
            publisher,
            context,
            owner,
            settings,
            testing: None,
        }
    }

    /// Binds sandbox attempts to the live generation/lifecycle lock. A sandbox
    /// worker without this registry cannot claim or send any handoff.
    #[must_use]
    pub fn with_testing_registry(mut self, registry: Arc<TestingRegistry>) -> Self {
        self.testing = Some(registry);
        self
    }

    /// Claims disconnected and connected recipients alike until shutdown.
    ///
    /// # Errors
    /// Returns durable-storage/configuration failures. Publisher failures leave
    /// handoffs retryable and never fail a DM message or fabricate a receipt.
    pub async fn run(&self, cancellation: CancellationToken) -> AppResult<()> {
        let Some(mut listener) = cancellation
            .run_until_cancelled(self.open_delivery_listener())
            .await
        else {
            return Ok(());
        };
        let mut poll = interval(self.settings.poll_interval.max(Duration::from_millis(10)));
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
            if maintain {
                let Some(result) = cancellation.run_until_cancelled(self.maintain_once()).await
                else {
                    return Ok(());
                };
                result?;
                if listener.is_none() {
                    let Some(reconnected) = cancellation
                        .run_until_cancelled(self.open_delivery_listener())
                        .await
                    else {
                        return Ok(());
                    };
                    listener = reconnected;
                }
            } else {
                let Some(result) = cancellation.run_until_cancelled(self.process_once()).await
                else {
                    return Ok(());
                };
                result?;
            }
        }
    }

    async fn open_delivery_listener(&self) -> Option<PgListener> {
        match self.store.subscribe_delivery_wakeups().await {
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

    /// Expires presence and retained legacy delivery/idempotency state while
    /// retaining pending Ting handoffs and all explicit DM receipt state.
    ///
    /// # Errors
    /// Reports storage failures or a changed sandbox lifecycle/generation.
    pub async fn maintain_once(&self) -> AppResult<()> {
        let _fence = self.lifecycle_fence().await?;
        let schema: String = sqlx::query_scalar("SELECT current_schema()")
            .fetch_one(self.store.pool())
            .await?;
        let expected = self
            .context
            .testing_environment_id
            .map_or_else(|| "dm".to_owned(), |id| format!("dm_test_{}", id.simple()));
        if schema != expected {
            return Err(AppError::conflict(
                "Ting maintenance context does not match the DM data plane",
            ));
        }
        sqlx::query("UPDATE contract_versions SET status='sunset',sunset_at=clock_timestamp() WHERE status='deprecated' AND GREATEST(COALESCE(last_request_at,introduced_at),deprecated_at)<=clock_timestamp()-INTERVAL '7 days'")
            .execute(self.store.pool()).await?;
        let expired_sessions = self.store.expire_realtime_sessions().await?;
        let expired_http_leases = self.store.expire_http_presence_leases().await?;
        let compacted = self.store.compact_delivery_state().await?;
        if expired_sessions > 0 || expired_http_leases > 0 || compacted > 0 {
            tracing::info!(
                expired_sessions,
                expired_http_leases,
                compacted_rows = compacted,
                "Ting worker maintenance completed"
            );
        }
        Ok(())
    }

    async fn lifecycle_fence(&self) -> AppResult<Option<Transaction<'static, Postgres>>> {
        match (
            self.context.testing_environment_id,
            self.context.testing_generation,
        ) {
            (None, None) => Ok(None),
            (Some(id), Some(generation)) => {
                let registry = self.testing.as_ref().ok_or_else(|| {
                    AppError::validation(
                        "sandbox Ting publisher requires a live testing lifecycle registry",
                    )
                })?;
                Ok(Some(registry.request_fence(id, generation).await?))
            }
            _ => Err(AppError::validation(
                "Ting environment and generation must be paired",
            )),
        }
    }

    /// Runs one bounded batch. Only one row is leased at a time, so slow sends
    /// cannot consume the leases of other rows waiting in an in-memory batch.
    ///
    /// # Errors
    /// Returns durable-storage/configuration failures; lost leases are harmless
    /// and are recovered by the next owner with the unchanged event key/body.
    pub async fn process_once(&self) -> AppResult<usize> {
        let mut processed = 0;
        let reserve = Duration::from_millis(500);
        let available = self
            .settings
            .lease_duration
            .checked_sub(reserve)
            .filter(|duration| !duration.is_zero())
            .ok_or_else(|| AppError::validation("Ting lease must exceed 500 milliseconds"))?;
        for _ in 0..self.settings.batch_size.get().min(100) {
            let _fence = self.lifecycle_fence().await?;
            let deadline = Instant::now() + available;
            let Some(claim) = self
                .store
                .claim_ting_deliveries(&self.owner, 1, self.settings.lease_duration, &self.context)
                .await?
                .pop()
            else {
                break;
            };
            let authorized = self.store.ting_delivery_authorized(&claim).await?;
            let outcome = if Instant::now() >= deadline {
                Err(TingFailure::Timeout)
            } else if authorized {
                timeout_at(deadline, self.publisher.publish(&claim))
                    .await
                    .unwrap_or(Err(TingFailure::Timeout))
            } else {
                Err(TingFailure::Rejected(
                    "ting_recipient_authority_changed".into(),
                ))
            };
            let stored = match outcome {
                Ok(acceptance) => {
                    self.store
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
                    let delay = retry_delay(claim.attempt_count, self.settings.max_retry_delay);
                    let next = OffsetDateTime::now_utc()
                        + time::Duration::try_from(delay).map_err(AppError::internal)?;
                    tracing::warn!(delivery_id = %claim.delivery_id, code = error.code(),
                        "Ting handoff remains pending");
                    self.store
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
