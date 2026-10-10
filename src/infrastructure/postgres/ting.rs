//! Durable, immutable references handed off to Ting independently of DM receipts.

use std::time::Duration;

use serde_json::{Value, json};
use sqlx::FromRow;
use time::OffsetDateTime;
use uuid::Uuid;

use super::{
    PostgresStore, map_constraint_error,
    rows::{parse_actor_id, parse_actor_type},
};
use crate::{
    AppError, AppResult,
    domain::{ActorId, ActorRef},
};

/// Producer identity sealed into an outgoing Ting request.
#[derive(Clone, Debug)]
pub struct TingDeliveryContext {
    /// Configured DM application ID, used as the registered type prefix.
    pub app_id: String,
}

/// One exclusively leased, proof-ready Ting request.
#[derive(Clone, Debug)]
pub struct TingDeliveryClaim {
    /// Stable DM event identity and Ting idempotency key.
    pub delivery_id: Uuid,
    /// Unique claim token; an expired/replaced claimant cannot mutate progress.
    pub lease_id: Uuid,
    /// Recipient (storage key = Accounts uuid), independent of optional ISI routing.
    pub target: ActorRef,
    /// Account whose action caused this event (provenance only).
    pub originator: Option<ActorRef>,
    /// Private conversation identity for current access checks.
    pub conversation_id: Uuid,
    /// Original position in the recipient's DM stream.
    pub sequence: i64,
    /// Exact persisted UTF-8 body to transmit unchanged on every retry.
    pub request_body: String,
    /// Prior recoverable handoff failures; never drives a message failure.
    pub attempt_count: i64,
}

#[derive(FromRow)]
struct HandoffRecord {
    delivery_id: Uuid,
    target_kind: String,
    target_id: String,
    target_public_id: Option<String>,
    originator_kind: Option<String>,
    originator_id: Option<String>,
    conversation_id: Uuid,
    public_conversation_id: String,
    message_sequence: i64,
    delivery_sequence: i64,
    event: String,
    routing_address: Option<String>,
    request_body: Option<String>,
    attempt_count: i64,
}

/// Discards one claim call may record before it returns (empty) to the worker.
const MAX_DISCARDS_PER_CLAIM: usize = 500;

/// Why a handoff was discarded instead of sent.
pub const DISCARD_TING_NOT_CONFIGURED: &str = "ting_not_configured";
/// The request was prepared by the IAM-era worker and cannot be re-signed.
pub const DISCARD_PREPARED_BEFORE_CUTOVER: &str = "prepared_before_accounts_cutover";
/// The recipient is an IAM-era identity that was never linked to a Silicon Accounts
/// account, so Ting has nobody to deliver to.
pub const DISCARD_RECIPIENT_WITHOUT_ACCOUNT: &str = "recipient_without_account";

