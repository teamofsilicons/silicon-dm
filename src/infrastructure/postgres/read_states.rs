//! Unread counts and reading in bulk.
//!
//! A message is unread by an account when it is from someone else, not deleted, not a bundle
//! member (the bundle's display message stands for it) and the account has no read receipt
//! for it on any device. `conversation_read_states.read_through_sequence` is a position
//! below which nothing is unread, so a count only scans what lies above it; receipts move it
//! forward and nothing ever moves it back (reads are never undone, deletions and bundling
//! only remove messages from the count, and new messages always come after it).

use std::collections::HashMap;

use sqlx::{FromRow, Postgres, Transaction};
use uuid::Uuid;

use super::{PostgresStore, map_constraint_error, messages::require_participant_in};
use crate::{
    AppError, AppResult,
    application::commands::BulkReceiptCommand,
    domain::{ActorRef, ReceiptStatus},
};

/// Most messages one bulk receipt marks; a larger backlog is marked by repeating the call
/// (the answer's `unread_count` says what is left).
pub const MAX_BULK_RECEIPTS: i64 = 5_000;

/// One account's unread facts for one conversation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct UnreadSummary {
    /// Unread messages.
    pub unread_count: i64,
    /// Sequence of the newest message the account has read, if any.
    pub last_read_sequence: Option<i64>,
}

/// What a bulk receipt changed.
#[derive(Clone, Copy, Debug)]
pub struct BulkReceiptOutcome {
    /// Messages whose state for the account advanced.
    pub updated: u64,
    /// The account's unread facts afterwards.
    pub summary: UnreadSummary,
}

#[derive(FromRow)]
struct UnreadRecord {
    conversation_id: Uuid,
    unread_count: i64,
    last_read_sequence: Option<i64>,
}

#[derive(FromRow)]
struct TargetRecord {
    id: Uuid,
    sender_kind: String,
    sender_id: String,
    status: String,
    sequence: i64,
}

impl PostgresStore {
    /// Unread facts of `subject` for a page of conversations, in one query.
    ///
    /// # Errors
    /// Database failures.
    pub async fn unread_summaries(
        &self,
        subject: &ActorRef,
        conversation_ids: &[Uuid],
    ) -> AppResult<HashMap<Uuid, UnreadSummary>> {
        if conversation_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let records = sqlx::query_as::<_, UnreadRecord>(
            r#"
            SELECT c.id AS conversation_id,
                   COALESCE(u.unread, 0) AS unread_count,
                   s.last_read_sequence
            FROM unnest($1::uuid[]) AS c(id)
            LEFT JOIN conversation_read_states s
              ON s.conversation_id = c.id AND s.actor_kind = $2::text::actor_kind AND s.actor_id = $3
            LEFT JOIN LATERAL (
                SELECT count(*) AS unread
                FROM messages m
                WHERE m.conversation_id = c.id
                  AND m.sequence > COALESCE(s.read_through_sequence, 0)
                  AND NOT (m.sender_kind = $2::text::actor_kind AND m.sender_id = $3)
                  AND NOT EXISTS (SELECT 1 FROM message_history h
                                  WHERE h.message_id = m.id AND h.deleted_at IS NOT NULL)
                  AND NOT EXISTS (SELECT 1 FROM message_bundle_items b
                                  WHERE b.message_id = m.id AND b.role = 'member')
                  AND NOT EXISTS (SELECT 1 FROM message_receipts r
                                  WHERE r.message_id = m.id AND r.recipient_kind = $2::text::actor_kind
                                    AND r.recipient_id = $3 AND r.status = 'read')
            ) u ON true
            "#,
        )
        .bind(conversation_ids)
        .bind(subject.actor_type.as_str())
        .bind(subject.id.as_str())
        .fetch_all(self.pool())
        .await?;
        Ok(records
            .into_iter()
            .map(|record| {
                (
                    record.conversation_id,
                    UnreadSummary {
                        unread_count: record.unread_count,
                        last_read_sequence: record.last_read_sequence,
                    },
                )
            })
            .collect())
    }

