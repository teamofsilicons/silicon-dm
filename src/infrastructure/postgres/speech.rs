//! Chosen voices, the speech audio cache, durable speech renders and speech usage meters.
//!
//! `speech_audio` is a derived cache, never an upload: every row is a rendering DM can make
//! again from a message's script and pinned voice, so the worker evicts rows by size and age
//! without losing anything, and no uploaded file ever lands there.

use std::{collections::HashMap, time::Duration};

use sqlx::{FromRow, Postgres, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use super::{
    PostgresStore,
    idempotency::{IdempotencyClaim, IdempotencyResult, claim, complete, request_hash},
    map_constraint_error,
};
use crate::{
    AppError, AppResult,
    domain::{ActorRef, IdempotencyKey, OrganizationId, audio::PEAK_COUNT},
};

/// Postgres channel notified when a render is queued.
pub(crate) const SPEECH_NOTIFICATION_CHANNEL: &str = "dm_speech";
/// How many renders are attempted before a spoken message is marked failed.
pub const MAX_RENDER_ATTEMPTS: i32 = 5;

/// A voice chosen for an account.
#[derive(Clone, Debug, FromRow)]
pub struct ChosenVoice {
    /// Catalog key.
    pub voice_key: String,
    /// Who chose it: the account itself or a Silicon's custodian.
    pub chosen_by_uuid: String,
    /// When.
    pub updated_at: OffsetDateTime,
}

/// What DM knows about one cached rendering, without its audio.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioMeta {
    /// Changes whenever the cached bytes change (a re-render after eviction).
    pub etag: String,
    /// Length of the audio.
    pub duration_milliseconds: u32,
    /// 64 loudness values, 0 to 100.
    pub peaks: Vec<i16>,
    /// Size of the WAV file.
    pub bytes: u64,
}

#[derive(FromRow)]
struct AudioMetaRecord {
    etag: String,
    duration_ms: i32,
    peaks: Vec<i16>,
    bytes: i64,
    last_used_at: OffsetDateTime,
}

impl AudioMetaRecord {
    fn meta(self) -> AppResult<AudioMeta> {
        Ok(AudioMeta {
            etag: self.etag,
            duration_milliseconds: u32::try_from(self.duration_ms).map_err(AppError::internal)?,
            peaks: self.peaks,
            bytes: u64::try_from(self.bytes).map_err(AppError::internal)?,
        })
    }
}

/// Where a spoken message's rendering stands.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RenderStatus {
    /// Queued or rendering.
    Pending,
    /// Rendered: duration and peaks are known (the audio itself may have been evicted).
    Ready,
    /// Every attempt failed; listening still tries once more on demand.
    Failed,
}

impl RenderStatus {
    /// Wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Ready => "ready",
            Self::Failed => "failed",
        }
    }

    fn parse(value: &str) -> AppResult<Self> {
        match value {
            "pending" => Ok(Self::Pending),
            "ready" => Ok(Self::Ready),
            "failed" => Ok(Self::Failed),
            other => Err(super::rows::data_error("speech render status", other)),
        }
    }
}

/// The rendering of one spoken message version, as clients see it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderState {
    /// Where it stands.
    pub status: RenderStatus,
    /// Length, once ready.
    pub duration_milliseconds: Option<u32>,
    /// Waveform, once ready.
    pub peaks: Option<Vec<i16>>,
}

/// A spoken message version to render, queued with the message.
#[derive(Clone, Debug)]
pub struct SpeechRenderJob {
    /// `sha256(model|voice_id|script)`.
    pub cache_key: [u8; 32],
    /// Speech model.
    pub model: String,
    /// Provider voice id.
    pub voice_id: String,
    /// The script.
    pub script: String,
    /// Already in the cache: the job starts ready and nothing is rendered or charged.
    pub cached: Option<AudioMeta>,
    /// What the sender is charged when the job is not already cached.
    pub charge: SpeechCharge,
}

/// One render's cost against an account's limits.
#[derive(Clone, Copy, Debug)]
pub struct SpeechCharge {
    /// Characters rendered (the script, cues included).
    pub characters: u64,
    /// Renders allowed per minute.
    pub renders_per_minute: u32,
    /// Characters allowed per UTC day.
    pub daily_characters: u64,
}

/// A leased render for the worker.
#[derive(Clone, Debug)]
pub struct RenderClaim {
    /// Message.
    pub message_id: Uuid,
    /// Cache key.
    pub cache_key: [u8; 32],
    /// Speech model.
    pub model: String,
    /// Provider voice id.
    pub voice_id: String,
    /// Script.
    pub script: String,
    /// Attempts before this one.
    pub attempts: i32,
    /// This claim's lease.
    pub lease_id: Uuid,
}

#[derive(FromRow)]
struct RenderJobRecord {
    message_id: Uuid,
    cache_key: Vec<u8>,
    status: String,
    duration_ms: Option<i32>,
    peaks: Option<Vec<i16>>,
}

#[derive(FromRow)]
struct RenderClaimRecord {
    message_id: Uuid,
    cache_key: Vec<u8>,
    model: String,
    voice_id: String,
    script: String,
    attempts: i32,
}

