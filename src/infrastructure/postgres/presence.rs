//! Cross-instance realtime session leases and presence queries.

use std::collections::HashSet;

use sqlx::FromRow;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{
    AppError, AppResult,
    application::commands::{OpenRealtimeSessionCommand, UpdateRealtimeActivityCommand},
    domain::{Activity, ActorId, ActorRef, Availability, OrganizationId, Presence},
};

use super::{PostgresStore, directory::refresh_directory_in, map_constraint_error};

#[derive(Debug, FromRow)]
struct PresenceRecord {
    target_count: i64,
    online: bool,
    activity: Option<String>,
    last_seen_at: Option<OffsetDateTime>,
}

impl PostgresStore {
    /// Persists a socket lease and every IAM-authorized represented actor.
    ///
    /// # Errors
    ///
    /// Rejects empty actor sets, expired leases, duplicate session IDs, and
    /// invalid directory references.
    pub async fn open_realtime_session(
        &self,
        mut command: OpenRealtimeSessionCommand,
    ) -> AppResult<()> {
        command.actors.sort_by(|left, right| {
            left.actor_type
                .as_str()
                .cmp(right.actor_type.as_str())
                .then_with(|| left.id.cmp(&right.id))
        });
        command.actors.dedup();
        if command.actors.is_empty() {
            return Err(AppError::validation(
                "a realtime connection must represent at least one actor",
            ));
        }
        validate_session_identifier(&command.instance_id, "instance_id")?;
        validate_session_identifier(&command.consumer_id, "consumer_id")?;
        validate_session_identifier(&command.authenticated_subject, "authenticated subject")?;
        if command
            .actors
            .iter()
            .map(|actor| &actor.id)
            .collect::<HashSet<_>>()
            .len()
            != command.actors.len()
        {
            return Err(AppError::validation(
                "a realtime connection cannot represent duplicate actor IDs",
            ));
        }
        if command.lease_expires_at <= OffsetDateTime::now_utc() {
            return Err(AppError::validation(
                "realtime session lease must expire in the future",
            ));
        }

        let mut transaction = self.pool().begin().await?;
        refresh_directory_in(&mut transaction, &command.organization_id, &command.actors).await?;
        sqlx::query(
            r#"
            INSERT INTO dm.realtime_sessions (
                id,
                instance_id,
                consumer_id,
                authenticated_subject,
                lease_expires_at
            )
            VALUES ($1, $2, $3, $4, $5)
            "#,
        )
        .bind(command.session_id)
        .bind(&command.instance_id)
        .bind(&command.consumer_id)
        .bind(&command.authenticated_subject)
        .bind(command.lease_expires_at)
        .execute(&mut *transaction)
        .await
        .map_err(map_constraint_error)?;
        for actor in &command.actors {
            sqlx::query(
                r#"
                INSERT INTO dm.realtime_session_actors (
                    session_id,
                    organization_id,
                    actor_kind,
                    actor_id
                )
                VALUES ($1, $2, $3::text::dm.actor_kind, $4)
                "#,
            )
            .bind(command.session_id)
            .bind(command.organization_id.as_str())
            .bind(actor.actor_type.as_str())
            .bind(actor.id.as_str())
            .execute(&mut *transaction)
            .await
            .map_err(map_constraint_error)?;
        }
        transaction.commit().await.map_err(map_constraint_error)
    }

    /// Extends a live session lease and optionally records a validated pong.
    ///
    /// # Errors
    ///
    /// Returns not-found when the session is absent, closed, or already stale.
    pub async fn refresh_realtime_session(
        &self,
        session_id: Uuid,
        lease_expires_at: OffsetDateTime,
        pong_received: bool,
    ) -> AppResult<()> {
        if lease_expires_at <= OffsetDateTime::now_utc() {
            return Err(AppError::validation(
                "realtime session lease must expire in the future",
            ));
        }
        let result = sqlx::query(
            r#"
            WITH current_time AS MATERIALIZED (
                SELECT clock_timestamp() AS value
            )
            UPDATE dm.realtime_sessions
            SET last_heartbeat_at = GREATEST(last_heartbeat_at, current_time.value),
                last_pong_at = CASE
                    WHEN $3 THEN GREATEST(last_heartbeat_at, current_time.value)
                    ELSE last_pong_at
                END,
                lease_expires_at = $2
            FROM current_time
            WHERE id = $1
              AND disconnected_at IS NULL
              AND lease_expires_at > clock_timestamp()
            "#,
        )
        .bind(session_id)
        .bind(lease_expires_at)
        .bind(pong_received)
        .execute(self.pool())
        .await
        .map_err(map_constraint_error)?;
        if result.rows_affected() == 1 {
            Ok(())
        } else {
            Err(AppError::NotFound)
        }
    }