    /// Marks every message from others up to a sequence (inclusive) read (which implies
    /// delivered) or delivered for the recipient, in one transaction: the same receipts,
    /// aggregate status changes, `message_status` events and Ting handoffs as one receipt per
    /// message. Messages the account already has that state for are left alone, so repeating
    /// the call changes nothing.
    ///
    /// # Errors
    /// Not found for a non-participant; database failures.
    #[allow(
        clippy::too_many_lines,
        reason = "targets, receipts, aggregate announcements and the read position commit together"
    )]
    pub async fn record_receipts_up_to(
        &self,
        command: &BulkReceiptCommand,
    ) -> AppResult<BulkReceiptOutcome> {
        let status = super::messages::receipt_status_name(command.status);
        let mut tx = self.pool().begin().await?;
        let partition =
            require_participant_in(&mut tx, &command.recipient, command.conversation_id).await?;
        // Lock in sequence order, the same order every bulk receipt uses.
        let targets = sqlx::query_as::<_, TargetRecord>(
            r#"
            SELECT m.id, m.sender_kind::text AS sender_kind, m.sender_id, m.status::text AS status,
                   m.sequence
            FROM messages m
            WHERE m.conversation_id = $1 AND m.organization_id = $2 AND m.sequence <= $3
              AND NOT (m.sender_kind = $4::text::actor_kind AND m.sender_id = $5)
              AND NOT EXISTS (SELECT 1 FROM message_history h
                              WHERE h.message_id = m.id AND h.deleted_at IS NOT NULL)
              AND NOT EXISTS (SELECT 1 FROM message_receipts r
                              WHERE r.message_id = m.id AND r.recipient_kind = $4::text::actor_kind
                                AND r.recipient_id = $5 AND (r.status = 'read' OR $6 = 'delivered'))
            ORDER BY m.sequence
            LIMIT $7
            FOR UPDATE OF m
            "#,
        )
        .bind(command.conversation_id)
        .bind(partition.as_str())
        .bind(command.up_to_sequence)
        .bind(command.recipient.actor_type.as_str())
        .bind(command.recipient.id.as_str())
        .bind(status)
        .bind(MAX_BULK_RECEIPTS)
        .fetch_all(&mut *tx)
        .await?;
        if !targets.is_empty() {
            let ids: Vec<Uuid> = targets.iter().map(|target| target.id).collect();
            let receipts: Vec<(Uuid, Uuid)> = sqlx::query_as(
                r#"
                INSERT INTO message_receipts (id, message_id, conversation_id, organization_id,
                    recipient_kind, recipient_id, device_id, status, read_at)
                SELECT gen_random_uuid(), target.id, $2, $3, $4::text::actor_kind, $5, $6,
                       $7::text::receipt_status,
                       CASE WHEN $7 = 'read' THEN clock_timestamp() ELSE NULL END
                FROM unnest($1::uuid[]) WITH ORDINALITY AS target(id, position)
                ORDER BY target.position
                ON CONFLICT (message_id, recipient_kind, recipient_id, device_id) DO UPDATE
                SET status = CASE WHEN message_receipts.status = 'read' THEN 'read'
                                  ELSE EXCLUDED.status END,
                    read_at = CASE
                        WHEN message_receipts.status = 'read' THEN message_receipts.read_at
                        WHEN EXCLUDED.status = 'read'
                            THEN GREATEST(message_receipts.delivered_at, clock_timestamp())
                        ELSE NULL END
                RETURNING message_id, id
                "#,
            )
            .bind(&ids)
            .bind(command.conversation_id)
            .bind(partition.as_str())
            .bind(command.recipient.actor_type.as_str())
            .bind(command.recipient.id.as_str())
            .bind(&command.device_id)
            .bind(status)
            .fetch_all(&mut *tx)
            .await
            .map_err(map_constraint_error)?;
            let receipt_of: HashMap<Uuid, Uuid> = receipts.into_iter().collect();
            let after: HashMap<Uuid, String> = sqlx::query_as::<_, (Uuid, String)>(
                "SELECT id, status::text FROM messages WHERE id = ANY($1)",
            )
            .bind(&ids)
            .fetch_all(&mut *tx)
            .await?
            .into_iter()
            .collect();
            // The sender hears about every message whose aggregate state moved, exactly as
            // with single receipts.
            let mut announcements = Vec::new();
            for target in &targets {
                let Some(now) = after.get(&target.id) else {
                    continue;
                };
                if now != &target.status && matches!(now.as_str(), "delivered" | "read") {
                    let receipt = receipt_of.get(&target.id).copied().ok_or_else(|| {
                        AppError::internal(anyhow::anyhow!("a bulk receipt lost its row"))
                    })?;
                    announcements.push((target, receipt, now.clone()));
                }
            }
            if !announcements.is_empty() {
                let delivery_ids: Vec<Uuid> =
                    announcements.iter().map(|_| Uuid::now_v7()).collect();
                let kinds: Vec<&str> = announcements
                    .iter()
                    .map(|(target, _, _)| target.sender_kind.as_str())
                    .collect();
                let senders: Vec<&str> = announcements
                    .iter()
                    .map(|(target, _, _)| target.sender_id.as_str())
                    .collect();
                let messages: Vec<Uuid> = announcements
                    .iter()
                    .map(|(target, _, _)| target.id)
                    .collect();
                let receipt_ids: Vec<Uuid> = announcements
                    .iter()
                    .map(|(_, receipt, _)| *receipt)
                    .collect();
                let statuses: Vec<&str> = announcements
                    .iter()
                    .map(|(_, _, status)| status.as_str())
                    .collect();
                sqlx::query(
                    r#"
                    INSERT INTO actor_deliveries (id, organization_id, target_kind, target_id,
                        delivery_kind, conversation_id, message_id, receipt_id, aggregate_status)
                    SELECT d.id, $1, d.kind::actor_kind, d.sender, 'message_status', $2,
                           d.message_id, d.receipt_id, d.status::message_status
                    FROM unnest($3::uuid[], $4::text[], $5::text[], $6::uuid[], $7::uuid[], $8::text[])
                        WITH ORDINALITY AS d(id, kind, sender, message_id, receipt_id, status, position)
                    ORDER BY d.position
                    "#,
                )
                .bind(partition.as_str())
                .bind(command.conversation_id)
                .bind(&delivery_ids)
                .bind(&kinds)
                .bind(&senders)
                .bind(&messages)
                .bind(&receipt_ids)
                .bind(&statuses)
                .execute(&mut *tx)
                .await
                .map_err(map_constraint_error)?;
                let mut notified: Vec<&str> = Vec::new();
                for sender in senders {
                    if !notified.contains(&sender) {
                        notified.push(sender);
                        sqlx::query("SELECT pg_notify('dm_delivery', $1)")
                            .bind(serde_json::json!({ "account": sender }).to_string())
                            .execute(&mut *tx)
                            .await?;
                    }
                }
            }
        }
        let newest_read = (command.status == ReceiptStatus::Read)
            .then(|| targets.iter().map(|target| target.sequence).max())
            .flatten();
        advance_read_state_in(
            &mut tx,
            command.conversation_id,
            &command.recipient,
            newest_read,
        )
        .await?;
        tx.commit().await.map_err(map_constraint_error)?;
        let summary = self
            .unread_summaries(&command.recipient, &[command.conversation_id])
            .await?
            .remove(&command.conversation_id)
            .unwrap_or_default();
        Ok(BulkReceiptOutcome {
            updated: u64::try_from(targets.len()).unwrap_or(u64::MAX),
            summary,
        })
    }
}