/// How a leased render ended.
pub enum RenderOutcome {
    /// Rendered and cached.
    Ready(AudioMeta),
    /// Failed; retry at this time.
    Retry {
        /// Stable failure code.
        error: &'static str,
        /// When to try again.
        next_attempt_at: OffsetDateTime,
    },
    /// Failed for good.
    Failed {
        /// Stable failure code.
        error: &'static str,
    },
}

/// What a listener got when it asked for a spoken message's rendering lease.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SpeechLease {
    /// The lease is the listener's: it renders now, and the worker leaves it alone.
    Taken(Uuid),
    /// Someone renders it now (a worker in some process, or another listener).
    Busy,
    /// The message version has no rendering record (the listener renders it without one).
    NoJob,
}

/// A charge that went through: what to give back if the render then fails.
#[derive(Clone, Copy, Debug)]
pub struct SpeechCharged {
    /// Characters charged.
    pub characters: u64,
    /// The UTC day they were charged to.
    pub day: OffsetDateTime,
}

/// Which limit a charge exceeded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LimitExceeded {
    /// Renders per minute.
    RendersPerMinute,
    /// Characters per day.
    DailyCharacters,
    /// Transcriptions per minute.
    TranscriptionsPerMinute,
}

impl PostgresStore {
    /// The voice chosen for an account, if any.
    ///
    /// # Errors
    /// Database failures.
    pub async fn chosen_voice(&self, account_uuid: &str) -> AppResult<Option<ChosenVoice>> {
        Ok(sqlx::query_as::<_, ChosenVoice>(
            "SELECT voice_key, chosen_by_uuid, updated_at FROM account_voices WHERE account_uuid = $1",
        )
        .bind(account_uuid)
        .fetch_optional(self.pool())
        .await?)
    }

    /// Chooses a voice for an account.
    ///
    /// # Errors
    /// Database failures.
    pub async fn set_voice(
        &self,
        account_uuid: &str,
        voice_key: &str,
        chosen_by_uuid: &str,
    ) -> AppResult<ChosenVoice> {
        sqlx::query_as::<_, ChosenVoice>(
            "INSERT INTO account_voices (account_uuid, voice_key, chosen_by_uuid)
             VALUES ($1, $2, $3)
             ON CONFLICT (account_uuid) DO UPDATE
                SET voice_key = EXCLUDED.voice_key, chosen_by_uuid = EXCLUDED.chosen_by_uuid,
                    updated_at = clock_timestamp()
             RETURNING voice_key, chosen_by_uuid, updated_at",
        )
        .bind(account_uuid)
        .bind(voice_key)
        .bind(chosen_by_uuid)
        .fetch_one(self.pool())
        .await
        .map_err(map_constraint_error)
    }

