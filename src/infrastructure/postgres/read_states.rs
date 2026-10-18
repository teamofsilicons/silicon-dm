//! Unread counts and reading in bulk.
//!
//! A message is unread by an account when it is from someone else, not deleted, not a bundle
//! member (the bundle's display message stands for it) and the account has no read receipt
//! for it on any device. `conversation_read_states.read_through_sequence` is a position
//! below which nothing is unread, so a count only scans what lies above it; receipts move it
//! forward and nothing ever moves it back (reads are never undone, deletions and bundling
//! only remove messages from the count, and new messages always come after it). An account
//! that joins a group starts there at the newest message: what was said before it joined
//! never counts as unread (it was never delivered to it, and the senders' ticks never waited
//! for it). `delivered_through_sequence` is the same kind of position for delivered
//! receipts, so a bulk receipt only ever scans the messages above what it already marked.

use std::collections::HashMap;

use sqlx::{FromRow, Postgres, Transaction};
use uuid::Uuid;

use super::{PostgresStore, map_constraint_error, messages::require_participant_in};
use crate::{
    AppError, AppResult,
    application::commands::BulkReceiptCommand,
    domain::{ActorRef, ReceiptStatus, UnreadTotal},
};

/// Most messages one bulk receipt marks, newest first; a larger backlog is marked by repeating
/// the call (the answer's `unread_count` says what is left).
pub const MAX_BULK_RECEIPTS: i64 = 5_000;

/// Most unread messages a count counts: a conversation with more says this many (show it as
/// "999+"). Counting is a scan of the unread messages, so an account that never marks
/// anything read (a Silicon that only reads its inbox) would otherwise pay for its whole
/// history on every list.
pub const UNREAD_COUNT_CAP: i64 = 1_000;

/// A message of `recent` (a conversation and its read position) unread by account `$1`/`$2`:
/// from someone else, not deleted, not a bundle member, without its read receipt.
const UNREAD_FOR_RECENT: &str = "m.conversation_id = recent.id AND m.sequence > recent.read_through
      AND NOT (m.sender_kind = $1::text::actor_kind AND m.sender_id = $2)
      AND NOT EXISTS (SELECT 1 FROM message_history h
                      WHERE h.message_id = m.id AND h.deleted_at IS NOT NULL)
      AND NOT EXISTS (SELECT 1 FROM message_bundle_items b
                      WHERE b.message_id = m.id AND b.role = 'member')
      AND NOT EXISTS (SELECT 1 FROM message_receipts r
                      WHERE r.message_id = m.id AND r.recipient_kind = $1::text::actor_kind
                        AND r.recipient_id = $2 AND r.status = 'read')";

