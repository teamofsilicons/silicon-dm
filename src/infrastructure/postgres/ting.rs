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
    domain::{ActorId, ActorRef, OrganizationId},
};

/// Producer identity and DM data generation sealed into an outgoing Ting request.
#[derive(Clone, Debug)]
pub struct TingDeliveryContext {
    /// Configured DM application ID, used as the registered type prefix.
    pub app_id: String,
    /// DM sandbox ID, or `None` for production.
    pub testing_environment_id: Option<Uuid>,
    /// Current sandbox generation, paired with the sandbox ID.
    pub testing_generation: Option<i64>,
}

/// One exclusively leased, proof-ready Ting request.
#[derive(Clone, Debug)]
pub struct TingDeliveryClaim {
    /// Stable DM event identity and Ting idempotency key.
    pub delivery_id: Uuid,
    /// Unique claim token; an expired/replaced claimant cannot mutate progress.
    pub lease_id: Uuid,
    /// Owning organization.
    pub organization_id: OrganizationId,
    /// Canonical recipient, independent of optional ISI routing.
    pub target: ActorRef,
    /// Actor whose authenticated mutation caused this event. Unattributable legacy
    /// events remain pending; their recipient is never a substitute proof owner.
    pub originator: Option<ActorRef>,
    /// Private conversation identity for current access checks.
    pub conversation_id: Uuid,
    /// Original position in the recipient's DM stream.
    pub sequence: i64,
    /// Exact persisted UTF-8 body to hash, obtain a proof for, and transmit unchanged.
    pub request_body: String,
    /// Prior recoverable handoff failures; never drives a message failure.
    pub attempt_count: i64,
}