    /// Forgets an account's chosen voice (it speaks in its default voice again).
    ///
    /// # Errors
    /// Database failures.
    pub async fn reset_voice(&self, account_uuid: &str) -> AppResult<()> {
        sqlx::query("DELETE FROM account_voices WHERE account_uuid = $1")
            .bind(account_uuid)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    /// Chooses (`Some`) or resets (`None`) an account's voice for `chooser`. With an
    /// idempotency key, a retry with the same key and body changes nothing again and a
    /// different body under the same key is 409.
    ///
    /// # Errors
    /// `conflict` for a reused key; database failures.
    pub async fn change_voice(
        &self,
        account_uuid: &str,
        voice_key: Option<&str>,
        chooser: &ActorRef,
        idempotency_key: Option<&IdempotencyKey>,
    ) -> AppResult<()> {
        let mut tx = self.pool().begin().await?;
        let operation = if voice_key.is_some() {
            "voices.set"
        } else {
            "voices.reset"
        };
        let accounts = OrganizationId::accounts();
        if let Some(key) = idempotency_key {
            super::directory::refresh_directory_in(
                &mut tx,
                &accounts,
                std::slice::from_ref(chooser),
            )
            .await?;
            let hash = request_hash(&(account_uuid, voice_key))?;
            if let IdempotencyClaim::Replay(_) =
                claim(&mut tx, &accounts, chooser, operation, key, &hash).await?
            {
                return tx.commit().await.map_err(map_constraint_error);
            }
        }
        match voice_key {
            Some(voice_key) => {
                sqlx::query(
                    "INSERT INTO account_voices (account_uuid, voice_key, chosen_by_uuid)
                     VALUES ($1, $2, $3)
                     ON CONFLICT (account_uuid) DO UPDATE
                        SET voice_key = EXCLUDED.voice_key, chosen_by_uuid = EXCLUDED.chosen_by_uuid,
                            updated_at = clock_timestamp()",
                )
                .bind(account_uuid)
                .bind(voice_key)
                .bind(chooser.id.as_str())
                .execute(&mut *tx)
                .await
                .map_err(map_constraint_error)?;
            }
            None => {
                sqlx::query("DELETE FROM account_voices WHERE account_uuid = $1")
                    .bind(account_uuid)
                    .execute(&mut *tx)
                    .await?;
            }
        }
        if let Some(key) = idempotency_key {
            complete(
                &mut tx,
                &accounts,
                chooser,
                operation,
                key,
                IdempotencyResult {
                    resource_type: "voice",
                    resource_id: Uuid::nil(),
                    response_status: 200,
                },
            )
            .await?;
        }
        tx.commit().await.map_err(map_constraint_error)
    }

    /// The cached rendering for a key, without its audio. A hit is recorded as a use (at most
    /// once an hour per key, so popular audio does not turn every read into a write).
    ///
    /// # Errors
    /// Database failures.
    pub async fn speech_audio_meta(&self, cache_key: &[u8; 32]) -> AppResult<Option<AudioMeta>> {
        let Some(record) = sqlx::query_as::<_, AudioMetaRecord>(
            "SELECT etag, duration_ms, peaks, bytes, last_used_at FROM speech_audio WHERE cache_key = $1",
        )
        .bind(cache_key.as_slice())
        .fetch_optional(self.pool())
        .await?
        else {
            return Ok(None);
        };
        if record.last_used_at < OffsetDateTime::now_utc() - time::Duration::hours(1) {
            sqlx::query(
                "UPDATE speech_audio SET last_used_at = clock_timestamp() WHERE cache_key = $1",
            )
            .bind(cache_key.as_slice())
            .execute(self.pool())
            .await?;
        }
        record.meta().map(Some)
    }

    /// `length` bytes of the cached WAV `etag` from `start` (only those bytes are read; the
    /// column is stored uncompressed). `None` when the rendering was evicted or replaced, so
    /// one response never mixes two renderings.
    ///
    /// # Errors
    /// Database failures.
    pub async fn speech_audio_window(
        &self,
        cache_key: &[u8; 32],
        etag: &str,
        start: u64,
        length: u64,
    ) -> AppResult<Option<Vec<u8>>> {
        let start = i64::try_from(start).map_err(AppError::internal)?;
        let length = i64::try_from(length).map_err(AppError::internal)?;
        Ok(sqlx::query_scalar::<_, Vec<u8>>(
            "SELECT substring(wav FROM $3::bigint::integer + 1 FOR $4::bigint::integer) FROM speech_audio WHERE cache_key = $1 AND etag = $2",
        )
        .bind(cache_key.as_slice())
        .bind(etag)
        .bind(start)
        .bind(length)
        .fetch_optional(self.pool())
        .await?)
    }

    /// Caches a rendering. When another render of the same key won the race, its row is kept
    /// and returned, so every reader of a key sees one audio.
    ///
    /// # Errors
    /// Database failures.
    pub async fn store_speech_audio(
        &self,
        cache_key: &[u8; 32],
        wav: &[u8],
        duration_milliseconds: u32,
        peaks: &[i16],
    ) -> AppResult<AudioMeta> {
        if peaks.len() != PEAK_COUNT {
            return Err(AppError::internal(anyhow::anyhow!(
                "a rendering needs 64 peaks"
            )));
        }
        let etag = blake3::hash(wav).to_hex()[..32].to_owned();
        let bytes = i64::try_from(wav.len()).map_err(AppError::internal)?;
        let duration = i32::try_from(duration_milliseconds).map_err(AppError::internal)?;
        sqlx::query(
            "INSERT INTO speech_audio (cache_key, wav, etag, duration_ms, peaks, bytes)
             VALUES ($1, $2, $3, $4, $5, $6)
             ON CONFLICT (cache_key) DO NOTHING",
        )
        .bind(cache_key.as_slice())
        .bind(wav)
        .bind(&etag)
        .bind(duration)
        .bind(peaks)
        .bind(bytes)
        .execute(self.pool())
        .await
        .map_err(map_constraint_error)?;
        self.speech_audio_meta(cache_key)
            .await?
            .ok_or_else(|| AppError::internal(anyhow::anyhow!("cached speech vanished")))
    }

    /// Evicts rendered audio unused for `ttl`, then the least recently used beyond
    /// `max_bytes`. Returns how many renderings were evicted.
    ///
    /// # Errors
    /// Database failures.
    pub async fn evict_speech_audio(&self, max_bytes: u64, ttl: Duration) -> AppResult<u64> {
        let ttl_seconds = i64::try_from(ttl.as_secs()).unwrap_or(i64::MAX);
        let max_bytes = i64::try_from(max_bytes).unwrap_or(i64::MAX);
        let expired = sqlx::query(
            "DELETE FROM speech_audio WHERE last_used_at < clock_timestamp() - make_interval(secs => $1::double precision)",
        )
        .bind(ttl_seconds)
        .execute(self.pool())
        .await?
        .rows_affected();
        let over_budget = sqlx::query(
            "DELETE FROM speech_audio WHERE cache_key IN (
                SELECT cache_key FROM (
                    SELECT cache_key,
                           sum(bytes) OVER (ORDER BY last_used_at DESC, cache_key) AS kept
                    FROM speech_audio
                ) ranked
                WHERE kept > $1
             )",
        )
        .bind(max_bytes)
        .execute(self.pool())
        .await?
        .rows_affected();
        Ok(expired.saturating_add(over_budget))
    }