impl PostgresStore {
    /// Claims a bounded set of eligible handoffs, including disconnected recipients.
    /// The first claim persists the exact request; all later claims reuse those bytes.
    /// A request prepared before the move to Silicon Accounts is discarded, not sent.
    ///
    /// # Errors
    /// Rejects invalid claim settings, or reports a durable database failure.
    pub async fn claim_ting_deliveries(
        &self,
        owner: &str,
        limit: usize,
        lease_duration: Duration,
        context: &TingDeliveryContext,
    ) -> AppResult<Vec<TingDeliveryClaim>> {
        validate_text(owner, "Ting lease owner")?;
        context.validate()?;
        if !(1..=100).contains(&limit) {
            return Err(AppError::validation("Ting claim limit must be 1 to 100"));
        }
        let limit = i64::try_from(limit).map_err(AppError::internal)?;
        let lease_milliseconds = i64::try_from(lease_duration.as_millis())
            .map_err(|_| AppError::validation("Ting lease duration is too large"))?;
        if lease_milliseconds < 1 {
            return Err(AppError::validation("Ting lease duration must be positive"));
        }
        let mut transaction = self.pool().begin().await?;
        let mut claims = Vec::new();
        let mut discarded = 0_usize;
        // Discarded handoffs do not count towards the batch: keep looking (bounded) so a
        // backlog of legacy rows never stalls the ones that can be sent.
        while claims.is_empty() && discarded < MAX_DISCARDS_PER_CLAIM {
            let records = sqlx::query_as::<_, HandoffRecord>(
                r#"
            SELECT h.delivery_id,h.target_kind::text AS target_kind,h.target_id,
                a.current_id AS target_public_id,
                h.originator_kind::text AS originator_kind,h.originator_id,
                h.conversation_id,h.public_conversation_id,h.message_sequence,h.delivery_sequence,
                h.event,h.routing_address,h.request_body,h.attempt_count
            FROM ting_handoffs h
            LEFT JOIN accounts a ON a.uuid = h.target_id
            WHERE h.accepted_at IS NULL AND h.discarded_at IS NULL
                AND h.next_attempt_at<=clock_timestamp()
                AND (h.lease_id IS NULL OR h.lease_expires_at<=clock_timestamp())
                AND EXISTS(SELECT 1 FROM effective_conversation_participants p
                    WHERE p.conversation_id=h.conversation_id
                        AND p.actor_kind=h.target_kind AND p.actor_id=h.target_id)
            ORDER BY h.next_attempt_at,h.created_at,h.delivery_id
            LIMIT $1 FOR UPDATE OF h SKIP LOCKED
            "#,
            )
            .bind(limit)
            .fetch_all(&mut *transaction)
            .await?;
            if records.is_empty() {
                break;
            }
            for record in records {
                let discard = if record
                    .request_body
                    .as_deref()
                    .is_some_and(is_prepared_before_cutover)
                {
                    Some(DISCARD_PREPARED_BEFORE_CUTOVER)
                } else if crate::domain::is_account_uuid(&record.target_id) {
                    None
                } else {
                    Some(DISCARD_RECIPIENT_WITHOUT_ACCOUNT)
                };
                if let Some(reason) = discard {
                    sqlx::query(
                    "UPDATE ting_handoffs SET discarded_at=clock_timestamp(),discard_reason=$2 WHERE delivery_id=$1",
                )
                .bind(record.delivery_id)
                .bind(reason)
                .execute(&mut *transaction)
                .await
                .map_err(map_constraint_error)?;
                    discarded += 1;
                    continue;
                }
                let request_body = prepared_body(&record, context)?;
                let lease_id = Uuid::new_v4();
                sqlx::query(
                "UPDATE ting_handoffs SET request_body=$2,lease_id=$3,lease_owner=$4,\
                 lease_expires_at=clock_timestamp()+($5::bigint * interval '1 millisecond') WHERE delivery_id=$1",
            ).bind(record.delivery_id).bind(&request_body).bind(lease_id).bind(owner)
                .bind(lease_milliseconds).execute(&mut *transaction).await.map_err(map_constraint_error)?;
                claims.push(TingDeliveryClaim {
                    delivery_id: record.delivery_id,
                    lease_id,
                    target: ActorRef {
                        actor_type: parse_actor_type(&record.target_kind)?,
                        id: parse_actor_id(&record.target_id)?,
                    },
                    originator: match (record.originator_kind, record.originator_id) {
                        (Some(kind), Some(id)) => Some(ActorRef {
                            actor_type: parse_actor_type(&kind)?,
                            id: parse_actor_id(&id)?,
                        }),
                        _ => None,
                    },
                    conversation_id: record.conversation_id,
                    sequence: record.delivery_sequence,
                    request_body,
                    attempt_count: record.attempt_count,
                });
            }
        }
        transaction.commit().await.map_err(map_constraint_error)?;
        Ok(claims)
    }

    /// Rechecks lease ownership and current participation immediately before sending.
    ///
    /// # Errors
    /// Reports database failures rather than assuming authority.
    pub async fn ting_delivery_authorized(&self, claim: &TingDeliveryClaim) -> AppResult<bool> {
        sqlx::query_scalar(
            r#"
            SELECT EXISTS(
                SELECT 1 FROM ting_handoffs h
                JOIN effective_conversation_participants p ON p.conversation_id=h.conversation_id
                    AND p.actor_kind=h.target_kind AND p.actor_id=h.target_id
                WHERE h.delivery_id=$1 AND h.lease_id=$2 AND h.lease_expires_at>clock_timestamp()
                    AND h.target_kind::text=$3 AND h.target_id=$4
                    AND h.accepted_at IS NULL AND h.discarded_at IS NULL)
            "#,
        )
        .bind(claim.delivery_id)
        .bind(claim.lease_id)
        .bind(claim.target.actor_type.as_str())
        .bind(claim.target.id.as_str())
        .fetch_one(self.pool())
        .await
        .map_err(AppError::Database)
    }

