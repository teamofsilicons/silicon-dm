//! Durable actor delivery streams, ACKs, replay, claims, and retention.

use std::{collections::BTreeMap, time::Duration};

use serde_json::Value;
use sqlx::{FromRow, Postgres, Transaction, postgres::PgListener};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{
    AppError, AppResult,
    application::commands::ActorDelivery,
    domain::{ActorRef, OrganizationId, SystemEvent},
    realtime::{DeliveryPayload, RealtimeTarget},
};

use super::{
    PostgresStore, map_constraint_error,
    messages::notify_actor_in,
    rows::{parse_actor_id, parse_actor_type, parse_message_status},
};

const ACK_RETENTION_DAYS: i64 = 30;
const MAX_REPLAY_BATCH: usize = 500;
const DELIVERY_NOTIFICATION_CHANNEL: &str = "dm_delivery";

/// One worker-owned delivery claim with its pre-attempt counter.
#[derive(Clone, Debug)]
pub struct DeliveryClaim {
    /// Durable delivery and fully hydrated immutable payload.
    pub delivery: ActorDelivery,
    /// Number of prior processing failures.
    pub attempt_count: u16,
}

#[derive(Clone, Debug, FromRow)]
struct DeliveryRecord {
    id: Uuid,
    organization_id: String,
    target_kind: String,
    target_id: String,
    sequence: i64,
    delivery_kind: String,
    message_id: Option<Uuid>,
    aggregate_status: Option<String>,
    system_event_id: Option<Uuid>,
    attempt_count: i32,
}

#[derive(Debug, FromRow)]
struct SystemEventRecord {
    event_id: Uuid,
    organization_id: String,
    target_silicon_id: String,
    event_type: String,
    trace_id: Option<String>,
    payload: Value,
}