    /// Where the renderings of spoken message versions stand, for a whole response at once:
    /// each version's render job, or (for a version that has none) the cache.
    ///
    /// # Errors
    /// Database failures.
    pub async fn speech_render_states(
        &self,
        versions: &[(Uuid, [u8; 32])],
    ) -> AppResult<HashMap<(Uuid, [u8; 32]), RenderState>> {
        let mut states = HashMap::new();
        if versions.is_empty() {
            return Ok(states);
        }
        let messages: Vec<Uuid> = versions.iter().map(|(message, _)| *message).collect();
        let keys: Vec<Vec<u8>> = versions.iter().map(|(_, key)| key.to_vec()).collect();
        let jobs: Vec<RenderJobRecord> = sqlx::query_as(
            "SELECT r.message_id, r.cache_key, r.status, r.duration_ms, r.peaks
             FROM speech_renders r
             JOIN unnest($1::uuid[], $2::bytea[]) AS version(message_id, cache_key)
               ON r.message_id = version.message_id AND r.cache_key = version.cache_key",
        )
        .bind(&messages)
        .bind(&keys)
        .fetch_all(self.pool())
        .await?;
        for RenderJobRecord {
            message_id: message,
            cache_key: key,
            status,
            duration_ms: duration,
            peaks,
        } in jobs
        {
            let key: [u8; 32] = key
                .try_into()
                .map_err(|_| super::rows::data_error("speech cache key", "not 32 bytes"))?;
            states.insert(
                (message, key),
                RenderState {
                    status: RenderStatus::parse(&status)?,
                    duration_milliseconds: duration
                        .map(u32::try_from)
                        .transpose()
                        .map_err(AppError::internal)?,
                    peaks,
                },
            );
        }
        let unrecorded: Vec<Vec<u8>> = versions
            .iter()
            .filter(|version| !states.contains_key(*version))
            .map(|(_, key)| key.to_vec())
            .collect();
        let cached: HashMap<Vec<u8>, (i32, Vec<i16>)> = if unrecorded.is_empty() {
            HashMap::new()
        } else {
            sqlx::query_as::<_, (Vec<u8>, i32, Vec<i16>)>(
                "SELECT cache_key, duration_ms, peaks FROM speech_audio WHERE cache_key = ANY($1)",
            )
            .bind(&unrecorded)
            .fetch_all(self.pool())
            .await?
            .into_iter()
            .map(|(key, duration, peaks)| (key, (duration, peaks)))
            .collect()
        };
        for version in versions {
            if states.contains_key(version) {
                continue;
            }
            let state = match cached.get(version.1.as_slice()) {
                Some((duration, peaks)) => RenderState {
                    status: RenderStatus::Ready,
                    duration_milliseconds: Some(
                        u32::try_from(*duration).map_err(AppError::internal)?,
                    ),
                    peaks: Some(peaks.clone()),
                },
                None => RenderState {
                    status: RenderStatus::Pending,
                    duration_milliseconds: None,
                    peaks: None,
                },
            };
            states.insert(*version, state);
        }
        Ok(states)
    }

    /// The status of one spoken message version's render job, if it has one.
    ///
    /// # Errors
    /// Database failures.
    pub async fn speech_render_status(
        &self,
        message_id: Uuid,
        cache_key: &[u8; 32],
    ) -> AppResult<Option<RenderStatus>> {
        sqlx::query_scalar::<_, String>(
            "SELECT status FROM speech_renders WHERE message_id = $1 AND cache_key = $2",
        )
        .bind(message_id)
        .bind(cache_key.as_slice())
        .fetch_optional(self.pool())
        .await?
        .as_deref()
        .map(RenderStatus::parse)
        .transpose()
    }

