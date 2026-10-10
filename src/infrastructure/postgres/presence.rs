//! Presence queries over HTTP presence leases (DM no longer serves sockets).

use sqlx::FromRow;
use time::OffsetDateTime;

use crate::{
    AppError, AppResult,
    domain::{Activity, ActorRef, Availability, Presence},
};

use super::PostgresStore;

#[derive(Debug, FromRow)]
struct PresenceRecord {
    online: bool,
    activity: Option<String>,
    last_seen_at: Option<OffsetDateTime>,
}

impl PostgresStore {
    /// Whether `requester` may see `target`'s presence: itself, an account that shares a
    /// conversation with it, or its custodian.
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

    /// Aggregate presence of an account across its devices.
    ///
    /// # Errors
    /// Database failures or malformed durable activity.
    pub async fn get_presence(&self, target: &ActorRef) -> AppResult<Presence> {
        let record = sqlx::query_as::<_, PresenceRecord>(
            r#"
            WITH active AS (
                SELECT lease.activity, lease.activity_expires_at, lease.heartbeat_at AS updated_at
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