    /// Changes an actor's expiring activity inside a live represented session.
    ///
    /// # Errors
    ///
    /// Returns not-found when the actor/session pair is not currently active.
    pub async fn update_realtime_activity(
        &self,
        command: UpdateRealtimeActivityCommand,
    ) -> AppResult<()> {
        if command.activity.is_some() != command.activity_expires_at.is_some() {
            return Err(AppError::validation(
                "presence activity and its expiry must be supplied together",
            ));
        }
        if command
            .activity_expires_at
            .is_some_and(|expiry| expiry <= OffsetDateTime::now_utc())
        {
            return Err(AppError::validation(
                "presence activity expiry must be in the future",
            ));
        }
        let activity = command.activity.map(activity_name);
        let result = sqlx::query(
            r#"
            WITH candidates AS MATERIALIZED (
                SELECT
                    actor.session_id,
                    actor.organization_id,
                    actor.actor_kind,
                    actor.actor_id
                FROM dm.realtime_session_actors AS actor
                JOIN dm.realtime_sessions AS session ON session.id = actor.session_id
                WHERE actor.session_id = $1
                  AND actor.organization_id = $2
                  AND actor.actor_id = $3
                  AND session.disconnected_at IS NULL
                  AND session.lease_expires_at > clock_timestamp()
            ),
            target AS (
                SELECT *
                FROM candidates
                WHERE (SELECT count(*) FROM candidates) = 1
            )
            UPDATE dm.realtime_session_actors AS actor
            SET activity = $4::text::dm.presence_activity,
                activity_set_at = CASE
                    WHEN $4 IS NULL THEN NULL
                    ELSE clock_timestamp()
                END,
                activity_expires_at = $5
            FROM target
            WHERE actor.session_id = target.session_id
              AND actor.organization_id = target.organization_id
              AND actor.actor_kind = target.actor_kind
              AND actor.actor_id = target.actor_id
            "#,
        )
        .bind(command.session_id)
        .bind(command.organization_id.as_str())
        .bind(command.actor_id.as_str())
        .bind(activity)
        .bind(command.activity_expires_at)
        .execute(self.pool())
        .await
        .map_err(map_constraint_error)?;
        if result.rows_affected() == 1 {
            Ok(())
        } else {
            Err(AppError::NotFound)
        }
    }

    /// Marks one socket closed and advances offline last-seen state when needed.
    ///
    /// # Errors
    ///
    /// Rejects an invalid close reason and propagates database failures.
    pub async fn close_realtime_session(
        &self,
        session_id: Uuid,
        close_code: u16,
        close_reason: &str,
    ) -> AppResult<()> {
        if !(1000..=4999).contains(&close_code) {
            return Err(AppError::validation(
                "WebSocket close code must be between 1000 and 4999",
            ));
        }
        if close_reason.len() > 123 {
            return Err(AppError::validation(
                "WebSocket close reason may not exceed 123 bytes",
            ));
        }
        let mut transaction = self.pool().begin().await?;
        lock_realtime_session_actors(&mut transaction, session_id).await?;
        let closed_at = sqlx::query_scalar::<_, OffsetDateTime>(
            r#"
            UPDATE dm.realtime_sessions
            SET disconnected_at = GREATEST(connected_at, clock_timestamp()),
                close_code = $2,
                close_reason = $3
            WHERE id = $1
              AND disconnected_at IS NULL
            RETURNING disconnected_at
            "#,
        )
        .bind(session_id)
        .bind(i32::from(close_code))
        .bind(close_reason)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(map_constraint_error)?;
        if let Some(closed_at) = closed_at {
            record_offline_actors(&mut transaction, session_id, closed_at).await?;
        }
        transaction.commit().await.map_err(map_constraint_error)
    }