/// Moves the account's read position forward: past every message that cannot be unread for
/// it, and (with `read_sequence`) records the newest message it read.
pub(crate) async fn advance_read_state_in(
    tx: &mut Transaction<'_, Postgres>,
    conversation_id: Uuid,
    actor: &ActorRef,
    read_sequence: Option<i64>,
) -> AppResult<()> {
    sqlx::query(
        r#"
        INSERT INTO conversation_read_states AS state
            (conversation_id, actor_kind, actor_id, read_through_sequence, last_read_sequence)
        SELECT $1, $2::text::actor_kind, $3,
            COALESCE(
                (SELECT m.sequence - 1 FROM messages m
                  WHERE m.conversation_id = $1
                    AND m.sequence > COALESCE((SELECT read_through_sequence
                        FROM conversation_read_states
                        WHERE conversation_id = $1 AND actor_kind = $2::text::actor_kind
                          AND actor_id = $3), 0)
                    AND NOT (m.sender_kind = $2::text::actor_kind AND m.sender_id = $3)
                    AND NOT EXISTS (SELECT 1 FROM message_history h
                                    WHERE h.message_id = m.id AND h.deleted_at IS NOT NULL)
                    AND NOT EXISTS (SELECT 1 FROM message_bundle_items b
                                    WHERE b.message_id = m.id AND b.role = 'member')
                    AND NOT EXISTS (SELECT 1 FROM message_receipts r
                                    WHERE r.message_id = m.id
                                      AND r.recipient_kind = $2::text::actor_kind
                                      AND r.recipient_id = $3 AND r.status = 'read')
                  ORDER BY m.sequence
                  LIMIT 1),
                (SELECT max(sequence) FROM messages WHERE conversation_id = $1),
                0),
            $4
        ON CONFLICT (conversation_id, actor_kind, actor_id) DO UPDATE
        SET read_through_sequence = GREATEST(state.read_through_sequence,
                                             EXCLUDED.read_through_sequence),
            last_read_sequence = GREATEST(state.last_read_sequence, EXCLUDED.last_read_sequence),
            updated_at = clock_timestamp()
        "#,
    )
    .bind(conversation_id)
    .bind(actor.actor_type.as_str())
    .bind(actor.id.as_str())
    .bind(read_sequence)
    .execute(&mut **tx)
    .await
    .map_err(map_constraint_error)?;
    Ok(())
}
