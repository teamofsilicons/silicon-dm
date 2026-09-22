//! Authenticated HTTP device leases, independent of notification transport.

use std::time::Duration;

use serde::Serialize;
use time::OffsetDateTime;

use super::{PostgresStore, directory::refresh_directory_in};
use crate::{
    AppError, AppResult,
    application::{auth::AuthContext, messaging::validate_device_id},
    domain::{Activity, Presence},
};

/// A renewed HTTP lease and the actor's current aggregate presence.
#[derive(Debug, Serialize)]
pub struct PresenceLease {
    /// Aggregate state across this actor's devices.
    pub presence: Presence,
    /// When another heartbeat is required to remain online.
    #[serde(with = "time::serde::rfc3339")]
    pub lease_expires_at: OffsetDateTime,
    /// Short-lived activity deadline, absent when cleared.
    #[serde(with = "time::serde::rfc3339::option")]
    pub activity_expires_at: Option<OffsetDateTime>,
}

impl PostgresStore {
    /// Closes expired HTTP leases and persists the last-seen transition only
    /// after the actor's final HTTP or legacy device is offline.
    ///
    /// # Errors
    /// Reports storage failures. Actor locks serialize expiry with renew/close.
    pub async fn expire_http_presence_leases(&self) -> AppResult<u64> {
        let mut transaction = self.pool().begin().await?;
        // All multi-actor presence operations acquire these hashes in the same
        // order. Re-check expiration after taking each lock because a heartbeat
        // may have renewed the lease since this initial candidate scan.
        let actors: Vec<(String,String,String,i64)> = sqlx::query_as(
            "SELECT DISTINCT organization_id,actor_kind::text,actor_id,hashtextextended(organization_id || chr(31) || actor_kind::text || chr(31) || actor_id,0) AS lock_key FROM client_presence_leases WHERE disconnected_at IS NULL AND lease_expires_at<=clock_timestamp() ORDER BY lock_key LIMIT 1000"
        ).fetch_all(&mut *transaction).await?;
        let mut expired = 0_u64;
        for (org, kind, actor, key) in actors {
            sqlx::query("SELECT pg_advisory_xact_lock($1)")
                .bind(key)
                .execute(&mut *transaction)
                .await?;
            let changed = sqlx::query(
                "UPDATE client_presence_leases SET disconnected_at=lease_expires_at,activity=NULL,activity_expires_at=NULL WHERE organization_id=$1 AND actor_kind=$2::text::actor_kind AND actor_id=$3 AND disconnected_at IS NULL AND lease_expires_at<=clock_timestamp()"
            ).bind(&org).bind(&kind).bind(&actor).execute(&mut *transaction).await?.rows_affected();
            if changed > 0 {
                record_actor_offline(&mut transaction, &org, &kind, &actor).await?;
                expired = expired.saturating_add(changed);
            }
        }
        transaction.commit().await?;
        Ok(expired)
    }

    /// Renews only the authenticated actor's named device; each call revalidates IAM.
    ///
    /// # Errors
    /// Rejects invalid device IDs or durations and reports storage failures.
    pub async fn renew_http_presence(
        &self,
        auth: &AuthContext,
        device: &str,
        activity: Option<Activity>,
        lease_duration: Duration,
        activity_duration: Duration,
    ) -> AppResult<PresenceLease> {
        validate_device_id(device)?;
        let lease_ms = duration_ms(lease_duration)?;
        let activity_ms = duration_ms(activity_duration)?.min(lease_ms);
        let mut transaction = self.pool().begin().await?;
        lock_actor(&mut transaction, auth).await?;
        refresh_directory_in(
            &mut transaction,
            &auth.organization_id,
            std::slice::from_ref(&auth.actor),
        )
        .await?;
        record_offline(&mut transaction, auth).await?;
        let activity = activity.map(|value| match value {
            Activity::Typing => "typing",
            Activity::RecordingVoice => "recording_voice",
            Activity::TranscribingVoice => "transcribing_voice",
            Activity::UploadingFile => "uploading_file",
            Activity::SearchingGifs => "searching_gifs",
        });
        let (lease_expires_at, activity_expires_at): (OffsetDateTime, Option<OffsetDateTime>) =
            sqlx::query_as(
                r#"
            WITH clock AS MATERIALIZED (SELECT clock_timestamp() AS now)
            INSERT INTO client_presence_leases AS previous (
                organization_id, actor_kind, actor_id, device_id,
                heartbeat_at, lease_expires_at, activity, activity_expires_at
            )
            SELECT $1, $2::text::actor_kind, $3, $4, clock.now,
                   clock.now + ($5::bigint * interval '1 millisecond'),
                   $6::text::presence_activity,
                   CASE WHEN $6 IS NULL THEN NULL
                        ELSE clock.now + ($7::bigint * interval '1 millisecond') END
            FROM clock
            ON CONFLICT (organization_id, actor_kind, actor_id, device_id)
            DO UPDATE SET
                heartbeat_at = EXCLUDED.heartbeat_at,
                lease_expires_at = EXCLUDED.lease_expires_at,
                activity = EXCLUDED.activity,
                activity_expires_at = EXCLUDED.activity_expires_at,
                disconnected_at = NULL,
                last_seen_at = GREATEST(previous.last_seen_at,
                    CASE WHEN previous.disconnected_at IS NOT NULL THEN previous.disconnected_at
                         WHEN previous.lease_expires_at <= EXCLUDED.heartbeat_at
                         THEN previous.lease_expires_at END)
            RETURNING lease_expires_at, activity_expires_at
            "#,
            )
            .bind(auth.organization_id.as_str())
            .bind(auth.actor.actor_type.as_str())
            .bind(auth.actor.id.as_str())
            .bind(device)
            .bind(lease_ms)
            .bind(activity)
            .bind(activity_ms)
            .fetch_one(&mut *transaction)
            .await?;
        transaction.commit().await?;
        Ok(PresenceLease {
            presence: self
                .get_presence(&auth.organization_id, &auth.actor, &auth.actor.id)
                .await?,
            lease_expires_at,
            activity_expires_at,
        })
    }

