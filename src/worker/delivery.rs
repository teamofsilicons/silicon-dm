//! Durable actor-delivery processing.

use std::{future::pending, sync::Arc, time::Duration};

use sqlx::postgres::PgListener;
use time::OffsetDateTime;
use tokio::time::{Instant, MissedTickBehavior, interval, interval_at};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    AppResult,
    application::commands::ActorDelivery,
    config::WorkerSettings,
    infrastructure::postgres::{DeliveryClaim, PostgresStore},
    realtime::{RealtimeHub, RealtimeTarget, ServerFrame},
};

const MINIMUM_RETRY_DELAY: Duration = Duration::from_secs(1);
const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(60);
const TARGET_MISMATCH_ERROR: &str = "delivery_target_mismatch";

/// Claims delivery rows only for locally connected targets and wakes their
/// bounded realtime queues.
pub struct DeliveryWorker {
    store: PostgresStore,
    realtime: RealtimeHub,
    owner: Arc<str>,
    settings: WorkerSettings,
    next_target_offset: usize,
}

impl DeliveryWorker {
    /// Creates one process-scoped delivery worker.
    #[must_use]
    pub fn new(
        store: PostgresStore,
        realtime: RealtimeHub,
        owner: Arc<str>,
        settings: WorkerSettings,
    ) -> Self {
        Self {
            store,
            realtime,
            owner,
            settings,
            next_target_offset: 0,
        }
    }