#[derive(FromRow)]
struct HandoffRecord {
    delivery_id: Uuid,
    organization_id: String,
    target_kind: String,
    target_id: String,
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

impl PostgresStore {
    /// Claims a bounded set of eligible handoffs, including disconnected recipients.
    /// The first claim persists the exact request; all later claims reuse those bytes.
    ///
    /// # Errors
    /// Rejects invalid claim/context settings, mismatched data planes and changed
    /// prepared contexts, or reports a durable database failure.
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
        let schema: String = sqlx::query_scalar("SELECT current_schema()")
            .fetch_one(&mut *transaction)
            .await?;
        let expected_schema = context
            .testing_environment_id
            .map_or_else(|| "dm".to_owned(), |id| format!("dm_test_{}", id.simple()));
        if schema != expected_schema {
            return Err(AppError::conflict(
                "Ting delivery context does not match the DM data plane",
            ));
        }
        let records = sqlx::query_as::<_, HandoffRecord>(
            r#"
            SELECT h.delivery_id,h.organization_id,h.target_kind::text AS target_kind,h.target_id,
                h.originator_kind::text AS originator_kind,h.originator_id,
                h.conversation_id,h.public_conversation_id,h.message_sequence,h.delivery_sequence,
                h.event,h.routing_address,h.request_body,h.attempt_count
            FROM ting_handoffs h
            JOIN organization_snapshots o ON o.organization_id=h.organization_id AND o.status='active'
            JOIN actor_snapshots a ON a.organization_id=h.organization_id
                AND a.actor_kind=h.target_kind AND a.actor_id=h.target_id AND a.status='active'
            WHERE h.accepted_at IS NULL AND h.next_attempt_at<=clock_timestamp()
                AND (h.lease_id IS NULL OR h.lease_expires_at<=clock_timestamp())
                AND (h.request_body IS NULL OR (
                    h.request_body::jsonb->'metadata'->'testing_environment_id'=$2::jsonb
                    AND h.request_body::jsonb->'metadata'->'testing_generation'=$3::jsonb
                    AND h.request_body::jsonb->>'type'=$4))
                AND EXISTS(SELECT 1 FROM effective_conversation_participants p
                    WHERE p.conversation_id=h.conversation_id AND p.organization_id=h.organization_id
                        AND p.actor_kind=h.target_kind AND p.actor_id=h.target_id)
            ORDER BY h.next_attempt_at,h.created_at,h.delivery_id
            LIMIT $1 FOR UPDATE OF h SKIP LOCKED
            "#,
        ).bind(limit).bind(sqlx::types::Json(json!(context.testing_environment_id)))
            .bind(sqlx::types::Json(json!(context.testing_generation)))
            .bind(format!("{}.sync.changed", context.app_id)).fetch_all(&mut *transaction).await?;
        let mut claims = Vec::with_capacity(records.len());
        for record in records {
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
                organization_id: record.organization_id.parse().map_err(AppError::internal)?,
                target: ActorRef {
                    actor_type: parse_actor_type(&record.target_kind)?,
                    id: parse_actor_id(&record.target_id)?,
                },
                originator: match (record.originator_kind, record.originator_id) {
                    (Some(kind), Some(id)) => Some(ActorRef {
                        actor_type: parse_actor_type(&kind)?,
                        id: parse_actor_id(&id)?,
                    }),
                    (None, None) => None,
                    _ => {
                        return Err(AppError::internal(anyhow::anyhow!(
                            "incomplete Ting event originator"
                        )));
                    }
                },
                conversation_id: record.conversation_id,
                sequence: record.delivery_sequence,
                request_body,
                attempt_count: record.attempt_count,
            });
        }
        transaction.commit().await.map_err(map_constraint_error)?;
        Ok(claims)
    }

    /// Rechecks current local membership and claim ownership immediately before send.
    /// This is additional to the publisher's live IAM authorization/proof checks.
    ///
    /// # Errors
    /// Reports database failures rather than assuming authority.
    pub async fn ting_delivery_authorized(&self, claim: &TingDeliveryClaim) -> AppResult<bool> {
        sqlx::query_scalar(
            r#"
            SELECT EXISTS(
                SELECT 1 FROM ting_handoffs h
                JOIN organization_snapshots o ON o.organization_id=h.organization_id AND o.status='active'
                JOIN actor_snapshots a ON a.organization_id=h.organization_id
                    AND a.actor_kind=h.target_kind AND a.actor_id=h.target_id AND a.status='active'
                JOIN effective_conversation_participants p ON p.conversation_id=h.conversation_id
                    AND p.organization_id=h.organization_id AND p.actor_kind=h.target_kind AND p.actor_id=h.target_id
                WHERE h.delivery_id=$1 AND h.lease_id=$2 AND h.lease_expires_at>clock_timestamp()
                    AND h.organization_id=$3 AND h.target_kind::text=$4 AND h.target_id=$5
                    AND h.originator_kind::text IS NOT DISTINCT FROM $6
                    AND h.originator_id IS NOT DISTINCT FROM $7
                    AND h.accepted_at IS NULL)
            "#,
        ).bind(claim.delivery_id).bind(claim.lease_id).bind(claim.organization_id.as_str())
            .bind(claim.target.actor_type.as_str()).bind(claim.target.id.as_str())
            .bind(claim.originator.as_ref().map(|actor| actor.actor_type.as_str()))
            .bind(claim.originator.as_ref().map(|actor| actor.id.as_str()))
            .fetch_one(self.pool()).await.map_err(AppError::Database)
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

impl TingDeliveryContext {
    fn validate(&self) -> AppResult<()> {
        validate_text(&self.app_id, "Ting producer app ID")?;
        if self.app_id.len() + ".sync.changed".len() > 255 {
            return Err(AppError::validation("Ting type exceeds 255 bytes"));
        }
        match (self.testing_environment_id, self.testing_generation) {
            (None, None) => Ok(()),
            (Some(id), Some(generation)) if !id.is_nil() && generation > 0 => Ok(()),
            _ => Err(AppError::validation(
                "Ting test context requires an environment ID and positive generation",
            )),
        }
    }
}

fn prepared_body(record: &HandoffRecord, context: &TingDeliveryContext) -> AppResult<String> {
    let event_type = format!("{}.sync.changed", context.app_id);
    if let Some(body) = &record.request_body {
        let parsed: Value = serde_json::from_str(body).map_err(AppError::internal)?;
        if parsed["type"] != event_type
            || parsed["metadata"]["testing_environment_id"] != json!(context.testing_environment_id)
            || parsed["metadata"]["testing_generation"] != json!(context.testing_generation)
        {
            return Err(AppError::conflict(
                "prepared Ting request belongs to another producer or generation",
            ));
        }
        return Ok(body.clone());
    }
    let message_id =
        silicon_dm_protocol::message_code(record.message_sequence).ok_or_else(|| {
            AppError::internal(anyhow::anyhow!("invalid Ting source message sequence"))
        })?;
    let mut metadata = json!({
        "testing_environment_id": context.testing_environment_id,
        "testing_generation": context.testing_generation,
    });
    if let Some(address) = &record.routing_address {
        let address: ActorId = address.parse().map_err(AppError::internal)?;
        let (base, isi) = address.address_parts().map_err(AppError::validation)?;
        if base == record.target_id
            && record.target_kind == "silicon"
            && let Some(isi) = isi
        {
            metadata["isi"] = json!(isi);
        }
    }
    serde_json::to_string(&json!({
        "org_id": record.organization_id,
        "type": event_type,
        "for": record.target_id,
        "key": record.delivery_id,
        "data": {
            "schema_version": 1,
            "event": record.event,
            "org_id": record.organization_id,
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