    /// Returns aggregate presence for an IAM-authorized actor.
    ///
    /// # Errors
    ///
    /// Returns not-found for unknown or ambiguous actor IDs.
    pub async fn get_presence(
        &self,
        organization_id: &OrganizationId,
        requester: &ActorRef,
        actor_id: &ActorId,
    ) -> AppResult<Presence> {
        let record = sqlx::query_as::<_, PresenceRecord>(
            r#"
            WITH requester AS (
                SELECT 1
                FROM dm.actor_snapshots
                WHERE organization_id = $1
                  AND actor_kind = $2::text::dm.actor_kind
                  AND actor_id = $3
                  AND status = 'active'
            ),
            target AS (
                SELECT actor_kind, actor_id
                FROM dm.actor_snapshots
                WHERE organization_id = $1
                  AND actor_id = $4
                  AND status = 'active'
                  AND EXISTS (SELECT 1 FROM requester)
            ),
            active_sessions AS (
                SELECT actor.activity, actor.activity_expires_at, actor.updated_at
                FROM target
                JOIN dm.realtime_session_actors AS actor
                  ON actor.organization_id = $1
                 AND actor.actor_kind = target.actor_kind
                 AND actor.actor_id = target.actor_id
                JOIN dm.realtime_sessions AS session ON session.id = actor.session_id
                WHERE session.disconnected_at IS NULL
                  AND session.lease_expires_at > clock_timestamp()
            )
            SELECT
                (SELECT count(*) FROM target) AS target_count,
                EXISTS (SELECT 1 FROM active_sessions) AS online,
                (
                    SELECT activity::text
                    FROM active_sessions
                    WHERE activity IS NOT NULL
                      AND activity_expires_at > clock_timestamp()
                    ORDER BY updated_at DESC
                    LIMIT 1
                ) AS activity,
                (
                    SELECT max(state.last_seen_at)
                    FROM target
                    LEFT JOIN dm.actor_presence_state AS state
                      ON state.organization_id = $1
                     AND state.actor_kind = target.actor_kind
                     AND state.actor_id = target.actor_id
                ) AS last_seen_at
            "#,
        )
        .bind(organization_id.as_str())
        .bind(requester.actor_type.as_str())
        .bind(requester.id.as_str())
        .bind(actor_id.as_str())
        .fetch_one(self.pool())
        .await?;
        if record.target_count != 1 {
            return Err(AppError::NotFound);
        }
        Ok(Presence {
            actor_id: actor_id.clone(),
            availability: if record.online {
                Availability::Online
            } else {
                Availability::Offline
            },
            activity: record.activity.as_deref().map(parse_activity).transpose()?,
            last_seen_at: record.last_seen_at,
        })
    }

    /// Closes expired leases and records last-seen state for newly offline actors.
    ///
    /// # Errors
    ///
    /// Propagates database failures.
    pub async fn expire_realtime_sessions(&self) -> AppResult<u64> {
        let mut transaction = self.pool().begin().await?;
        lock_expired_realtime_actors(&mut transaction).await?;
        let expired_count = sqlx::query_scalar::<_, i64>(
            r#"
            WITH expired AS (
                UPDATE dm.realtime_sessions
                SET disconnected_at = lease_expires_at,
                    close_code = 4000,
                    close_reason = 'heartbeat-timeout'
                WHERE disconnected_at IS NULL
                  AND lease_expires_at <= clock_timestamp()
                RETURNING id, lease_expires_at
            ),
            newly_offline AS (
                SELECT
                    actor.organization_id,
                    actor.actor_kind,
                    actor.actor_id,
                    max(expired.lease_expires_at) AS last_seen_at
                FROM expired
                JOIN dm.realtime_session_actors AS actor
                  ON actor.session_id = expired.id
                WHERE NOT EXISTS (
                    SELECT 1
                    FROM dm.realtime_session_actors AS other_actor
                    JOIN dm.realtime_sessions AS other_session
                      ON other_session.id = other_actor.session_id
                    WHERE other_actor.organization_id = actor.organization_id
                      AND other_actor.actor_kind = actor.actor_kind
                      AND other_actor.actor_id = actor.actor_id
                      AND other_session.disconnected_at IS NULL
                      AND other_session.lease_expires_at > clock_timestamp()
                )
                GROUP BY actor.organization_id, actor.actor_kind, actor.actor_id
            ),
            presence_updates AS (
                INSERT INTO dm.actor_presence_state (
                    organization_id,
                    actor_kind,
                    actor_id,
                    last_seen_at,
                    created_at
                )
                SELECT
                    organization_id,
                    actor_kind,
                    actor_id,
                    last_seen_at,
                    last_seen_at
                FROM newly_offline
                ON CONFLICT (organization_id, actor_kind, actor_id)
                DO UPDATE SET last_seen_at = GREATEST(
                    dm.actor_presence_state.last_seen_at,
                    EXCLUDED.last_seen_at
                )
                RETURNING 1
            )
            SELECT count(*) FROM expired
            "#,
        )
        .fetch_one(&mut *transaction)
        .await
        .map_err(map_constraint_error)?;
        transaction.commit().await.map_err(map_constraint_error)?;
        u64::try_from(expired_count).map_err(|error| AppError::internal(anyhow::Error::new(error)))
    }
}