/// Conversations an unread total looks at: the account's most recently active ones. A total
/// over every conversation would cost an account in thousands of them (a busy Silicon) a scan
/// per conversation on every first page; a badge needs the recent ones.
pub const UNREAD_TOTAL_CONVERSATIONS: i64 = 1_000;

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
        unread_summaries_with(self.pool(), subject, conversation_ids).await
    }

    /// What is unread across `subject`'s conversations (its [`UNREAD_TOTAL_CONVERSATIONS`]
    /// most recently active ones, as the list orders them), in one bounded query: each
    /// conversation is asked whether it has an unread message (the first one found ends that
    /// probe), and messages are counted up to [`UNREAD_COUNT_CAP`] in all.
    ///
    /// # Errors
    /// Database failures.
    pub async fn unread_total(&self, subject: &ActorRef) -> AppResult<UnreadTotal> {
        // Both probes read each conversation's messages by its index, with their own bound (a
        // LIMIT inside LATERAL keeps the planner from hashing every message of the service; a
        // CTE of the unread messages, read twice, would be materialized whole first).
        let (conversations, messages): (i64, i64) = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            r#"
            WITH recent AS MATERIALIZED (
                SELECT conversation.id, COALESCE(state.read_through_sequence, 0) AS read_through
                FROM conversations AS conversation
                JOIN effective_conversation_participants AS participant
                  ON participant.conversation_id = conversation.id
                 AND participant.organization_id = conversation.organization_id
                LEFT JOIN conversation_read_states AS state
                  ON state.conversation_id = conversation.id
                 AND state.actor_kind = $1::text::actor_kind AND state.actor_id = $2
                WHERE participant.actor_kind = $1::text::actor_kind AND participant.actor_id = $2
                  AND conversation.superseded_by IS NULL
                ORDER BY conversation.updated_at DESC, conversation.id DESC
                LIMIT $3
            )
            SELECT
                (SELECT count(*) FROM recent
                 CROSS JOIN LATERAL (SELECT 1 FROM messages m WHERE {UNREAD_FOR_RECENT} LIMIT 1) one),
                (SELECT count(*) FROM (
                    SELECT 1 FROM recent
                    CROSS JOIN LATERAL (SELECT 1 FROM messages m WHERE {UNREAD_FOR_RECENT} LIMIT $4) one
                    LIMIT $4) counted)
            "#
        )))
        .bind(subject.actor_type.as_str())
        .bind(subject.id.as_str())
        .bind(UNREAD_TOTAL_CONVERSATIONS)
        .bind(UNREAD_COUNT_CAP)
        .fetch_one(self.pool())
        .await?;
        Ok(UnreadTotal {
            conversations,
            messages,
        })
    }

    /// Marks every message from others up to a sequence (inclusive) read (which implies
    /// delivered) or delivered for the recipient, in one transaction: the same receipts,
    /// aggregate status changes, `message_status` events and Ting handoffs as one receipt per
    /// message, and one read event for the reader's own devices when it read something.
    /// Messages the account already has that state for are left alone, so repeating the call
    /// changes nothing. Only messages above the account's read (or delivered) position are
    /// looked at, newest first, so the messages on screen are marked even when a backlog takes
    /// more than one call.
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
        // Nothing at or below the account's read position is unread, and nothing at or below
        // its delivered position lacks a receipt: the scan starts above them, so a call costs
        // what is new rather than the whole conversation.
        let (read_through, delivered_through): (i64, i64) = sqlx::query_as(
            "SELECT read_through_sequence, delivered_through_sequence FROM conversation_read_states
             WHERE conversation_id = $1 AND actor_kind = $2::text::actor_kind AND actor_id = $3",
        )
        .bind(command.conversation_id)
        .bind(command.recipient.actor_type.as_str())
        .bind(command.recipient.id.as_str())
        .fetch_optional(&mut *tx)
        .await?
        .unwrap_or((0, 0));
        let floor = match command.status {
            ReceiptStatus::Read => read_through,
            ReceiptStatus::Delivered => read_through.max(delivered_through),
        };
        // The newest first (what the reader sees), locked in sequence order, the order every
        // writer that locks several messages takes them in.
        let targets = sqlx::query_as::<_, TargetRecord>(
            r#"
            SELECT m.id, m.sender_kind::text AS sender_kind, m.sender_id, m.status::text AS status,
                   m.sequence
            FROM messages m
            WHERE m.id IN (
                SELECT candidate.id
                FROM messages candidate
                WHERE candidate.conversation_id = $1 AND candidate.organization_id = $2
                  AND candidate.sequence <= $3 AND candidate.sequence > $8
                  AND NOT (candidate.sender_kind = $4::text::actor_kind AND candidate.sender_id = $5)
                  AND NOT EXISTS (SELECT 1 FROM message_history h
                                  WHERE h.message_id = candidate.id AND h.deleted_at IS NOT NULL)
                  AND NOT EXISTS (SELECT 1 FROM message_receipts r
                                  WHERE r.message_id = candidate.id
                                    AND r.recipient_kind = $4::text::actor_kind
                                    AND r.recipient_id = $5
                                    AND (r.status = 'read' OR $6 = 'delivered'))
                ORDER BY candidate.sequence DESC
                LIMIT $7
            )
            ORDER BY m.sequence
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
        .bind(floor)
        .fetch_all(&mut *tx)
        .await?;
        // Every message above the floor up to `up_to` has the receipt now, unless the call was
        // capped (then older ones are left for the next call).
        let complete = i64::try_from(targets.len()).unwrap_or(i64::MAX) < MAX_BULK_RECEIPTS;
        // The senders hear about every message whose aggregate state moved, exactly as with
        // single receipts.
        let mut announcements: Vec<(&TargetRecord, Uuid, String)> = Vec::new();
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
        }
        // The read position before any stream: every writer that moves it (receipts, joining a
        // group) takes it before the accounts' streams.
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
        if complete && command.up_to_sequence > delivered_through {
            sqlx::query(
                "UPDATE conversation_read_states
                 SET delivered_through_sequence = GREATEST(delivered_through_sequence, $4)
                 WHERE conversation_id = $1 AND actor_kind = $2::text::actor_kind AND actor_id = $3",
            )
            .bind(command.conversation_id)
            .bind(command.recipient.actor_type.as_str())
            .bind(command.recipient.id.as_str())
            .bind(command.up_to_sequence)
            .execute(&mut *tx)
            .await
            .map_err(map_constraint_error)?;
        }
        // Every message marked read was unread for the account (targets lack its read
        // receipt), so its other devices hear the new count.
        let read_event = newest_read.is_some();
        let mut streams: Vec<(&str, &str)> = announcements
            .iter()
            .map(|(target, _, _)| (target.sender_kind.as_str(), target.sender_id.as_str()))
            .collect();
        if read_event {
            streams.push((
                command.recipient.actor_type.as_str(),
                command.recipient.id.as_str(),
            ));
        }
        lock_streams_in(&mut tx, &streams).await?;
        if !announcements.is_empty() {
            // Each sender still hears in message order.
            let delivery_ids: Vec<Uuid> = announcements.iter().map(|_| Uuid::now_v7()).collect();
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
                ORDER BY d.kind::actor_kind, d.sender, d.position
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
        let counted = if read_event {
            Some(record_read_event_in(&mut tx, command.conversation_id, &command.recipient).await?)
        } else {
            None
        };
        tx.commit().await.map_err(map_constraint_error)?;
        let summary = match counted {
            Some(summary) => summary,
            None => self
                .unread_summaries(&command.recipient, &[command.conversation_id])
                .await?
                .remove(&command.conversation_id)
                .unwrap_or_default(),
        };
        Ok(BulkReceiptOutcome {
            updated: u64::try_from(targets.len()).unwrap_or(u64::MAX),
            summary,
        })
    }
}