    /// A listener asks for the lease on a spoken message version's rendering, so that it alone
    /// renders it now: the worker (in any process) and other listeners wait for it instead of
    /// rendering the same speech a second time. With `leave_due_to_worker`, a render that is
    /// due now is left to the worker that is about to take it (the listener waits for it).
    ///
    /// # Errors
    /// Database failures.
    pub async fn lease_speech_render(
        &self,
        message_id: Uuid,
        cache_key: &[u8; 32],
        lease: Duration,
        leave_due_to_worker: bool,
    ) -> AppResult<SpeechLease> {
        let lease_id = Uuid::now_v7();
        let lease_milliseconds = i64::try_from(lease.as_millis()).unwrap_or(i64::MAX);
        let taken: Option<Uuid> = sqlx::query_scalar(
            "UPDATE speech_renders SET lease_id = $3,
                lease_expires_at = clock_timestamp() + ($4::bigint * interval '1 millisecond')
             WHERE message_id = $1 AND cache_key = $2
               AND (lease_expires_at IS NULL OR lease_expires_at <= clock_timestamp())
               AND NOT ($5 AND status = 'pending' AND next_attempt_at <= clock_timestamp())
             RETURNING lease_id",
        )
        .bind(message_id)
        .bind(cache_key.as_slice())
        .bind(lease_id)
        .bind(lease_milliseconds)
        .bind(leave_due_to_worker)
        .fetch_optional(self.pool())
        .await
        .map_err(map_constraint_error)?;
        if let Some(lease_id) = taken {
            return Ok(SpeechLease::Taken(lease_id));
        }
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM speech_renders WHERE message_id = $1 AND cache_key = $2)",
        )
        .bind(message_id)
        .bind(cache_key.as_slice())
        .fetch_one(self.pool())
        .await?;
        Ok(if exists {
            SpeechLease::Busy
        } else {
            SpeechLease::NoJob
        })
    }

    /// Gives back a listener's lease (whatever its render did).
    ///
    /// # Errors
    /// Database failures.
    pub async fn release_speech_lease(
        &self,
        message_id: Uuid,
        cache_key: &[u8; 32],
        lease_id: Uuid,
    ) -> AppResult<()> {
        sqlx::query(
            "UPDATE speech_renders SET lease_id = NULL, lease_expires_at = NULL
             WHERE message_id = $1 AND cache_key = $2 AND lease_id = $3",
        )
        .bind(message_id)
        .bind(cache_key.as_slice())
        .bind(lease_id)
        .execute(self.pool())
        .await
        .map_err(map_constraint_error)?;
        Ok(())
    }

    /// Whether a spoken message version's pre-render is under way: pending and either leased
    /// by a worker now or due now (a worker will take it at once). A pending render waiting out
    /// a retry delay is not under way.
    ///
    /// # Errors
    /// Database failures.
    pub async fn speech_render_under_way(
        &self,
        message_id: Uuid,
        cache_key: &[u8; 32],
    ) -> AppResult<bool> {
        Ok(sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM speech_renders
                WHERE message_id = $1 AND cache_key = $2 AND status = 'pending'
                  AND (lease_expires_at > clock_timestamp() OR next_attempt_at <= clock_timestamp()))",
        )
        .bind(message_id)
        .bind(cache_key.as_slice())
        .fetch_one(self.pool())
        .await?)
    }

    /// Leases renders that are due.
    ///
    /// # Errors
    /// Database failures.
    pub async fn claim_speech_renders(
        &self,
        limit: i64,
        lease: Duration,
    ) -> AppResult<Vec<RenderClaim>> {
        let lease_id = Uuid::now_v7();
        let lease_milliseconds = i64::try_from(lease.as_millis()).unwrap_or(i64::MAX);
        let records = sqlx::query_as::<_, RenderClaimRecord>(
            "WITH due AS (
                SELECT message_id, cache_key FROM speech_renders
                WHERE status = 'pending' AND next_attempt_at <= clock_timestamp()
                  AND (lease_expires_at IS NULL OR lease_expires_at <= clock_timestamp())
                ORDER BY next_attempt_at, created_at
                LIMIT $1
                FOR UPDATE SKIP LOCKED
             )
             UPDATE speech_renders r
             SET lease_id = $2,
                 lease_expires_at = clock_timestamp() + ($3::bigint * interval '1 millisecond')
             FROM due
             WHERE r.message_id = due.message_id AND r.cache_key = due.cache_key
             RETURNING r.message_id, r.cache_key, r.model, r.voice_id, r.script, r.attempts",
        )
        .bind(limit)
        .bind(lease_id)
        .bind(lease_milliseconds)
        .fetch_all(self.pool())
        .await
        .map_err(map_constraint_error)?;
        records
            .into_iter()
            .map(|record| {
                Ok(RenderClaim {
                    message_id: record.message_id,
                    cache_key: record
                        .cache_key
                        .try_into()
                        .map_err(|_| super::rows::data_error("speech cache key", "not 32 bytes"))?,
                    model: record.model,
                    voice_id: record.voice_id,
                    script: record.script,
                    attempts: record.attempts,
                    lease_id,
                })
            })
            .collect()
    }

    /// Whether a message was deleted (its speech is never rendered then).
    ///
    /// # Errors
    /// Database failures.
    pub async fn message_deleted(&self, message_id: Uuid) -> AppResult<bool> {
        Ok(sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM message_history WHERE message_id = $1 AND deleted_at IS NOT NULL)",
        )
        .bind(message_id)
        .fetch_one(self.pool())
        .await?)
    }

    /// Records how a leased render ended. Reaching `ready` or `failed` announces the message
    /// to its conversation (a `message` sync event and Ting handoff), so clients refresh it.
    /// A render the playback path completed first is left as it is.
    ///
    /// # Errors
    /// Database failures.
    pub async fn finish_speech_render(
        &self,
        claim: &RenderClaim,
        outcome: RenderOutcome,
    ) -> AppResult<()> {
        let mut tx = self.pool().begin().await?;
        lock_message_in(&mut tx, claim.message_id).await?;
        let changed = match outcome {
            RenderOutcome::Ready(meta) => {
                mark_ready_in(&mut tx, claim.message_id, &claim.cache_key, &meta, Some(claim.lease_id))
                    .await?
            }
            RenderOutcome::Retry {
                error,
                next_attempt_at,
            } => {
                sqlx::query(
                    "UPDATE speech_renders SET attempts = attempts + 1, next_attempt_at = $4,
                        last_error = $3, lease_id = NULL, lease_expires_at = NULL
                     WHERE message_id = $1 AND cache_key = $2 AND lease_id = $5 AND status = 'pending'",
                )
                .bind(claim.message_id)
                .bind(claim.cache_key.as_slice())
                .bind(error)
                .bind(next_attempt_at)
                .bind(claim.lease_id)
                .execute(&mut *tx)
                .await
                .map_err(map_constraint_error)?;
                false
            }
            RenderOutcome::Failed { error } => {
                sqlx::query(
                    "UPDATE speech_renders SET attempts = attempts + 1, status = 'failed',
                        last_error = $3, completed_at = clock_timestamp(),
                        lease_id = NULL, lease_expires_at = NULL
                     WHERE message_id = $1 AND cache_key = $2 AND lease_id = $4 AND status = 'pending'",
                )
                .bind(claim.message_id)
                .bind(claim.cache_key.as_slice())
                .bind(error)
                .bind(claim.lease_id)
                .execute(&mut *tx)
                .await
                .map_err(map_constraint_error)?
                .rows_affected()
                    == 1
            }
        };
        if changed {
            announce_message_update_in(&mut tx, claim.message_id).await?;
        }
        tx.commit().await.map_err(map_constraint_error)
    }

    /// Marks a spoken message version ready after listening rendered it on demand (a pending
    /// or failed pre-render), announcing the change. Does nothing when already ready.
    ///
    /// # Errors
    /// Database failures.
    pub async fn complete_speech_render(
        &self,
        message_id: Uuid,
        cache_key: &[u8; 32],
        meta: &AudioMeta,
    ) -> AppResult<()> {
        let mut tx = self.pool().begin().await?;
        lock_message_in(&mut tx, message_id).await?;
        if mark_ready_in(&mut tx, message_id, cache_key, meta, None).await? {
            announce_message_update_in(&mut tx, message_id).await?;
        }
        tx.commit().await.map_err(map_constraint_error)
    }

    /// Charges a render outside a transaction (listening and `POST /speech`).
    ///
    /// # Errors
    /// `Ok(Err(limit))` when a limit is reached (nothing is charged); database failures.
    pub async fn charge_speech(
        &self,
        account: &ActorRef,
        charge: SpeechCharge,
    ) -> AppResult<Result<SpeechCharged, LimitExceeded>> {
        let mut tx = self.pool().begin().await?;
        let outcome = charge_speech_in(&mut tx, account, charge).await?;
        if outcome.is_ok() {
            tx.commit().await.map_err(map_constraint_error)?;
        }
        Ok(outcome)
    }

    /// Gives an account back the characters of a render the provider failed: the day's
    /// budget counts rendered characters, and nothing was rendered.
    ///
    /// # Errors
    /// Database failures.
    pub async fn refund_speech_characters(
        &self,
        account: &ActorRef,
        charged: &SpeechCharged,
    ) -> AppResult<()> {
        let characters = i64::try_from(charged.characters).map_err(AppError::internal)?;
        sqlx::query(
            "UPDATE speech_usage SET amount = GREATEST(amount - $3, 0)
             WHERE account_uuid = $1 AND meter = 'characters' AND window_start = $2",
        )
        .bind(account.id.as_str())
        .bind(charged.day)
        .bind(characters)
        .execute(self.pool())
        .await
        .map_err(map_constraint_error)?;
        Ok(())
    }

    /// Charges one transcription.
    ///
    /// # Errors
    /// `Ok(Err(limit))` when the per-minute limit is reached; database failures.
    pub async fn charge_transcription(
        &self,
        account: &ActorRef,
        per_minute: u32,
    ) -> AppResult<Result<(), LimitExceeded>> {
        let mut tx = self.pool().begin().await?;
        let within = meter_in(
            &mut tx,
            account,
            "transcriptions",
            Window::Minute,
            1,
            u64::from(per_minute),
        )
        .await?;
        if within.is_none() {
            return Ok(Err(LimitExceeded::TranscriptionsPerMinute));
        }
        tx.commit().await.map_err(map_constraint_error)?;
        Ok(Ok(()))
    }

    /// Subscribes to render wakeups on a connection of its own (polling stays
    /// authoritative).
    ///
    /// # Errors
    /// Listener failures.
    pub async fn subscribe_speech_wakeups(&self) -> AppResult<Option<sqlx::postgres::PgListener>> {
        self.dedicated_listener(SPEECH_NOTIFICATION_CHANNEL).await
    }

    /// Removes usage windows that no limit looks at any more.
    ///
    /// # Errors
    /// Database failures.
    pub async fn prune_speech_usage(&self) -> AppResult<u64> {
        Ok(sqlx::query(
            "DELETE FROM speech_usage WHERE window_start < clock_timestamp() - interval '2 days'",
        )
        .execute(self.pool())
        .await?
        .rows_affected())
    }
}