async fn lock_realtime_session_actors(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    session_id: Uuid,
) -> AppResult<()> {
    let keys = sqlx::query_scalar::<_, i64>(
        r#"
        SELECT DISTINCT hashtextextended(
            actor.organization_id || chr(31) || actor.actor_kind::text || chr(31) || actor.actor_id,
            0
        ) AS lock_key
        FROM dm.realtime_session_actors AS actor
        WHERE actor.session_id = $1
        ORDER BY lock_key
        "#,
    )
    .bind(session_id)
    .fetch_all(&mut **transaction)
    .await
    .map_err(map_constraint_error)?;
    lock_actor_keys(transaction, keys).await
}

async fn lock_expired_realtime_actors(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> AppResult<()> {
    let keys = sqlx::query_scalar::<_, i64>(
        r#"
        SELECT DISTINCT hashtextextended(
            actor.organization_id || chr(31) || actor.actor_kind::text || chr(31) || actor.actor_id,
            0
        ) AS lock_key
        FROM dm.realtime_session_actors AS actor
        JOIN dm.realtime_sessions AS session ON session.id = actor.session_id
        WHERE session.disconnected_at IS NULL
          AND session.lease_expires_at <= clock_timestamp()
        ORDER BY lock_key
        "#,
    )
    .fetch_all(&mut **transaction)
    .await
    .map_err(map_constraint_error)?;
    lock_actor_keys(transaction, keys).await
}

async fn lock_actor_keys(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    keys: Vec<i64>,
) -> AppResult<()> {
    // Actor-scoped transaction locks serialize the final offline transition
    // across manual disconnects and lease expiry on every API instance.
    for key in keys {
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(key)
            .execute(&mut **transaction)
            .await
            .map_err(map_constraint_error)?;
    }
    Ok(())
}

async fn record_offline_actors(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    session_id: Uuid,
    closed_at: OffsetDateTime,
) -> AppResult<()> {
    sqlx::query(
        r#"
        INSERT INTO dm.actor_presence_state (
            organization_id,
            actor_kind,
            actor_id,
            last_seen_at,
            created_at
        )
        SELECT
            actor.organization_id,
            actor.actor_kind,
            actor.actor_id,
            $2,
            $2
        FROM dm.realtime_session_actors AS actor
        WHERE actor.session_id = $1
          AND NOT EXISTS (
              SELECT 1
              FROM dm.realtime_session_actors AS other_actor
              JOIN dm.realtime_sessions AS other_session
                ON other_session.id = other_actor.session_id
              WHERE other_actor.organization_id = actor.organization_id
                AND other_actor.actor_kind = actor.actor_kind
                AND other_actor.actor_id = actor.actor_id
                AND other_session.disconnected_at IS NULL
                AND other_session.lease_expires_at > clock_timestamp()
          )
        ON CONFLICT (organization_id, actor_kind, actor_id)
        DO UPDATE SET last_seen_at = GREATEST(
            dm.actor_presence_state.last_seen_at,
            EXCLUDED.last_seen_at
        )
        "#,
    )
    .bind(session_id)
    .bind(closed_at)
    .execute(&mut **transaction)
    .await
    .map_err(map_constraint_error)?;
    Ok(())
}

const fn activity_name(activity: Activity) -> &'static str {
    match activity {
        Activity::Typing => "typing",
        Activity::RecordingVoice => "recording_voice",
        Activity::TranscribingVoice => "transcribing_voice",
        Activity::UploadingFile => "uploading_file",
        Activity::SearchingGifs => "searching_gifs",
    }
}

fn parse_activity(value: &str) -> AppResult<Activity> {
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

fn validate_session_identifier(value: &str, field: &str) -> AppResult<()> {
    if value.is_empty() || value.len() > 255 || value.chars().any(char::is_control) {
        return Err(AppError::validation(format!(
            "{field} must contain 1 to 255 non-control characters"
        )));
    }
    Ok(())
}