/// Starts an account that just joined a conversation at its newest message: nothing said
/// before it joined was delivered to it, so nothing of it is unread or waits for its
/// receipts. A position it already had is never moved back.
pub(crate) async fn start_reading_at_newest_in(
    tx: &mut Transaction<'_, Postgres>,
    conversation_id: Uuid,
    actor: &ActorRef,
) -> AppResult<()> {
    sqlx::query(
        r#"
        INSERT INTO conversation_read_states AS state
            (conversation_id, actor_kind, actor_id, read_through_sequence,
             delivered_through_sequence)
        SELECT $1, $2::text::actor_kind, $3, newest.sequence, newest.sequence
        FROM (SELECT COALESCE(max(sequence), 0) AS sequence FROM messages
              WHERE conversation_id = $1) newest
        ON CONFLICT (conversation_id, actor_kind, actor_id) DO UPDATE
        SET read_through_sequence = GREATEST(state.read_through_sequence,
                                             EXCLUDED.read_through_sequence),
            delivered_through_sequence = GREATEST(state.delivered_through_sequence,
                                                  EXCLUDED.delivered_through_sequence),
            updated_at = clock_timestamp()
        "#,
    )
    .bind(conversation_id)
    .bind(actor.actor_type.as_str())
    .bind(actor.id.as_str())
    .execute(&mut **tx)
    .await
    .map_err(map_constraint_error)?;
    Ok(())
}

/// Locks these accounts' sync streams, in (kind, account) order: the order every writer takes
/// them in (a send's deliveries, conversation events). A writer that adds to several streams
/// takes them all first, before counting what a read event says, so its own rows can go in
/// any order without waiting on a concurrent send in a cycle. Accounts without a stream yet
/// get one from the first row added (nothing to wait for).
///
/// # Errors
/// Database failures.
pub(crate) async fn lock_streams_in(
    tx: &mut Transaction<'_, Postgres>,
    accounts: &[(&str, &str)],
) -> AppResult<()> {
    if accounts.is_empty() {
        return Ok(());
    }
    let kinds: Vec<&str> = accounts.iter().map(|(kind, _)| *kind).collect();
    let ids: Vec<&str> = accounts.iter().map(|(_, id)| *id).collect();
    sqlx::query(
        "SELECT 1 FROM actor_delivery_streams stream
         WHERE stream.organization_id = '@accounts'
           AND (stream.actor_kind, stream.actor_id) IN (
               SELECT account.kind::actor_kind, account.id
               FROM unnest($1::text[], $2::text[]) AS account(kind, id))
         ORDER BY stream.actor_kind, stream.actor_id
         FOR UPDATE OF stream",
    )
    .bind(&kinds)
    .bind(&ids)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// The reader's read event: a conversation event `read` in its own stream with its unread
/// facts afterwards, so its other devices update their badges at once; its long-polls wake
/// after commit. The caller holds the reader's stream lock ([`lock_streams_in`]): the count
/// then includes every message delivered to the reader before the event's position and none
/// after, so a client applying its feed in order ends with the right count.
///
/// # Errors
/// Database failures.
pub(crate) async fn record_read_event_in(
    tx: &mut Transaction<'_, Postgres>,
    conversation_id: Uuid,
    reader: &ActorRef,
) -> AppResult<UnreadSummary> {
    let summary = unread_summaries_with(&mut **tx, reader, &[conversation_id])
        .await?
        .remove(&conversation_id)
        .unwrap_or_default();
    sqlx::query(
        "INSERT INTO conversation_events (id, target_kind, target_id, conversation_id, change,
             sequence, unread_count, last_read_sequence)
         VALUES ($1, $2::text::actor_kind, $3, $4, 'read', 1, $5, $6)",
    )
    .bind(Uuid::now_v7())
    .bind(reader.actor_type.as_str())
    .bind(reader.id.as_str())
    .bind(conversation_id)
    .bind(i32::try_from(summary.unread_count).unwrap_or(i32::MAX))
    .bind(summary.last_read_sequence)
    .execute(&mut **tx)
    .await
    .map_err(map_constraint_error)?;
    super::messages::notify_account_in(tx, reader.id.as_str()).await?;
    Ok(summary)
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

/// Unread facts of `subject` for some conversations, in one query, through `executor` (the
/// pool, or a transaction that must count what it wrote).
async fn unread_summaries_with<'e>(
    executor: impl sqlx::PgExecutor<'e>,
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
            SELECT count(*) AS unread FROM (
                SELECT 1
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
                LIMIT $4
            ) counted
        ) u ON true
        "#,
    )
    .bind(conversation_ids)
    .bind(subject.actor_type.as_str())
    .bind(subject.id.as_str())
    .bind(UNREAD_COUNT_CAP)
    .fetch_all(executor)
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