#[derive(Clone, Copy)]
enum Window {
    Minute,
    Day,
}

/// Charges a render inside the caller's transaction: one render this minute and the script's
/// characters today. Either both are charged or neither is.
pub(crate) async fn charge_speech_in(
    tx: &mut Transaction<'_, Postgres>,
    account: &ActorRef,
    charge: SpeechCharge,
) -> AppResult<Result<SpeechCharged, LimitExceeded>> {
    sqlx::query("SAVEPOINT speech_charge")
        .execute(&mut **tx)
        .await?;
    if meter_in(
        tx,
        account,
        "renders",
        Window::Minute,
        1,
        u64::from(charge.renders_per_minute),
    )
    .await?
    .is_none()
    {
        sqlx::query("ROLLBACK TO SAVEPOINT speech_charge")
            .execute(&mut **tx)
            .await?;
        return Ok(Err(LimitExceeded::RendersPerMinute));
    }
    let Some(day) = meter_in(
        tx,
        account,
        "characters",
        Window::Day,
        charge.characters,
        charge.daily_characters,
    )
    .await?
    else {
        sqlx::query("ROLLBACK TO SAVEPOINT speech_charge")
            .execute(&mut **tx)
            .await?;
        return Ok(Err(LimitExceeded::DailyCharacters));
    };
    sqlx::query("RELEASE SAVEPOINT speech_charge")
        .execute(&mut **tx)
        .await?;
    Ok(Ok(SpeechCharged {
        characters: charge.characters,
        day,
    }))
}