    /// Closes one actor-owned HTTP device lease without changing other devices.
    ///
    /// # Errors
    /// Rejects invalid device IDs and reports storage failures. Repeated close is safe.
    pub async fn close_http_presence(&self, auth: &AuthContext, device: &str) -> AppResult<()> {
        validate_device_id(device)?;
        let mut transaction = self.pool().begin().await?;
        lock_actor(&mut transaction, auth).await?;
        sqlx::query(
            r#"
            UPDATE client_presence_leases
            SET disconnected_at = LEAST(lease_expires_at, clock_timestamp()),
                activity = NULL, activity_expires_at = NULL
            WHERE organization_id = $1 AND actor_kind = $2::text::actor_kind
              AND actor_id = $3 AND device_id = $4 AND disconnected_at IS NULL
            "#,
        )
        .bind(auth.organization_id.as_str())
        .bind(auth.actor.actor_type.as_str())
        .bind(auth.actor.id.as_str())
        .bind(device)
        .execute(&mut *transaction)
        .await?;
        record_offline(&mut transaction, auth).await?;
        transaction.commit().await?;
        Ok(())
    }
}

async fn lock_actor(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    auth: &AuthContext,
) -> AppResult<()> {
    // Use the same actor lock as legacy lease expiration during cutover.
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended($1 || chr(31) || $2 || chr(31) || $3, 0))",
    )
    .bind(auth.organization_id.as_str())
    .bind(auth.actor.actor_type.as_str())
    .bind(auth.actor.id.as_str())
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

async fn record_offline(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    auth: &AuthContext,
) -> AppResult<()> {
    record_actor_offline(
        transaction,
        auth.organization_id.as_str(),
        auth.actor.actor_type.as_str(),
        auth.actor.id.as_str(),
    )
    .await
}

async fn record_actor_offline(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    org: &str,
    kind: &str,
    actor: &str,
) -> AppResult<()> {
    sqlx::query(
        r#"
        WITH devices AS MATERIALIZED (
            SELECT lease_expires_at, disconnected_at
            FROM client_presence_leases
            WHERE organization_id=$1 AND actor_kind=$2::text::actor_kind AND actor_id=$3
            UNION ALL
            SELECT session.lease_expires_at, session.disconnected_at
            FROM realtime_session_actors actor
            JOIN realtime_sessions session ON session.id=actor.session_id
            WHERE actor.organization_id=$1 AND actor.actor_kind=$2::text::actor_kind AND actor.actor_id=$3
        ), offline AS (
            SELECT max(COALESCE(disconnected_at, lease_expires_at)) AS seen
            FROM devices
            WHERE NOT EXISTS (
                SELECT 1 FROM devices
                WHERE disconnected_at IS NULL AND lease_expires_at>clock_timestamp()
            )
        )
        INSERT INTO actor_presence_state(organization_id,actor_kind,actor_id,last_seen_at,created_at)
        SELECT $1,$2::text::actor_kind,$3,seen,seen FROM offline WHERE seen IS NOT NULL
        ON CONFLICT (organization_id,actor_kind,actor_id)
        DO UPDATE SET last_seen_at=GREATEST(actor_presence_state.last_seen_at,EXCLUDED.last_seen_at)
        "#,
    )
    .bind(org)
    .bind(kind)
    .bind(actor)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

fn duration_ms(duration: Duration) -> AppResult<i64> {
    let value = i64::try_from(duration.as_millis())
        .map_err(|_| AppError::validation("presence duration is too large"))?;
    if value <= 0 {
        return Err(AppError::validation("presence duration must be positive"));
    }
    Ok(value)
}