    /// Polls until cooperative cancellation or an unrecoverable storage error.
    ///
    /// Database operations are raced against cancellation so shutdown does not
    /// wait for another polling interval. An in-flight lease abandoned by
    /// cancellation becomes eligible again after its database expiry.
    ///
    /// # Errors
    ///
    /// Returns a redacted storage error when claiming, releasing, dead-lettering,
    /// presence expiry, or retention compaction fails.
    pub async fn run(mut self, cancellation: CancellationToken) -> AppResult<()> {
        let mut listener = self.open_delivery_listener().await;
        let mut poll = interval(self.settings.poll_interval);
        poll.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let maintenance_start = Instant::now() + MAINTENANCE_INTERVAL;
        let mut maintenance = interval_at(maintenance_start, MAINTENANCE_INTERVAL);
        maintenance.set_missed_tick_behavior(MissedTickBehavior::Skip);

        loop {
            let work = tokio::select! {
                biased;
                () = cancellation.cancelled() => return Ok(()),
                _ = poll.tick() => Work::Deliver,
                _ = maintenance.tick() => Work::Maintain,
                notification = receive_delivery_wakeup(&mut listener) => {
                    if notification.is_err() {
                        tracing::warn!(
                            failure = "delivery_listener",
                            "PostgreSQL delivery wakeup listener disconnected; polling remains active"
                        );
                        listener = None;
                    }
                    Work::Deliver
                },
            };
            let outcome = match work {
                Work::Deliver => self.deliver_connected(&cancellation).await?,
                Work::Maintain => {
                    let outcome = self.maintain(&cancellation).await?;
                    if listener.is_none() {
                        listener = self.open_delivery_listener().await;
                    }
                    outcome
                }
            };
            if outcome == CycleOutcome::Cancelled {
                return Ok(());
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
                    "PostgreSQL delivery wakeup listener is unavailable; polling remains active"
                );
                None
            }
        }
    }

    async fn deliver_connected(
        &mut self,
        cancellation: &CancellationToken,
    ) -> AppResult<CycleOutcome> {
        let mut targets = self.realtime.connected_targets();
        if targets.is_empty() {
            return Ok(CycleOutcome::Complete);
        }
        sort_targets(&mut targets);

        let target_count = targets.len();
        let start = self.next_target_offset % target_count;
        self.next_target_offset = (start + 1) % target_count;
        let mut remaining = self.settings.batch_size.get();

        for offset in 0..target_count {
            if remaining == 0 {
                break;
            }
            if cancellation.is_cancelled() {
                return Ok(CycleOutcome::Cancelled);
            }

            let target = &targets[(start + offset) % target_count];
            if self.realtime.connection_count(target) == 0 {
                continue;
            }
            let Some(result) = cancellation
                .run_until_cancelled(self.store.claim_deliveries_for_target(
                    target,
                    &self.owner,
                    remaining,
                    self.settings.lease_duration,
                ))
                .await
            else {
                return Ok(CycleOutcome::Cancelled);
            };
            let claims = result?;
            remaining = remaining.saturating_sub(claims.len());

            for claim in claims {
                if self.deliver_claim(target, claim, cancellation).await? == CycleOutcome::Cancelled
                {
                    return Ok(CycleOutcome::Cancelled);
                }
            }
        }

        Ok(CycleOutcome::Complete)
    }

    async fn deliver_claim(
        &self,
        target: &RealtimeTarget,
        claim: DeliveryClaim,
        cancellation: &CancellationToken,
    ) -> AppResult<CycleOutcome> {
        let DeliveryClaim {
            delivery,
            attempt_count,
        } = claim;
        if !delivery_matches_target(&delivery, target) {
            tracing::warn!(
                delivery_id = %delivery.id,
                failure = TARGET_MISMATCH_ERROR,
                "claimed delivery failed a storage invariant"
            );
            return self
                .record_processing_failure(
                    delivery.id,
                    attempt_count,
                    TARGET_MISMATCH_ERROR,
                    cancellation,
                )
                .await;
        }

        let delivery_id = delivery.id;
        let frame = ServerFrame::delivery(
            delivery.id,
            delivery.target.id,
            delivery.sequence,
            delivery.payload,
        );
        let report = self.realtime.publish(target, &frame);
        tracing::debug!(
            %delivery_id,
            attempted = report.attempted,
            enqueued = report.enqueued,
            saturated = report.saturated,
            closed = report.closed,
            "durable delivery fan-out attempted"
        );

        // Enqueue, saturation, and a disconnect race are all normal delivery
        // states. The row remains replayable and is retried without consuming
        // an attempt until the client advances its durable ACK cursor.
        let next_attempt_at = deadline_after(short_retry_delay(&self.settings));
        let Some(result) = cancellation
            .run_until_cancelled(self.store.release_delivery_claim(
                delivery_id,
                &self.owner,
                next_attempt_at,
                None,
                false,
            ))
            .await
        else {
            return Ok(CycleOutcome::Cancelled);
        };
        result?;
        Ok(CycleOutcome::Complete)
    }

    async fn record_processing_failure(
        &self,
        delivery_id: Uuid,
        attempt_count: u16,
        error_code: &'static str,
        cancellation: &CancellationToken,
    ) -> AppResult<CycleOutcome> {
        let terminal = attempt_count.saturating_add(1) >= self.settings.max_attempts;
        let result = if terminal {
            cancellation
                .run_until_cancelled(self.store.dead_letter_delivery(
                    delivery_id,
                    &self.owner,
                    error_code,
                ))
                .await
        } else {
            let next_attempt_at =
                deadline_after(failure_retry_delay(&self.settings, attempt_count));
            cancellation
                .run_until_cancelled(self.store.release_delivery_claim(
                    delivery_id,
                    &self.owner,
                    next_attempt_at,
                    Some(error_code),
                    true,
                ))
                .await
        };
        let Some(result) = result else {
            return Ok(CycleOutcome::Cancelled);
        };
        result?;
        Ok(CycleOutcome::Complete)
    }

    async fn maintain(&self, cancellation: &CancellationToken) -> AppResult<CycleOutcome> {
        let Some(expired) = cancellation
            .run_until_cancelled(self.store.expire_realtime_sessions())
            .await
        else {
            return Ok(CycleOutcome::Cancelled);
        };
        let expired = expired?;
        let Some(compacted) = cancellation
            .run_until_cancelled(self.store.compact_delivery_state())
            .await
        else {
            return Ok(CycleOutcome::Cancelled);
        };
        let compacted = compacted?;
        if expired > 0 || compacted > 0 {
            tracing::info!(
                expired_sessions = expired,
                compacted_rows = compacted,
                "delivery maintenance completed"
            );
        }
        Ok(CycleOutcome::Complete)
    }
}