/// Adds `amount` to a meter's current window when that stays within `limit`; returns the
/// window charged, or `None` (nothing charged) when it would not.
async fn meter_in(
    tx: &mut Transaction<'_, Postgres>,
    account: &ActorRef,
    meter: &str,
    window: Window,
    amount: u64,
    limit: u64,
) -> AppResult<Option<OffsetDateTime>> {
    let amount = i64::try_from(amount).map_err(AppError::internal)?;
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    let window = match window {
        Window::Minute => "minute",
        Window::Day => "day",
    };
    let charged: Option<OffsetDateTime> = sqlx::query_scalar(
        "INSERT INTO speech_usage (account_uuid, meter, window_start, amount)
         SELECT $1, $2, date_trunc($3, clock_timestamp(), 'UTC'), $4
         WHERE $4 <= $5
         ON CONFLICT (account_uuid, meter, window_start) DO UPDATE
            SET amount = speech_usage.amount + EXCLUDED.amount
            WHERE speech_usage.amount + EXCLUDED.amount <= $5
         RETURNING window_start",
    )
    .bind(account.id.as_str())
    .bind(meter)
    .bind(window)
    .bind(amount)
    .bind(limit)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_constraint_error)?;
    Ok(charged)
}

/// Seconds until the window of `limit` resets.
#[must_use]
pub fn retry_after_seconds(limit: LimitExceeded) -> u64 {
    let now = OffsetDateTime::now_utc();
    match limit {
        LimitExceeded::RendersPerMinute | LimitExceeded::TranscriptionsPerMinute => {
            u64::from(60 - u16::from(now.second())).max(1)
        }
        LimitExceeded::DailyCharacters => {
            let seconds_today = u64::from(now.hour()) * 3_600
                + u64::from(now.minute()) * 60
                + u64::from(now.second());
            (86_400 - seconds_today).max(1)
        }
    }
}

/// Queues the rendering of a spoken message version in the transaction that accepts it:
/// ready at once when the audio is already cached, else charged to the sender and pending. A
/// version the message already has a rendering for (an edit that only changed the caption) is
/// never queued or charged twice.
///
/// # Errors
/// `Ok(Err(limit))` when the sender is over a speech limit (the caller rolls back);
/// database failures.
pub(crate) async fn queue_speech_render_in(
    tx: &mut Transaction<'_, Postgres>,
    message_id: Uuid,
    sender: &ActorRef,
    job: &SpeechRenderJob,
) -> AppResult<Result<(), LimitExceeded>> {
    // The caller holds the message lock; take the rendering's row lock after it, in the same
    // order as the worker, so the two never wait on each other.
    let existing: Option<String> = sqlx::query_scalar(
        "SELECT status FROM speech_renders WHERE message_id = $1 AND cache_key = $2 FOR UPDATE",
    )
    .bind(message_id)
    .bind(job.cache_key.as_slice())
    .fetch_optional(&mut **tx)
    .await?;
    if let Some(meta) = &job.cached {
        sqlx::query(
            "INSERT INTO speech_renders (message_id, cache_key, model, voice_id, script, status,
                duration_ms, peaks, completed_at)
             VALUES ($1, $2, $3, $4, $5, 'ready', $6, $7, clock_timestamp())
             ON CONFLICT (message_id, cache_key) DO UPDATE
                SET status = 'ready', duration_ms = EXCLUDED.duration_ms, peaks = EXCLUDED.peaks,
                    completed_at = EXCLUDED.completed_at, last_error = NULL,
                    lease_id = NULL, lease_expires_at = NULL
                WHERE speech_renders.status <> 'ready'",
        )
        .bind(message_id)
        .bind(job.cache_key.as_slice())
        .bind(&job.model)
        .bind(&job.voice_id)
        .bind(&job.script)
        .bind(i32::try_from(meta.duration_milliseconds).map_err(AppError::internal)?)
        .bind(&meta.peaks)
        .execute(&mut **tx)
        .await
        .map_err(map_constraint_error)?;
        return Ok(Ok(()));
    }
    if existing.is_some() {
        return Ok(Ok(()));
    }
    if let Err(limit) = charge_speech_in(tx, sender, job.charge).await? {
        return Ok(Err(limit));
    }
    sqlx::query(
        "INSERT INTO speech_renders (message_id, cache_key, model, voice_id, script)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (message_id, cache_key) DO NOTHING",
    )
    .bind(message_id)
    .bind(job.cache_key.as_slice())
    .bind(&job.model)
    .bind(&job.voice_id)
    .bind(&job.script)
    .execute(&mut **tx)
    .await
    .map_err(map_constraint_error)?;
    sqlx::query("SELECT pg_notify($1, '')")
        .bind(SPEECH_NOTIFICATION_CHANNEL)
        .execute(&mut **tx)
        .await?;
    Ok(Ok(()))
}

