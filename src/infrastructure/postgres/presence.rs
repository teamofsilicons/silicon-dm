//! Presence queries over HTTP presence leases (DM no longer serves sockets).

use sqlx::FromRow;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{
    AppError, AppResult,
    domain::{Activity, ActorRef, Availability, Presence},
};

use super::PostgresStore;

/// Which of an account's activity a presence answer may show. Activity is about the
/// conversation a device named when it reported it (or about none, from clients before
/// DM 0.17 and the CLI without `--conversation`), and is shown only to accounts in that
/// conversation: what someone types to Bo is never shown to Cy.
#[derive(Clone, Copy, Debug)]
pub enum ActivityView<'a> {
    /// Activity about this conversation only (`?conversation=`), for a viewer in it; `None`
    /// when the viewer is not in it: then no activity at all.
    Conversation(Option<Uuid>),
    /// No conversation named: activity about no conversation in particular, or about one
    /// this account is in.
    Viewer(&'a ActorRef),
}

#[derive(Debug, FromRow)]
struct PresenceRecord {
    online: bool,
    activity: Option<String>,
    last_seen_at: Option<OffsetDateTime>,
}

impl PostgresStore {
    /// Whether `requester` may see `target`'s presence: itself, an account that shares a
    /// conversation with it, or its custodian. What it may see of the activity is decided
    /// per conversation ([`ActivityView`]).
    ///
    /// # Errors
    /// Database failures.
    pub async fn may_see_presence(
        &self,
        requester: &ActorRef,
        target: &ActorRef,
    ) -> AppResult<bool> {
        Ok(sqlx::query_scalar(
            "SELECT $1 = $2
                OR EXISTS (
                    SELECT 1 FROM effective_conversation_participants mine
                    JOIN effective_conversation_participants theirs
                      ON theirs.conversation_id = mine.conversation_id
                    WHERE mine.actor_id = $1 AND theirs.actor_id = $2)
                OR EXISTS (SELECT 1 FROM accounts WHERE uuid = $2 AND custodian_uuid = $1)",
        )
        .bind(requester.id.as_str())
        .bind(target.id.as_str())
        .fetch_one(self.pool())
        .await?)
    }

    /// Aggregate presence of an account across its devices: availability and last seen from
    /// all of them, and the newest current activity that `view` may show.
    ///
    /// # Errors
    /// Database failures or malformed durable activity.
    pub async fn get_presence(
        &self,
        target: &ActorRef,
        view: ActivityView<'_>,
    ) -> AppResult<Presence> {
        let (scoped, conversation, viewer) = match view {
            ActivityView::Conversation(conversation) => (true, conversation, None),
            ActivityView::Viewer(viewer) => (false, None, Some(viewer)),
        };
        let record = sqlx::query_as::<_, PresenceRecord>(
            r#"
            WITH active AS (
                SELECT lease.activity, lease.activity_expires_at, lease.activity_conversation_id,
                       lease.heartbeat_at AS updated_at
                FROM client_presence_leases AS lease
                WHERE lease.actor_kind = $1::text::actor_kind
                  AND lease.actor_id = $2
                  AND lease.disconnected_at IS NULL
                  AND lease.lease_expires_at > clock_timestamp()
            )
            SELECT
                EXISTS (SELECT 1 FROM active) AS online,
                (
                    SELECT activity::text FROM active
                    WHERE activity IS NOT NULL AND activity_expires_at > clock_timestamp()
                      AND CASE WHEN $3 THEN activity_conversation_id = $4
                               ELSE activity_conversation_id IS NULL OR EXISTS (
                                   SELECT 1 FROM effective_conversation_participants mine
                                   WHERE mine.conversation_id = active.activity_conversation_id
                                     AND mine.actor_kind = $5::text::actor_kind
                                     AND mine.actor_id = $6)
                          END
                    ORDER BY updated_at DESC
                    LIMIT 1
                ) AS activity,
                GREATEST(
                    (SELECT max(state.last_seen_at) FROM actor_presence_state AS state
                     WHERE state.actor_kind = $1::text::actor_kind AND state.actor_id = $2),
                    (SELECT max(CASE WHEN lease.disconnected_at IS NOT NULL THEN lease.disconnected_at
                                     WHEN lease.lease_expires_at <= clock_timestamp()
                                     THEN lease.lease_expires_at END)
                     FROM client_presence_leases AS lease
                     WHERE lease.actor_kind = $1::text::actor_kind AND lease.actor_id = $2
                       AND NOT EXISTS (SELECT 1 FROM active))
                ) AS last_seen_at
            "#,
        )
        .bind(target.actor_type.as_str())
        .bind(target.id.as_str())
        .bind(scoped)
        .bind(conversation)
        .bind(viewer.map(|viewer| viewer.actor_type.as_str()))
        .bind(viewer.map(|viewer| viewer.id.as_str()))
        .fetch_one(self.pool())
        .await?;
        Ok(Presence {
            actor_id: target.id.clone(),
            availability: if record.online {
                Availability::Online
            } else {
                Availability::Offline
            },
            activity: record.activity.as_deref().map(parse_activity).transpose()?,
            last_seen_at: record.last_seen_at,
        })
    }
}

pub(crate) fn parse_activity(value: &str) -> AppResult<Activity> {
    match value {
        "typing" => Ok(Activity::Typing),
        "recording_voice" => Ok(Activity::RecordingVoice),
        "transcribing_voice" => Ok(Activity::TranscribingVoice),
        "uploading_file" => Ok(Activity::UploadingFile),
        "searching_gifs" => Ok(Activity::SearchingGifs),
        other => Err(AppError::internal(anyhow::anyhow!(
            "invalid presence activity loaded from DM database: {other}"
        ))),
    }
}