async fn receive_delivery_wakeup(listener: &mut Option<PgListener>) -> Result<(), sqlx::Error> {
    match listener {
        Some(listener) => listener.recv().await.map(|_| ()),
        None => pending().await,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CycleOutcome {
    Complete,
    Cancelled,
}

#[derive(Clone, Copy, Debug)]
enum Work {
    Deliver,
    Maintain,
}

fn delivery_matches_target(delivery: &ActorDelivery, target: &RealtimeTarget) -> bool {
    delivery.organization_id == target.organization_id && delivery.target == target.actor
}

fn sort_targets(targets: &mut [RealtimeTarget]) {
    targets.sort_by(|left, right| {
        left.organization_id
            .cmp(&right.organization_id)
            .then_with(|| {
                left.actor
                    .actor_type
                    .as_str()
                    .cmp(right.actor.actor_type.as_str())
            })
            .then_with(|| left.actor.id.cmp(&right.actor.id))
    });
}

fn short_retry_delay(settings: &WorkerSettings) -> Duration {
    settings
        .poll_interval
        .max(MINIMUM_RETRY_DELAY)
        .min(settings.max_retry_delay)
}

fn failure_retry_delay(settings: &WorkerSettings, attempt_count: u16) -> Duration {
    let base = short_retry_delay(settings);
    let shift = u32::from(attempt_count.min(31));
    let multiplier = 1_u32.checked_shl(shift).unwrap_or(u32::MAX);
    base.saturating_mul(multiplier)
        .min(settings.max_retry_delay)
}

fn deadline_after(delay: Duration) -> OffsetDateTime {
    let now = OffsetDateTime::now_utc();
    let delay = match time::Duration::try_from(delay) {
        Ok(delay) => delay,
        Err(_) => time::Duration::days(1),
    };
    match now.checked_add(delay) {
        Some(deadline) => deadline,
        None => now + time::Duration::days(1),
    }
}

#[cfg(test)]
mod tests {
    use std::{num::NonZeroUsize, str::FromStr as _, time::Duration};

    use uuid::Uuid;

    use super::{delivery_matches_target, failure_retry_delay, short_retry_delay, sort_targets};
    use crate::{
        application::commands::ActorDelivery,
        config::WorkerSettings,
        domain::{ActorId, ActorRef, ActorType, MessageStatus, OrganizationId},
        realtime::{DeliveryPayload, RealtimeTarget},
    };

    fn settings() -> Result<WorkerSettings, &'static str> {
        Ok(WorkerSettings {
            batch_size: NonZeroUsize::new(100).ok_or("batch size must be non-zero")?,
            poll_interval: Duration::from_millis(250),
            lease_duration: Duration::from_secs(30),
            max_attempts: 20,
            max_retry_delay: Duration::from_secs(5),
        })
    }

    #[test]
    fn successful_delivery_retry_is_short_but_not_a_busy_loop() -> Result<(), &'static str> {
        assert_eq!(short_retry_delay(&settings()?), Duration::from_secs(1));
        Ok(())
    }

    #[test]
    fn processing_failure_backoff_is_exponential_and_capped() -> Result<(), &'static str> {
        let settings = settings()?;
        assert_eq!(failure_retry_delay(&settings, 0), Duration::from_secs(1));
        assert_eq!(failure_retry_delay(&settings, 2), Duration::from_secs(4));
        assert_eq!(failure_retry_delay(&settings, 10), Duration::from_secs(5));
        Ok(())
    }

    #[test]
    fn delivery_target_match_includes_organization_scope() -> Result<(), Box<dyn std::error::Error>>
    {
        let target = target("org-a", ActorType::Carbon, "shared-actor")?;
        let delivery = ActorDelivery {
            id: Uuid::nil(),
            organization_id: OrganizationId::from_str("org-b")?,
            target: target.actor.clone(),
            sequence: 1,
            payload: DeliveryPayload::Receipt {
                message_id: Uuid::nil(),
                status: MessageStatus::Sent,
            },
        };

        assert!(!delivery_matches_target(&delivery, &target));
        Ok(())
    }

    #[test]
    fn target_sorting_is_stable_across_organizations_and_actor_types()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut targets = vec![
            target("org-b", ActorType::Carbon, "actor-a")?,
            target("org-a", ActorType::Silicon, "actor-a")?,
            target("org-a", ActorType::Carbon, "actor-b")?,
            target("org-a", ActorType::Carbon, "actor-a")?,
        ];

        sort_targets(&mut targets);

        let ordered = targets
            .iter()
            .map(|target| {
                (
                    target.organization_id.as_str(),
                    target.actor.actor_type.as_str(),
                    target.actor.id.as_str(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            ordered,
            vec![
                ("org-a", "carbon", "actor-a"),
                ("org-a", "carbon", "actor-b"),
                ("org-a", "silicon", "actor-a"),
                ("org-b", "carbon", "actor-a"),
            ]
        );
        Ok(())
    }

    fn target(
        organization_id: &str,
        actor_type: ActorType,
        actor_id: &str,
    ) -> Result<RealtimeTarget, Box<dyn std::error::Error>> {
        Ok(RealtimeTarget {
            organization_id: OrganizationId::from_str(organization_id)?,
            actor: ActorRef {
                actor_type,
                id: ActorId::from_str(actor_id)?,
            },
        })
    }
}