    /// Marks unsent handoffs discarded with a reason (never deletes them): what DM does
    /// while Ting is not configured. Returns how many were marked.
    ///
    /// # Errors
    /// Database failures.
    pub async fn discard_ting_handoffs(&self, reason: &str, limit: i64) -> AppResult<u64> {
        validate_text(reason, "Ting discard reason")?;
        Ok(sqlx::query(
            "UPDATE ting_handoffs SET discarded_at=clock_timestamp(), discard_reason=$1
             WHERE delivery_id IN (
                SELECT delivery_id FROM ting_handoffs
                WHERE accepted_at IS NULL AND discarded_at IS NULL
                  AND (lease_id IS NULL OR lease_expires_at <= clock_timestamp())
                ORDER BY created_at LIMIT $2 FOR UPDATE SKIP LOCKED)",
        )
        .bind(reason)
        .bind(limit)
        .execute(self.pool())
        .await
        .map_err(map_constraint_error)?
        .rows_affected())
    }
    /// Records durable Ting acceptance without acknowledging the DM stream or changing receipts.
    ///
    /// # Errors
    /// Rejects malformed acceptance or a lost lease; repeating an already stored
    /// identical acceptance succeeds, including after a lost database reply.
    pub async fn accept_ting_delivery(
        &self,
        delivery_id: Uuid,
        lease_id: Uuid,
        ting_id: &str,
        created_at: OffsetDateTime,
        silent: bool,
    ) -> AppResult<()> {
        validate_text(ting_id, "Ting ID")?;
        let result = sqlx::query(
            r#"
            UPDATE ting_handoffs SET accepted_at=clock_timestamp(),ting_id=$3,ting_created_at=$4,
                silent=$5,lease_id=NULL,lease_owner=NULL,lease_expires_at=NULL,last_error_code=NULL
            WHERE delivery_id=$1 AND lease_id=$2 AND lease_expires_at>clock_timestamp() AND accepted_at IS NULL
            "#,
        ).bind(delivery_id).bind(lease_id).bind(ting_id).bind(created_at).bind(silent)
            .execute(self.pool()).await.map_err(map_constraint_error)?;
        if result.rows_affected() == 1 {
            return Ok(());
        }
        let identical: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM ting_handoffs WHERE delivery_id=$1 AND accepted_at IS NOT NULL \
             AND ting_id=$2 AND ting_created_at=$3 AND silent=$4)",
        ).bind(delivery_id).bind(ting_id).bind(created_at).bind(silent).fetch_one(self.pool()).await?;
        if identical {
            Ok(())
        } else {
            Err(AppError::conflict("Ting delivery claim is no longer owned"))
        }
    }

    /// Releases a failed/uncertain attempt for recoverable retry with the same body and key.
    /// Retry counts saturate; no number of transport failures changes message status.
    ///
    /// # Errors
    /// Rejects invalid diagnostic codes, database errors and lost leases.
    pub async fn retry_ting_delivery(
        &self,
        delivery_id: Uuid,
        lease_id: Uuid,
        next_attempt_at: OffsetDateTime,
        error_code: &str,
    ) -> AppResult<()> {
        validate_text(error_code, "Ting retry error code")?;
        let result = sqlx::query(
            r#"
            UPDATE ting_handoffs SET next_attempt_at=$3,last_error_code=$4,
                attempt_count=CASE WHEN attempt_count<9223372036854775807 THEN attempt_count+1 ELSE attempt_count END,
                lease_id=NULL,lease_owner=NULL,lease_expires_at=NULL
            WHERE delivery_id=$1 AND lease_id=$2 AND lease_expires_at>clock_timestamp() AND accepted_at IS NULL
            "#,
        ).bind(delivery_id).bind(lease_id).bind(next_attempt_at).bind(error_code)
            .execute(self.pool()).await.map_err(map_constraint_error)?;
        if result.rows_affected() == 1 {
            return Ok(());
        }
        let accepted: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM ting_handoffs WHERE delivery_id=$1 AND accepted_at IS NOT NULL)",
        ).bind(delivery_id).fetch_one(self.pool()).await?;
        if accepted {
            Ok(())
        } else {
            Err(AppError::conflict("Ting delivery claim is no longer owned"))
        }
    }
}