#[derive(Debug, FromRow)]
struct FailedMessageSource {
    organization_id: String,
    conversation_id: Uuid,
    message_id: Uuid,
    sender_kind: String,
    sender_id: String,
}

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

    /// Returns the per-consumer cumulative ACK cursor for one actor stream.
    ///
    /// # Errors
    ///
    /// Rejects an invalid consumer ID and propagates database errors.
    pub async fn acknowledged_through(
        &self,
        organization_id: &OrganizationId,
        actor: &ActorRef,
        consumer_id: &str,
    ) -> AppResult<i64> {
        validate_consumer_id(consumer_id)?;
        let cursor = sqlx::query_scalar::<_, i64>(
            r#"
            SELECT last_acked_sequence
            FROM dm.actor_delivery_ack_cursors
            WHERE organization_id = $1
              AND actor_kind = $2::text::dm.actor_kind
              AND actor_id = $3
              AND consumer_id = $4
            "#,
        )
        .bind(organization_id.as_str())
        .bind(actor.actor_type.as_str())
        .bind(actor.id.as_str())
        .bind(consumer_id)
        .fetch_optional(self.pool())
        .await?;
        Ok(cursor.unwrap_or(0))
    }

    /// Returns ACK cursors keyed by public actor ID for a ready frame.
    ///
    /// # Errors
    ///
    /// Rejects an invalid consumer ID or ambiguous duplicate actor IDs.
    pub async fn acknowledged_cursors(
        &self,
        organization_id: &OrganizationId,
        actors: &[ActorRef],
        consumer_id: &str,
    ) -> AppResult<BTreeMap<String, i64>> {
        validate_consumer_id(consumer_id)?;
        let mut cursors = BTreeMap::new();
        for actor in actors {
            let previous = cursors.insert(
                actor.id.to_string(),
                self.acknowledged_through(organization_id, actor, consumer_id)
                    .await?,
            );
            if previous.is_some() {
                return Err(AppError::validation("represented actor IDs must be unique"));
            }
        }
        Ok(cursors)
    }

    /// Returns the highest sequence allocated for one actor stream.
    ///
    /// # Errors
    ///
    /// Propagates database failures.
    pub async fn delivery_high_watermark(
        &self,
        organization_id: &OrganizationId,
        actor: &ActorRef,
    ) -> AppResult<i64> {
        let next = sqlx::query_scalar::<_, i64>(
            r#"
            SELECT next_sequence
            FROM dm.actor_delivery_streams
            WHERE organization_id = $1
              AND actor_kind = $2::text::dm.actor_kind
              AND actor_id = $3
            "#,
        )
        .bind(organization_id.as_str())
        .bind(actor.actor_type.as_str())
        .bind(actor.id.as_str())
        .fetch_optional(self.pool())
        .await?;
        Ok(next.map_or(0, |value| value.saturating_sub(1)))
    }

    /// Cumulatively ACKs one actor stream for one durable consumer.
    ///
    /// Acknowledged envelopes remain replayable through the retention window.
    ///
    /// # Errors
    ///
    /// Rejects a negative or unallocated sequence and propagates database
    /// invariant failures.
    pub async fn acknowledge_deliveries(
        &self,
        organization_id: &OrganizationId,
        actor: &ActorRef,
        consumer_id: &str,
        through_sequence: i64,
    ) -> AppResult<()> {
        validate_consumer_id(consumer_id)?;
        if through_sequence < 0 {
            return Err(AppError::validation(
                "delivery ACK sequence must be non-negative",
            ));
        }
        let mut transaction = self.pool().begin().await?;
        sqlx::query(
            r#"
            INSERT INTO dm.actor_delivery_streams (
                organization_id,
                actor_kind,
                actor_id
            )
            VALUES ($1, $2::text::dm.actor_kind, $3)
            ON CONFLICT DO NOTHING
            "#,
        )
        .bind(organization_id.as_str())
        .bind(actor.actor_type.as_str())
        .bind(actor.id.as_str())
        .execute(&mut *transaction)
        .await
        .map_err(map_constraint_error)?;
        sqlx::query(
            r#"
            INSERT INTO dm.actor_delivery_ack_cursors (
                organization_id,
                actor_kind,
                actor_id,
                consumer_id,
                last_acked_sequence
            )
            VALUES ($1, $2::text::dm.actor_kind, $3, $4, $5)
            ON CONFLICT (organization_id, actor_kind, actor_id, consumer_id)
            DO UPDATE SET last_acked_sequence = GREATEST(
                dm.actor_delivery_ack_cursors.last_acked_sequence,
                EXCLUDED.last_acked_sequence
            )
            "#,
        )
        .bind(organization_id.as_str())
        .bind(actor.actor_type.as_str())
        .bind(actor.id.as_str())
        .bind(consumer_id)
        .bind(through_sequence)
        .execute(&mut *transaction)
        .await
        .map_err(map_constraint_error)?;
        sqlx::query(
            r#"
            UPDATE dm.actor_delivery_streams
            SET last_acked_sequence = GREATEST(last_acked_sequence, $4)
            WHERE organization_id = $1
              AND actor_kind = $2::text::dm.actor_kind
              AND actor_id = $3
            "#,
        )
        .bind(organization_id.as_str())
        .bind(actor.actor_type.as_str())
        .bind(actor.id.as_str())
        .bind(through_sequence)
        .execute(&mut *transaction)
        .await
        .map_err(map_constraint_error)?;
        sqlx::query(
            r#"
            UPDATE dm.actor_deliveries
            SET acked_at = COALESCE(acked_at, transaction_timestamp()),
                retain_until = COALESCE(
                    retain_until,
                    transaction_timestamp() + ($5::bigint * interval '1 day')
                ),
                lease_owner = NULL,
                lease_started_at = NULL,
                lease_expires_at = NULL
            WHERE organization_id = $1
              AND target_kind = $2::text::dm.actor_kind
              AND target_id = $3
              AND sequence <= $4
              AND dead_lettered_at IS NULL
            "#,
        )
        .bind(organization_id.as_str())
        .bind(actor.actor_type.as_str())
        .bind(actor.id.as_str())
        .bind(through_sequence)
        .bind(ACK_RETENTION_DAYS)
        .execute(&mut *transaction)
        .await
        .map_err(map_constraint_error)?;
        transaction.commit().await.map_err(map_constraint_error)
    }

    /// Replays durable envelopes after a caller-owned stream cursor.
    ///
    /// # Errors
    ///
    /// Rejects invalid bounds and reports corrupt source payloads as internal
    /// errors.
    pub async fn replay_deliveries(
        &self,
        organization_id: &OrganizationId,
        actor: &ActorRef,
        after_sequence: i64,
        limit: usize,
    ) -> AppResult<Vec<ActorDelivery>> {
        if after_sequence < 0 {
            return Err(AppError::validation(
                "delivery replay sequence must be non-negative",
            ));
        }
        let limit = validate_delivery_limit(limit, MAX_REPLAY_BATCH)?;
        let records = sqlx::query_as::<_, DeliveryRecord>(
            r#"
            SELECT
                id,
                organization_id,
                target_kind::text AS target_kind,
                target_id,
                sequence,
                delivery_kind::text AS delivery_kind,
                message_id,
                aggregate_status::text AS aggregate_status,
                system_event_id,
                attempt_count
            FROM dm.actor_deliveries
            WHERE organization_id = $1
              AND target_kind = $2::text::dm.actor_kind
              AND target_id = $3
              AND sequence > $4
              AND dead_lettered_at IS NULL
            ORDER BY sequence
            LIMIT $5
            "#,
        )
        .bind(organization_id.as_str())
        .bind(actor.actor_type.as_str())
        .bind(actor.id.as_str())
        .bind(after_sequence)
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        self.hydrate_delivery_records(records).await
    }

    /// Claims due envelopes for one locally connected, fully scoped target.
    ///
    /// # Errors
    ///
    /// Rejects invalid owner/limit/lease values and propagates database or
    /// payload-hydration errors.
    pub async fn claim_deliveries_for_target(
        &self,
        target: &RealtimeTarget,
        owner: &str,
        limit: usize,
        lease_duration: Duration,
    ) -> AppResult<Vec<DeliveryClaim>> {
        validate_owner(owner)?;
        let limit = validate_delivery_limit(limit, 1_000)?;
        let lease_milliseconds = i64::try_from(lease_duration.as_millis())
            .map_err(|error| AppError::validation(error.to_string()))?;
        if lease_milliseconds <= 0 {
            return Err(AppError::validation(
                "delivery lease duration must be positive",
            ));
        }
        let mut transaction = self.pool().begin().await?;
        let records = sqlx::query_as::<_, DeliveryRecord>(
            r#"
            WITH candidates AS (
                SELECT id
                FROM dm.actor_deliveries
                WHERE organization_id = $1
                  AND target_kind = $2::text::dm.actor_kind
                  AND target_id = $3
                  AND acked_at IS NULL
                  AND dead_lettered_at IS NULL
                  AND next_attempt_at <= transaction_timestamp()
                  AND (
                      lease_owner IS NULL
                      OR lease_expires_at <= transaction_timestamp()
                  )
                ORDER BY sequence
                LIMIT $4
                FOR UPDATE SKIP LOCKED
            )
            UPDATE dm.actor_deliveries AS delivery
            SET lease_owner = $5,
                lease_started_at = transaction_timestamp(),
                lease_expires_at = transaction_timestamp()
                    + ($6::bigint * interval '1 millisecond')
            FROM candidates
            WHERE delivery.id = candidates.id
            RETURNING
                delivery.id,
                delivery.organization_id,
                delivery.target_kind::text AS target_kind,
                delivery.target_id,
                delivery.sequence,
                delivery.delivery_kind::text AS delivery_kind,
                delivery.message_id,
                delivery.aggregate_status::text AS aggregate_status,
                delivery.system_event_id,
                delivery.attempt_count
            "#,
        )
        .bind(target.organization_id.as_str())
        .bind(target.actor.actor_type.as_str())
        .bind(target.actor.id.as_str())
        .bind(limit)
        .bind(owner)
        .bind(lease_milliseconds)
        .fetch_all(&mut *transaction)
        .await
        .map_err(map_constraint_error)?;
        transaction.commit().await.map_err(map_constraint_error)?;

        let deliveries = self.hydrate_delivery_records(records.clone()).await?;
        records
            .into_iter()
            .zip(deliveries)
            .map(|(record, delivery)| {
                let attempt_count = u16::try_from(record.attempt_count).map_err(|error| {
                    AppError::internal(anyhow::anyhow!(
                        "invalid delivery attempt count loaded from DM database: {error}"
                    ))
                })?;
                Ok(DeliveryClaim {
                    delivery,
                    attempt_count,
                })
            })
            .collect()
    }

    /// Releases a worker lease and optionally records one actual processing error.
    ///
    /// # Errors
    ///
    /// Returns conflict when the caller no longer owns the claim.
    pub async fn release_delivery_claim(
        &self,
        delivery_id: Uuid,
        owner: &str,
        next_attempt_at: OffsetDateTime,
        error_code: Option<&str>,
        increment_attempt: bool,
    ) -> AppResult<()> {
        validate_owner(owner)?;
        validate_error_code(error_code)?;
        let result = sqlx::query(
            r#"
            UPDATE dm.actor_deliveries
            SET next_attempt_at = $3,
                attempt_count = attempt_count + CASE WHEN $5 THEN 1 ELSE 0 END,
                last_attempt_at = CASE
                    WHEN $5 THEN transaction_timestamp()
                    ELSE last_attempt_at
                END,
                last_error_code = $4,
                lease_owner = NULL,
                lease_started_at = NULL,
                lease_expires_at = NULL
            WHERE id = $1
              AND lease_owner = $2
              AND acked_at IS NULL
              AND dead_lettered_at IS NULL
            "#,
        )
        .bind(delivery_id)
        .bind(owner)
        .bind(next_attempt_at)
        .bind(error_code)
        .bind(increment_attempt)
        .execute(self.pool())
        .await
        .map_err(map_constraint_error)?;
        if result.rows_affected() == 1 {
            return Ok(());
        }
        if delivery_is_terminal(self, delivery_id).await? {
            return Ok(());
        }
        Err(AppError::conflict("delivery claim is no longer owned"))
    }

    /// Dead-letters a repeatedly failing claim and atomically fails a still-sent
    /// source message with a durable sender status frame.
    ///
    /// # Errors
    ///
    /// Returns conflict when the lease was lost and propagates durable invariant
    /// failures.
    pub async fn dead_letter_delivery(
        &self,
        delivery_id: Uuid,
        owner: &str,
        error_code: &str,
    ) -> AppResult<()> {
        validate_owner(owner)?;
        validate_error_code(Some(error_code))?;
        let mut transaction = self.pool().begin().await?;
        let message_id = sqlx::query_scalar::<_, Option<Uuid>>(
            r#"
            UPDATE dm.actor_deliveries
            SET attempt_count = attempt_count + 1,
                last_attempt_at = transaction_timestamp(),
                last_error_code = $3,
                dead_lettered_at = transaction_timestamp(),
                lease_owner = NULL,
                lease_started_at = NULL,
                lease_expires_at = NULL
            WHERE id = $1
              AND lease_owner = $2
              AND acked_at IS NULL
              AND dead_lettered_at IS NULL
            RETURNING CASE WHEN delivery_kind = 'message' THEN message_id END
            "#,
        )
        .bind(delivery_id)
        .bind(owner)
        .bind(error_code)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(map_constraint_error)?;
        let Some(message_id) = message_id else {
            transaction.rollback().await?;
            if delivery_is_terminal(self, delivery_id).await? {
                return Ok(());
            }
            return Err(AppError::conflict("delivery claim is no longer owned"));
        };

        if let Some(message_id) = message_id {
            fail_source_message_in(&mut transaction, message_id, error_code).await?;
        }
        transaction.commit().await.map_err(map_constraint_error)
    }

    /// Deletes expired ACK envelopes and completed/abandoned idempotency rows.
    ///
    /// # Errors
    ///
    /// Propagates database failures.
    pub async fn compact_delivery_state(&self) -> AppResult<u64> {
        let deliveries = sqlx::query(
            r#"
            DELETE FROM dm.actor_deliveries
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
            DELETE FROM dm.idempotency_records
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

    async fn hydrate_delivery_records(
        &self,
        records: Vec<DeliveryRecord>,
    ) -> AppResult<Vec<ActorDelivery>> {
        let mut deliveries = Vec::with_capacity(records.len());
        for record in records {
            let organization_id = record.organization_id.parse().map_err(|error| {
                AppError::internal(anyhow::anyhow!(
                    "invalid delivery organization loaded from DM database: {error}"
                ))
            })?;
            let target = ActorRef {
                actor_type: parse_actor_type(&record.target_kind)?,
                id: parse_actor_id(&record.target_id)?,
            };
            let payload = match record.delivery_kind.as_str() {
                "message" => DeliveryPayload::Message {
                    message: Box::new(
                        self.load_message(record.message_id.ok_or_else(|| {
                            AppError::internal(anyhow::anyhow!(
                                "message delivery has no source message"
                            ))
                        })?)
                        .await?,
                    ),
                },
                "message_status" => DeliveryPayload::Receipt {
                    message_id: record.message_id.ok_or_else(|| {
                        AppError::internal(anyhow::anyhow!(
                            "message status delivery has no source message"
                        ))
                    })?,
                    status: parse_message_status(record.aggregate_status.as_deref().ok_or_else(
                        || {
                            AppError::internal(anyhow::anyhow!(
                                "message status delivery has no aggregate status"
                            ))
                        },
                    )?)?,
                },
                "system_event" => DeliveryPayload::SystemEvent {
                    event: Box::new(
                        self.load_system_event(record.system_event_id.ok_or_else(|| {
                            AppError::internal(anyhow::anyhow!(
                                "system event delivery has no source event"
                            ))
                        })?)
                        .await?,
                    ),
                },
                other => {
                    return Err(AppError::internal(anyhow::anyhow!(
                        "invalid delivery kind loaded from DM database: {other}"
                    )));
                }
            };
            deliveries.push(ActorDelivery {
                id: record.id,
                organization_id,
                target,
                sequence: record.sequence,
                payload,
            });
        }
        Ok(deliveries)
    }

    async fn load_system_event(&self, event_id: Uuid) -> AppResult<SystemEvent> {
        let record = sqlx::query_as::<_, SystemEventRecord>(
            r#"
            SELECT
                event_id,
                organization_id,
                target_silicon_id,
                event_type,
                trace_id,
                payload
            FROM dm.system_events
            WHERE event_id = $1
            "#,
        )
        .bind(event_id)
        .fetch_optional(self.pool())
        .await?
        .ok_or_else(|| {
            AppError::internal(anyhow::anyhow!(
                "delivery references a missing system event"
            ))
        })?;
        let Value::Object(payload) = record.payload else {
            return Err(AppError::internal(anyhow::anyhow!(
                "system event payload loaded from DM database is not an object"
            )));
        };
        Ok(SystemEvent {
            event_id: record.event_id,
            org_id: record.organization_id.parse().map_err(|error| {
                AppError::internal(anyhow::anyhow!(
                    "invalid system event organization loaded from DM database: {error}"
                ))
            })?,
            silicon_id: parse_actor_id(&record.target_silicon_id)?,
            event_type: record.event_type,
            trace_id: record.trace_id,
            payload,
        })
    }
}

async fn delivery_is_terminal(store: &PostgresStore, delivery_id: Uuid) -> AppResult<bool> {
    sqlx::query_scalar::<_, bool>(
        r#"
        SELECT acked_at IS NOT NULL OR dead_lettered_at IS NOT NULL
        FROM dm.actor_deliveries
        WHERE id = $1
        "#,
    )
    .bind(delivery_id)
    .fetch_optional(store.pool())
    .await
    .map(Option::unwrap_or_default)
    .map_err(AppError::Database)
}

async fn fail_source_message_in(
    transaction: &mut Transaction<'_, Postgres>,
    message_id: Uuid,
    error_code: &str,
) -> AppResult<()> {
    let source = sqlx::query_as::<_, FailedMessageSource>(
        r#"
        UPDATE dm.messages
        SET status = 'failed',
            failure_reason = $2
        WHERE id = $1
          AND status IN ('waiting', 'sent')
        RETURNING
            organization_id,
            conversation_id,
            id AS message_id,
            sender_kind::text AS sender_kind,
            sender_id
        "#,
    )
    .bind(message_id)
    .bind(error_code)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(map_constraint_error)?;
    let Some(source) = source else {
        return Ok(());
    };
    let organization_id: OrganizationId = source.organization_id.parse().map_err(|error| {
        AppError::internal(anyhow::anyhow!(
            "invalid failed-message organization loaded from DM database: {error}"
        ))
    })?;
    let sender = ActorRef {
        actor_type: parse_actor_type(&source.sender_kind)?,
        id: parse_actor_id(&source.sender_id)?,
    };
    let result = sqlx::query(
        r#"
        INSERT INTO dm.actor_deliveries (
            id,
            organization_id,
            target_kind,
            target_id,
            delivery_kind,
            conversation_id,
            message_id,
            aggregate_status
        )
        VALUES (
            $1, $2, $3::text::dm.actor_kind, $4, 'message_status', $5, $6, 'failed'
        )
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(organization_id.as_str())
    .bind(sender.actor_type.as_str())
    .bind(sender.id.as_str())
    .bind(source.conversation_id)
    .bind(source.message_id)
    .execute(&mut **transaction)
    .await
    .map_err(map_constraint_error)?;
    if result.rows_affected() == 1 {
        notify_actor_in(transaction, &organization_id, &sender).await?;
    }
    Ok(())
}

fn validate_delivery_limit(limit: usize, maximum: usize) -> AppResult<i64> {
    if !(1..=maximum).contains(&limit) {
        return Err(AppError::validation(format!(
            "delivery batch size must contain 1 to {maximum} rows"
        )));
    }
    i64::try_from(limit).map_err(|error| AppError::internal(anyhow::Error::new(error)))
}

fn validate_consumer_id(consumer_id: &str) -> AppResult<()> {
    if consumer_id.is_empty()
        || consumer_id.len() > 255
        || consumer_id.chars().any(char::is_control)
    {
        return Err(AppError::validation(
            "consumer_id must contain 1 to 255 non-control characters",
        ));
    }
    Ok(())
}

fn validate_owner(owner: &str) -> AppResult<()> {
    if owner.is_empty() || owner.len() > 255 || owner.chars().any(char::is_control) {
        return Err(AppError::validation(
            "delivery lease owner must contain 1 to 255 non-control characters",
        ));
    }
    Ok(())
}

fn validate_error_code(error_code: Option<&str>) -> AppResult<()> {
    if error_code.is_some_and(|code| {
        code.is_empty() || code.len() > 255 || code.chars().any(char::is_control)
    }) {
        return Err(AppError::validation(
            "delivery error code must contain 1 to 255 non-control characters",
        ));
    }
    Ok(())
}
