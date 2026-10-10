//! The per-account delivery outbox that feeds `/api/v1/sync` and Ting: wakeups and
//! retention. (Socket replay, ACKs and claims retired with DM's WebSocket.)

use sqlx::postgres::PgListener;

use crate::AppResult;

use super::{PostgresStore, map_constraint_error};

/// Postgres channel notified after a delivery or Ting handoff commits.
pub(crate) const DELIVERY_NOTIFICATION_CHANNEL: &str = "dm_delivery";

impl PostgresStore {
    /// Subscribes to best-effort delivery wakeups when the pool has capacity
    /// for a dedicated listener connection.
    ///
    /// Durable polling remains authoritative, so a pool intentionally limited
    /// to one connection returns `None` rather than starving normal queries.
    ///
    /// # Errors
    ///
    /// Propagates listener acquisition or PostgreSQL `LISTEN` failures.
    pub async fn subscribe_delivery_wakeups(&self) -> AppResult<Option<PgListener>> {
        if self.pool().options().get_max_connections() < 2 {
            return Ok(None);
        }
        let mut listener = PgListener::connect_with(self.pool()).await?;
        listener.listen(DELIVERY_NOTIFICATION_CHANNEL).await?;
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
}