impl PostgresStore {
    /// Whether DM enrolled the account with Ting.
    ///
    /// # Errors
    /// Database failures.
    pub async fn ting_enrolled(&self, app_id: &str, account: &str) -> AppResult<bool> {
        Ok(sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM ting_automatic_enrollments
             WHERE app_id = $1 AND generation = 0 AND organization_id = '@accounts'
               AND actor_id = $2 AND enrolled_at IS NOT NULL)",
        )
        .bind(app_id)
        .bind(account)
        .fetch_one(self.pool())
        .await?)
    }

    /// Reserves an enrolment attempt: `true` when the account is not enrolled and no
    /// attempt ran in the last ten minutes.
    ///
    /// # Errors
    /// Database failures.
    pub async fn reserve_ting_enrolment(&self, app_id: &str, actor: &ActorRef) -> AppResult<bool> {
        Ok(sqlx::query_scalar::<_, bool>(
            "INSERT INTO ting_automatic_enrollments (app_id, generation, organization_id, actor_kind, actor_id, attempted_at)
             VALUES ($1, 0, '@accounts', $2::text::actor_kind, $3, clock_timestamp())
             ON CONFLICT (app_id, generation, organization_id, actor_kind, actor_id) DO UPDATE
                SET attempted_at = clock_timestamp(), last_error = NULL
                WHERE ting_automatic_enrollments.enrolled_at IS NULL
                  AND ting_automatic_enrollments.attempted_at < clock_timestamp() - interval '10 minutes'
             RETURNING true",
        )
        .bind(app_id)
        .bind(actor.actor_type.as_str())
        .bind(actor.id.as_str())
        .fetch_optional(self.pool())
        .await
        .map_err(map_constraint_error)?
        .unwrap_or(false))
    }

    /// Records the outcome of an enrolment attempt.
    ///
    /// # Errors
    /// Database failures.
    pub async fn finish_ting_enrolment(
        &self,
        app_id: &str,
        actor: &ActorRef,
        subscription_id: Option<&str>,
        error: Option<&str>,
    ) -> AppResult<()> {
        sqlx::query(
            "UPDATE ting_automatic_enrollments
             SET subscription_id = $4, enrolled_at = CASE WHEN $4 IS NULL THEN NULL ELSE clock_timestamp() END,
                 last_error = $5
             WHERE app_id = $1 AND generation = 0 AND organization_id = '@accounts'
               AND actor_kind = $2::text::actor_kind AND actor_id = $3",
        )
        .bind(app_id)
        .bind(actor.actor_type.as_str())
        .bind(actor.id.as_str())
        .bind(subscription_id)
        .bind(error.map(|error| error.chars().take(128).collect::<String>()))
        .execute(self.pool())
        .await
        .map_err(map_constraint_error)?;
        Ok(())
    }

    /// Stores an encrypted proof refresh token (`sapr_…`) for later enrolment refreshes.
    ///
    /// # Errors
    /// Database failures.
    #[allow(clippy::too_many_arguments, reason = "one row of a narrow table")]
    pub async fn save_ting_proof_grant(
        &self,
        app_id: &str,
        actor: &ActorRef,
        purpose: &str,
        proof_id: &str,
        nonce: &[u8],
        ciphertext: &[u8],
        refresh_expires_at: Option<time::OffsetDateTime>,
    ) -> AppResult<()> {
        sqlx::query(
            "INSERT INTO ting_proof_grants (app_id, actor_kind, actor_id, purpose, proof_id, nonce, ciphertext, refresh_expires_at)
             VALUES ($1, $2::text::actor_kind, $3, $4, $5, $6, $7, $8)
             ON CONFLICT (app_id, actor_kind, actor_id, purpose) DO UPDATE SET proof_id = EXCLUDED.proof_id,
                nonce = EXCLUDED.nonce, ciphertext = EXCLUDED.ciphertext,
                refresh_expires_at = EXCLUDED.refresh_expires_at, updated_at = clock_timestamp()",
        )
        .bind(app_id)
        .bind(actor.actor_type.as_str())
        .bind(actor.id.as_str())
        .bind(purpose)
        .bind(proof_id)
        .bind(nonce)
        .bind(ciphertext)
        .bind(refresh_expires_at)
        .execute(self.pool())
        .await
        .map_err(map_constraint_error)?;
        Ok(())
    }

    /// The encrypted proof refresh token stored for an account and purpose.
    ///
    /// # Errors
    /// Database failures.
    pub async fn ting_proof_grant(
        &self,
        app_id: &str,
        actor: &ActorRef,
        purpose: &str,
    ) -> AppResult<Option<(Vec<u8>, Vec<u8>)>> {
        Ok(sqlx::query_as(
            "SELECT nonce, ciphertext FROM ting_proof_grants
             WHERE app_id = $1 AND actor_kind = $2::text::actor_kind AND actor_id = $3 AND purpose = $4",
        )
        .bind(app_id)
        .bind(actor.actor_type.as_str())
        .bind(actor.id.as_str())
        .bind(purpose)
        .fetch_optional(self.pool())
        .await?)
    }

    /// Forgets the enrolment of a recipient Ting no longer knows (re-enrolled on its next
    /// request).
    ///
    /// # Errors
    /// Database failures.
    pub async fn forget_ting_enrolment(&self, app_id: &str, actor: &ActorRef) -> AppResult<()> {
        sqlx::query(
            "DELETE FROM ting_automatic_enrollments WHERE app_id = $1 AND organization_id = '@accounts'
             AND actor_kind = $2::text::actor_kind AND actor_id = $3",
        )
        .bind(app_id)
        .bind(actor.actor_type.as_str())
        .bind(actor.id.as_str())
        .execute(self.pool())
        .await?;
        Ok(())
    }
}

