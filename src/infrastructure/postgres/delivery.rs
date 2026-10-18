//! The per-account delivery outbox that feeds `/api/v1/sync` and Ting: wakeups and
//! retention. (Socket replay, ACKs and claims retired with DM's WebSocket.)

use std::time::Duration;

use sqlx::postgres::{PgListener, PgPoolOptions};
use time::OffsetDateTime;

use crate::{AppError, AppResult};

use super::{PostgresStore, map_constraint_error};

/// Postgres channel notified after a delivery or Ting handoff commits.
pub(crate) const DELIVERY_NOTIFICATION_CHANNEL: &str = "dm_delivery";

/// How long the sync feed keeps an account's events, and Ting its finished handoffs. A sync
/// cursor lasts a day, so every cursor a client can still hold lies inside the window; a
/// scan without one (`dm inbox --from beginning`) reads the last 30 days.
pub const SYNC_RETENTION: Duration = Duration::from_hours(30 * 24);
/// Accounts whose oldest events are trimmed in one maintenance pass.
const PRUNE_ACCOUNTS_PER_PASS: i64 = 200;
/// Stream positions one account gives up in one transaction (it holds that account's
/// stream for the moment it takes).
const PRUNE_POSITIONS_PER_ACCOUNT: i64 = 2_000;
/// Finished Ting handoffs deleted in one pass.
const PRUNE_HANDOFFS_PER_PASS: i64 = 5_000;

/// What one retention pass removed.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Pruned {
    /// Delivery rows.
    pub deliveries: u64,
    /// Conversation event rows.
    pub conversation_events: u64,
    /// Ting handoffs.
    pub ting_handoffs: u64,
}

impl Pruned {
    /// Rows removed in all.
    #[must_use]
    pub fn total(self) -> u64 {
        self.deliveries
            .saturating_add(self.conversation_events)
            .saturating_add(self.ting_handoffs)
    }
}

impl PostgresStore {
    /// Subscribes to best-effort delivery wakeups.
    ///
    /// # Errors
    ///
    /// Propagates listener connection or PostgreSQL `LISTEN` failures.
    pub async fn subscribe_delivery_wakeups(&self) -> AppResult<Option<PgListener>> {
        self.dedicated_listener(DELIVERY_NOTIFICATION_CHANNEL).await
    }

    /// A `LISTEN` on its own connection, outside the query pool: a listener holds its
    /// connection for as long as it lives, and three of them (the sync hub, the Ting worker,
    /// the speech worker) would otherwise keep three of the pool's connections for good
    /// (production runs four). Durable polling stays authoritative, so a deployment that
    /// limits DM to one connection gets no listener at all (`None`).
    pub(crate) async fn dedicated_listener(&self, channel: &str) -> AppResult<Option<PgListener>> {
        if self.pool().options().get_max_connections() < 2 {
            return Ok(None);
        }
        let options = (*self.pool().connect_options()).clone();
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .min_connections(0)
            .acquire_timeout(self.pool().options().get_acquire_timeout())
            .max_lifetime(None)
            .idle_timeout(None)
            .connect_with(options)
            .await
            .map_err(AppError::database)?;
        let mut listener = PgListener::connect_with(&pool).await?;
        listener.listen(channel).await?;
        Ok(Some(listener))
    }

    /// Deletes expired acknowledged outbox rows and completed/abandoned idempotency rows.
    ///
    /// # Errors
    ///
    /// Propagates database failures.
    pub async fn compact_delivery_state(&self) -> AppResult<u64> {
        let deliveries = sqlx::query(
            r#"
            DELETE FROM actor_deliveries
            WHERE acked_at IS NOT NULL
              AND retain_until <= transaction_timestamp()
            "#,
        )
        .execute(self.pool())
        .await
        .map_err(map_constraint_error)?
        .rows_affected();
        let idempotency = sqlx::query(
            r#"
            DELETE FROM idempotency_records
            WHERE expires_at <= transaction_timestamp()
              AND (
                  status = 'completed'
                  OR lease_expires_at <= transaction_timestamp()
              )
            "#,
        )
        .execute(self.pool())
        .await
        .map_err(map_constraint_error)?
        .rows_affected();
        Ok(deliveries.saturating_add(idempotency))
    }

