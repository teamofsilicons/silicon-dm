//! Bounded authoritative actor event references, independent of Ting delivery ACKs.

use serde::Serialize;
use sqlx::FromRow;
use uuid::Uuid;

use super::PostgresStore;
use crate::{AppError, AppResult, application::auth::AuthContext};

/// A reference instructing the client to fetch current authorized DM state.
#[derive(Clone, Debug, Serialize)]
pub struct SyncEvent {
    /// Stable DM event identity, also usable across Ting idempotency windows.
    pub event_id: Uuid,
    /// Position in this authenticated actor's authoritative stream.
    pub sequence: i64,
    /// Source category; both categories require fetching the current message.
    #[serde(rename = "type")]
    pub kind: String,
    /// Existing public conversation address.
    pub conversation_id: String,
    /// Existing conversation-local message code.
    pub message_id: String,
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
    authorized: bool,
}

impl PostgresStore {
    /// Reads the committed actor-stream head for a fixed sync boundary.
    ///
    /// # Errors
    /// Reports storage failures.
    pub async fn sync_head(&self, auth: &AuthContext) -> AppResult<i64> {
        Ok(sqlx::query_scalar::<_, i64>(
            "SELECT next_sequence - 1 FROM actor_delivery_streams
             WHERE organization_id=$1 AND actor_kind=$2::text::actor_kind AND actor_id=$3",
        )
        .bind(auth.organization_id.as_str())
        .bind(auth.actor.actor_type.as_str())
        .bind(auth.actor.id.as_str())
        .fetch_optional(self.pool())
        .await?
        .unwrap_or(0))
    }

    /// Scans retained references under current participant and exact-token group permissions.
    ///
    /// # Errors
    /// Rejects invalid bounds and requires a snapshot reset if any source range expired.
    pub async fn sync_events(
        &self,
        auth: &AuthContext,
        after: i64,
        upper: i64,
        limit: u16,
    ) -> AppResult<SyncScan> {
        if after < 0 || upper < after || !(1..=100).contains(&limit) {
            return Err(AppError::validation("invalid synchronization bounds"));
        }
        let tags: Vec<_> = auth.tag_ids.iter().flatten().copied().collect();
        let mut rows = sqlx::query_as::<_, SyncRow>(
            r#"
            SELECT d.id, d.sequence, d.delivery_kind::text AS kind,
                   address.public_id AS conversation_id, message.sequence AS message_sequence,
                   (d.delivery_kind IN ('message', 'message_status')
                    AND EXISTS (
                        SELECT 1 FROM effective_conversation_participants participant
                        WHERE participant.organization_id=d.organization_id
                          AND participant.conversation_id=d.conversation_id
                          AND participant.actor_kind=d.target_kind
                          AND participant.actor_id=d.target_id
                    )
                    AND EXISTS (
                        SELECT 1 FROM conversations conversation
                        WHERE conversation.id=d.conversation_id
                          AND conversation.organization_id=d.organization_id
                          AND (NOT conversation.is_group OR EXISTS (
                              SELECT 1 FROM groups app_group
                              WHERE app_group.conversation_id=conversation.id
                                AND (EXISTS (
                                    SELECT 1 FROM group_invitations invitation
                                    WHERE invitation.conversation_id=conversation.id
                                      AND invitation.actor_kind=d.target_kind
                                      AND invitation.actor_id=d.target_id
                                ) OR (app_group.is_public AND $2='carbon')
                                  OR (NOT app_group.is_public AND app_group.tag_ids && $7))
                          ))
                    )) AS authorized
            FROM actor_deliveries d
            LEFT JOIN messages message ON message.id=d.message_id
              AND message.organization_id=d.organization_id
            LEFT JOIN conversation_addresses address ON address.id=d.conversation_id
              AND address.organization_id=d.organization_id
            WHERE d.organization_id=$1 AND d.target_kind=$2::text::actor_kind
              AND d.target_id=$3 AND d.sequence>$4 AND d.sequence<=$5
            ORDER BY d.sequence
            LIMIT $6
            "#,
        )
        .bind(auth.organization_id.as_str())
        .bind(auth.actor.actor_type.as_str())
        .bind(auth.actor.id.as_str())
        .bind(after)
        .bind(upper)
        .bind(i64::from(limit) + 1)
        .bind(tags)
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
                let message_id = row
                    .message_sequence
                    .and_then(silicon_dm_protocol::message_code)
                    .ok_or_else(|| {
                        AppError::internal(anyhow::anyhow!("sync event lost its message"))
                    })?;
                events.push(SyncEvent {
                    event_id: row.id,
                    sequence: row.sequence,
                    kind: row.kind,
                    conversation_id,
                    message_id,
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