impl TingDeliveryContext {
    fn validate(&self) -> AppResult<()> {
        validate_text(&self.app_id, "Ting producer app ID")?;
        if self.app_id.len() + ".sync.changed".len() > 255 {
            return Err(AppError::validation("Ting type exceeds 255 bytes"));
        }
        Ok(())
    }
}

/// Bodies the IAM-era worker prepared name an organization and a bare public id.
fn is_prepared_before_cutover(body: &str) -> bool {
    serde_json::from_str::<Value>(body).map_or(true, |value| {
        value.get("org_id").is_some() || value["for"].is_string()
    })
}

fn prepared_body(record: &HandoffRecord, context: &TingDeliveryContext) -> AppResult<String> {
    let event_type = format!("{}.sync.changed", context.app_id);
    if let Some(body) = &record.request_body {
        let parsed: Value = serde_json::from_str(body).map_err(AppError::internal)?;
        if parsed["type"] != event_type {
            return Err(AppError::conflict(
                "prepared Ting request belongs to another producer",
            ));
        }
        return Ok(body.clone());
    }
    let message_id =
        silicon_dm_protocol::message_code(record.message_sequence).ok_or_else(|| {
            AppError::internal(anyhow::anyhow!("invalid Ting source message sequence"))
        })?;
    let mut metadata = json!({});
    if let Some(address) = &record.routing_address {
        let address: ActorId = address.parse().map_err(AppError::internal)?;
        if let Ok((base, Some(isi))) = address.address_parts()
            && (base == record.target_id || record.target_public_id.as_deref() == Some(base))
            && record.target_kind == "silicon"
        {
            metadata["isi"] = json!(isi);
        }
    }
    serde_json::to_string(&json!({
        "type": event_type,
        "for": { "uuid": record.target_id, "id": record.target_public_id },
        "key": record.delivery_id,
        "data": {
            "schema_version": 2,
            "event": record.event,
            "conversation_id": record.public_conversation_id,
            "message_id": message_id,
            "delivery_id": record.delivery_id,
            "delivery_sequence": record.delivery_sequence,
        },
        "metadata": metadata,
    }))
    .map_err(AppError::internal)
}

fn validate_text(value: &str, field: &str) -> AppResult<()> {
    if value.is_empty() || value.len() > 255 || value.chars().any(char::is_control) {
        Err(AppError::validation(format!(
            "{field} must contain 1 to 255 bytes without controls"
        )))
    } else {
        Ok(())
    }
}