    /// One bounded retention pass: for accounts whose oldest kept event is older than
    /// `before`, raises the sync floor past their events from before then (a cursor below the
    /// floor is told to reset, as for any expired cursor) and deletes those events and the
    /// conversation events at the same positions; then deletes Ting handoffs accepted or
    /// discarded before then. A handoff still waiting to be sent is never touched.
    ///
    /// # Errors
    /// Database failures.
    pub async fn prune_sync_history(&self, before: OffsetDateTime) -> AppResult<Pruned> {
        let mut pruned = Pruned::default();
        let accounts: Vec<(String, String)> = sqlx::query_as(
            r#"
            SELECT stream.actor_kind::text, stream.actor_id
            FROM actor_delivery_streams stream
            WHERE stream.organization_id = '@accounts'
              AND stream.sync_floor + 1 < stream.next_sequence
              AND (EXISTS (SELECT 1 FROM actor_deliveries d
                           WHERE d.target_kind = stream.actor_kind AND d.target_id = stream.actor_id
                             AND d.sequence = stream.sync_floor + 1 AND d.created_at < $1)
                   OR EXISTS (SELECT 1 FROM conversation_events e
                              WHERE e.target_kind = stream.actor_kind AND e.target_id = stream.actor_id
                                AND e.sequence = stream.sync_floor + 1 AND e.created_at < $1))
            LIMIT $2
            "#,
        )
        .bind(before)
        .bind(PRUNE_ACCOUNTS_PER_PASS)
        .fetch_all(self.pool())
        .await?;
        for (kind, account) in accounts {
            let (deliveries, events) = self.prune_account(&kind, &account, before).await?;
            pruned.deliveries = pruned.deliveries.saturating_add(deliveries);
            pruned.conversation_events = pruned.conversation_events.saturating_add(events);
        }
        pruned.ting_handoffs = sqlx::query(
            r#"
            DELETE FROM ting_handoffs
            WHERE delivery_id IN (
                SELECT delivery_id FROM ting_handoffs
                WHERE (accepted_at IS NOT NULL OR discarded_at IS NOT NULL)
                  AND created_at < $1
                ORDER BY created_at, delivery_id
                LIMIT $2
                FOR UPDATE SKIP LOCKED)
            "#,
        )
        .bind(before)
        .bind(PRUNE_HANDOFFS_PER_PASS)
        .execute(self.pool())
        .await
        .map_err(map_constraint_error)?
        .rows_affected();
        Ok(pruned)
    }

    /// Trims one account's stream: the floor moves to the newest of its next positions whose
    /// event is older than `before`, and the events at or below it go, in one transaction.
    async fn prune_account(
        &self,
        kind: &str,
        account: &str,
        before: OffsetDateTime,
    ) -> AppResult<(u64, u64)> {
        let mut tx = self.pool().begin().await?;
        // The stream row is what every delivery to the account locks; skip an account that
        // is busy now (the next pass gets it).
        let Some(floor): Option<i64> = sqlx::query_scalar(
            "SELECT sync_floor FROM actor_delivery_streams
             WHERE organization_id = '@accounts' AND actor_kind = $1::text::actor_kind AND actor_id = $2
             FOR UPDATE SKIP LOCKED",
        )
        .bind(kind)
        .bind(account)
        .fetch_optional(&mut *tx)
        .await?
        else {
            return Ok((0, 0));
        };
        let new_floor: Option<i64> = sqlx::query_scalar(
            r#"
            SELECT max(sequence) FROM (
                SELECT sequence, created_at FROM actor_deliveries
                WHERE target_kind = $1::text::actor_kind AND target_id = $2
                  AND sequence > $3 AND sequence <= $3 + $4
                UNION ALL
                SELECT sequence, created_at FROM conversation_events
                WHERE target_kind = $1::text::actor_kind AND target_id = $2
                  AND sequence > $3 AND sequence <= $3 + $4
            ) kept
            WHERE created_at < $5
            "#,
        )
        .bind(kind)
        .bind(account)
        .bind(floor)
        .bind(PRUNE_POSITIONS_PER_ACCOUNT)
        .bind(before)
        .fetch_one(&mut *tx)
        .await?;
        let Some(new_floor) = new_floor.filter(|new_floor| *new_floor > floor) else {
            return Ok((0, 0));
        };
        sqlx::query(
            "UPDATE actor_delivery_streams SET sync_floor = $3
             WHERE organization_id = '@accounts' AND actor_kind = $1::text::actor_kind AND actor_id = $2
               AND sync_floor < $3 AND $3 < next_sequence",
        )
        .bind(kind)
        .bind(account)
        .bind(new_floor)
        .execute(&mut *tx)
        .await
        .map_err(map_constraint_error)?;
        let deliveries = sqlx::query(
            "DELETE FROM actor_deliveries
             WHERE target_kind = $1::text::actor_kind AND target_id = $2
               AND sequence > $3 AND sequence <= $4",
        )
        .bind(kind)
        .bind(account)
        .bind(floor)
        .bind(new_floor)
        .execute(&mut *tx)
        .await
        .map_err(map_constraint_error)?
        .rows_affected();
        let events = sqlx::query(
            "DELETE FROM conversation_events
             WHERE target_kind = $1::text::actor_kind AND target_id = $2
               AND sequence > $3 AND sequence <= $4",
        )
        .bind(kind)
        .bind(account)
        .bind(floor)
        .bind(new_floor)
        .execute(&mut *tx)
        .await
        .map_err(map_constraint_error)?
        .rows_affected();
        tx.commit().await.map_err(map_constraint_error)?;
        Ok((deliveries, events))
    }
}
