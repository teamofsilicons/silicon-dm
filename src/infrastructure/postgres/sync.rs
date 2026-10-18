//! Bounded authoritative actor event references, independent of Ting delivery ACKs.

use serde::Serialize;
use sqlx::FromRow;
use uuid::Uuid;

use super::PostgresStore;
use crate::{AppError, AppResult, domain::ActorRef};

/// A reference instructing the client to fetch current authorized DM state.
#[derive(Clone, Debug, Serialize)]
pub struct SyncEvent {
    /// Stable DM event identity, also usable across Ting idempotency windows.
    pub event_id: Uuid,
    /// Position in this authenticated actor's authoritative stream.
    pub sequence: i64,
    /// Source category: `message` and `message_status` name a message to fetch;
    /// `conversation` (only when asked for) a conversation that changed for the account.
    #[serde(rename = "type")]
    pub kind: String,
    /// Public conversation address (rendered from the conversation's internal id).
    pub conversation_id: String,
    /// Existing conversation-local message code (message events only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    /// What happened to the conversation (conversation events only), such as `renamed`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub change: Option<String>,
}

/// One bounded scan; position advances over currently inaccessible events too.
pub struct SyncScan {
    /// Currently authorized references.
    pub events: Vec<SyncEvent>,
    /// Last scanned position, never the maximum sequence seen in Ting.
    pub position: i64,
    /// More source rows remain below this scan's fixed upper boundary.
    pub has_more: bool,
}

#[derive(FromRow)]
struct SyncRow {
    id: Uuid,
    sequence: i64,
    kind: String,
    conversation_id: Option<String>,
    message_sequence: Option<i64>,
    change: Option<String>,
    authorized: bool,
}

impl PostgresStore {
    /// The committed head of the account's stream and the floor sync starts above
    /// (sequences at or below it predate the account's Silicon Accounts stream).
    ///
    /// # Errors
    /// Reports storage failures.
    pub async fn sync_head(&self, actor: &ActorRef) -> AppResult<(i64, i64)> {
        Ok(sqlx::query_as::<_, (i64, i64)>(
            "SELECT next_sequence - 1, sync_floor FROM actor_delivery_streams
             WHERE organization_id = '@accounts' AND actor_kind = $1::text::actor_kind AND actor_id = $2",
        )
        .bind(actor.actor_type.as_str())
        .bind(actor.id.as_str())
        .fetch_optional(self.pool())
        .await?
        .unwrap_or((0, 0)))
    }

    /// Scans retained references of the account's stream (whatever partition each
    /// conversation lives in) under current participation. Message events and conversation
    /// events share the stream; conversation events are returned only when `conversations` is
    /// set (`include=conversations`), and otherwise skipped like events the account cannot see,
    /// so older clients never meet them while the cursor still moves past them.
    ///
    /// # Errors
    /// Rejects invalid bounds and requires a snapshot reset if any source range expired.
    pub async fn sync_events(
        &self,
        actor: &ActorRef,
        after: i64,
        upper: i64,
        limit: u16,
        conversations: bool,
    ) -> AppResult<SyncScan> {
        if after < 0 || upper < after || !(1..=100).contains(&limit) {
            return Err(AppError::validation("invalid synchronization bounds"));
        }
        let mut rows = sqlx::query_as::<_, SyncRow>(
            r#"
            SELECT id, sequence, kind, conversation_id, message_sequence, change, authorized
            FROM (
                (SELECT d.id, d.sequence, d.delivery_kind::text AS kind,
                        d.conversation_id::text AS conversation_id,
                        message.sequence AS message_sequence, NULL::text AS change,
                        (d.delivery_kind IN ('message', 'message_status')
                         AND EXISTS (
                             SELECT 1 FROM effective_conversation_participants participant
                             WHERE participant.conversation_id = d.conversation_id
                               AND participant.actor_kind = d.target_kind
                               AND participant.actor_id = d.target_id
                         )) AS authorized
                 FROM actor_deliveries d
                 LEFT JOIN messages message ON message.id = d.message_id
                 WHERE d.target_kind = $1::text::actor_kind
                   AND d.target_id = $2 AND d.sequence > $3 AND d.sequence <= $4
                 ORDER BY d.sequence
                 LIMIT $5)
                UNION ALL
                (SELECT e.id, e.sequence, 'conversation', e.conversation_id::text,
                        NULL::bigint, e.change, $6
                 FROM conversation_events e
                 WHERE e.target_kind = $1::text::actor_kind
                   AND e.target_id = $2 AND e.sequence > $3 AND e.sequence <= $4
                 ORDER BY e.sequence
                 LIMIT $5)
            ) stream
            ORDER BY sequence
            LIMIT $5
            "#,
        )
        .bind(actor.actor_type.as_str())
        .bind(actor.id.as_str())
        .bind(after)
        .bind(upper)
        .bind(i64::from(limit) + 1)
        .bind(conversations)
        .fetch_all(self.pool())
        .await?;
        let has_more = rows.len() > usize::from(limit);
        rows.truncate(usize::from(limit));
        let mut position = after;
        let mut events = Vec::new();
        for row in rows {
            if position.checked_add(1) != Some(row.sequence) {
                return Err(sync_reset_required());
            }
            position = row.sequence;
            if row.authorized {
                let conversation_id = row.conversation_id.ok_or_else(|| {
                    AppError::internal(anyhow::anyhow!("sync event lost its conversation"))
                })?;
                let message_id = if row.kind == "conversation" {
                    None
                } else {
                    Some(
                        row.message_sequence
                            .and_then(silicon_dm_protocol::message_code)
                            .ok_or_else(|| {
                                AppError::internal(anyhow::anyhow!("sync event lost its message"))
                            })?,
                    )
                };
                events.push(SyncEvent {
                    event_id: row.id,
                    sequence: row.sequence,
                    kind: row.kind,
                    conversation_id,
                    message_id,
                    change: row.change,
                });
            }
        }
        if !has_more && position != upper {
            return Err(sync_reset_required());
        }
        Ok(SyncScan {
            events,
            position,
            has_more,
        })
    }
}

/// Public recovery instruction shared by cursor and retained-range checks.
pub(crate) fn sync_reset_required() -> AppError {
    AppError::SyncResetRequired(
        "Synchronization history expired or the environment changed. Request /api/v1/sync?reset=true, refresh conversations and messages, then resume from its cursor.".into(),
    )
}