/// Locks a message row: everything that changes a rendering takes this lock first.
async fn lock_message_in(tx: &mut Transaction<'_, Postgres>, message_id: Uuid) -> AppResult<()> {
    sqlx::query("SELECT 1 FROM messages WHERE id = $1 FOR UPDATE")
        .bind(message_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// `true` when the render moved to ready.
async fn mark_ready_in(
    tx: &mut Transaction<'_, Postgres>,
    message_id: Uuid,
    cache_key: &[u8; 32],
    meta: &AudioMeta,
    lease_id: Option<Uuid>,
) -> AppResult<bool> {
    let duration = i32::try_from(meta.duration_milliseconds).map_err(AppError::internal)?;
    Ok(sqlx::query(
        "UPDATE speech_renders SET status = 'ready', duration_ms = $3, peaks = $4,
            completed_at = clock_timestamp(), last_error = NULL,
            lease_id = NULL, lease_expires_at = NULL,
            attempts = attempts + CASE WHEN $5::uuid IS NULL THEN 0 ELSE 1 END
         WHERE message_id = $1 AND cache_key = $2 AND status <> 'ready'
           AND ($5::uuid IS NULL OR lease_id = $5 OR lease_id IS NULL)",
    )
    .bind(message_id)
    .bind(cache_key.as_slice())
    .bind(duration)
    .bind(&meta.peaks)
    .bind(lease_id)
    .execute(&mut **tx)
    .await
    .map_err(map_constraint_error)?
    .rows_affected()
        == 1)
}

/// Tells every participant that a message changed without new content (its speech became
/// ready or failed): a `message` sync event and a `message.updated` Ting handoff each.
/// Tombstones are not announced again.
pub(crate) async fn announce_message_update_in(
    tx: &mut Transaction<'_, Postgres>,
    message_id: Uuid,
) -> AppResult<()> {
    // The message lock serializes delivery revisions with concurrent edits and deletions.
    let Some((conversation_id, organization_id)): Option<(Uuid, String)> = sqlx::query_as(
        "SELECT conversation_id, organization_id FROM messages WHERE id = $1 FOR UPDATE",
    )
    .bind(message_id)
    .fetch_optional(&mut **tx)
    .await?
    else {
        return Ok(());
    };
    let deleted: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM message_history WHERE message_id = $1 AND deleted_at IS NOT NULL)",
    )
    .bind(message_id)
    .fetch_one(&mut **tx)
    .await?;
    if deleted {
        return Ok(());
    }
    // Revision 1 is the send; retention may have let the older deliveries go, so an
    // announcement is never below revision 2.
    let revision: i64 = sqlx::query_scalar(
        "SELECT COALESCE(max(delivery_revision), 1) + 1 FROM actor_deliveries
         WHERE message_id = $1 AND delivery_kind = 'message'",
    )
    .bind(message_id)
    .fetch_one(&mut **tx)
    .await?;
    let targets: Vec<(String, String)> = sqlx::query_as(
        "SELECT actor_kind::text, actor_id FROM effective_conversation_participants
         WHERE conversation_id = $1 ORDER BY actor_kind, actor_id",
    )
    .bind(conversation_id)
    .fetch_all(&mut **tx)
    .await?;
    for (kind, target) in targets {
        sqlx::query(
            "INSERT INTO actor_deliveries (id, organization_id, target_kind, target_id, sequence,
                delivery_kind, conversation_id, message_id, delivery_revision)
             VALUES ($1, $2, $3::text::actor_kind, $4, 1, 'message', $5, $6, $7)",
        )
        .bind(Uuid::now_v7())
        .bind(&organization_id)
        .bind(&kind)
        .bind(&target)
        .bind(conversation_id)
        .bind(message_id)
        .bind(revision)
        .execute(&mut **tx)
        .await
        .map_err(map_constraint_error)?;
        sqlx::query("SELECT pg_notify('dm_delivery', $1)")
            .bind(serde_json::json!({ "account": target }).to_string())
            .execute(&mut **tx)
            .await?;
    }
    Ok(())
}

/// 429 for an account over a speech limit, with `Retry-After` set to when the window resets.
#[must_use]
pub fn limit_error(limit: LimitExceeded, allowed: u64) -> AppError {
    let retry = retry_after_seconds(limit);
    let (code, message, hint) = match limit {
        LimitExceeded::RendersPerMinute => (
            "speech_rate_limited",
            format!(
                "This account already had {allowed} speech renders this minute, the most DM allows. Audio that is already rendered still plays."
            ),
            format!("Retry in {retry} seconds."),
        ),
        LimitExceeded::DailyCharacters => (
            "speech_budget_exceeded",
            format!(
                "This would take the account past its {allowed} characters of speech for today (UTC). Rendered audio still plays; text messages are not limited."
            ),
            format!("Send text instead, or retry after midnight UTC ({retry} seconds)."),
        ),
        LimitExceeded::TranscriptionsPerMinute => (
            "transcription_rate_limited",
            format!(
                "This account already asked for {allowed} transcriptions this minute, the most DM allows."
            ),
            format!("Retry in {retry} seconds."),
        ),
    };
    AppError::refused(http::StatusCode::TOO_MANY_REQUESTS, code, message)
        .with_hint(hint)
        .with_retry_after(retry)
        .with_details(serde_json::json!({ "limit": allowed, "retry_after_seconds": retry }))
}
