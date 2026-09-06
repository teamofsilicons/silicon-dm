//! Transaction-scoped mutation idempotency.

use serde::Serialize;
use sqlx::{FromRow, Postgres, Transaction};
use subtle::ConstantTimeEq as _;
use uuid::Uuid;

use crate::{
    AppError, AppResult,
    domain::{ActorRef, IdempotencyKey, OrganizationId},
};

use super::map_constraint_error;

pub(crate) enum IdempotencyClaim {
    Acquired,
    Replay(Uuid),
}

pub(crate) struct IdempotencyResult<'a> {
    pub resource_type: &'a str,
    pub resource_id: Uuid,
    pub response_status: u16,
}

#[derive(FromRow)]
struct IdempotencyRecord {
    request_hash: Vec<u8>,
    status: String,
    resource_id: Option<Uuid>,
}

pub(crate) fn request_hash<T: Serialize>(request: &T) -> AppResult<[u8; 32]> {
    let mut hasher = blake3::Hasher::new();
    serde_json::to_writer(&mut hasher, request).map_err(AppError::internal)?;
    Ok(*hasher.finalize().as_bytes())
}

pub(crate) async fn claim(
    transaction: &mut Transaction<'_, Postgres>,
    organization_id: &OrganizationId,
    actor: &ActorRef,
    operation: &str,
    key: &IdempotencyKey,
    hash: &[u8; 32],
) -> AppResult<IdempotencyClaim> {
    let lease_owner = Uuid::now_v7();
    let inserted = sqlx::query_scalar::<_, Uuid>(
        r#"
        INSERT INTO idempotency_records (
            organization_id,
            actor_kind,
            actor_id,
            operation,
            idempotency_key,
            request_hash,
            status,
            lease_owner,
            lease_expires_at,
            expires_at
        )
        VALUES (
            $1,
            $2::text::actor_kind,
            $3,
            $4,
            $5,
            $6,
            'in_progress',
            $7,
            transaction_timestamp() + interval '2 minutes',
            transaction_timestamp() + interval '30 days'
        )
        ON CONFLICT DO NOTHING
        RETURNING lease_owner
        "#,
    )
    .bind(organization_id.as_str())
    .bind(actor.actor_type.as_str())
    .bind(actor.id.as_str())
    .bind(operation)
    .bind(key.as_str())
    .bind(hash.as_slice())
    .bind(lease_owner)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(map_constraint_error)?;
    if inserted.is_some() {
        return Ok(IdempotencyClaim::Acquired);
    }

    let existing = sqlx::query_as::<_, IdempotencyRecord>(
        r#"
        SELECT
            request_hash,
            status::text AS status,
            resource_id
        FROM idempotency_records
        WHERE organization_id = $1
          AND actor_kind = $2::text::actor_kind
          AND actor_id = $3
          AND operation = $4
          AND idempotency_key = $5
        FOR UPDATE
        "#,
    )
    .bind(organization_id.as_str())
    .bind(actor.actor_type.as_str())
    .bind(actor.id.as_str())
    .bind(operation)
    .bind(key.as_str())
    .fetch_optional(&mut **transaction)
    .await?
    .ok_or_else(|| AppError::conflict("idempotency request could not be serialized"))?;

    if existing.request_hash.len() != hash.len()
        || !bool::from(existing.request_hash.as_slice().ct_eq(hash.as_slice()))
    {
        return Err(AppError::conflict(
            "idempotency key was already used with different request content",
        ));
    }
    match (existing.status.as_str(), existing.resource_id) {
        ("completed", Some(resource_id)) => Ok(IdempotencyClaim::Replay(resource_id)),
        ("in_progress", _) => Err(AppError::conflict(
            "idempotent request is still in progress",
        )),
        _ => Err(AppError::internal(anyhow::anyhow!(
            "invalid idempotency record state"
        ))),
    }
}

pub(crate) async fn complete(
    transaction: &mut Transaction<'_, Postgres>,
    organization_id: &OrganizationId,
    actor: &ActorRef,
    operation: &str,
    key: &IdempotencyKey,
    result: IdempotencyResult<'_>,
) -> AppResult<()> {
    let updated = sqlx::query(
        r#"
        UPDATE idempotency_records
        SET status = 'completed',
            lease_owner = NULL,
            lease_expires_at = NULL,
            resource_type = $6,
            resource_id = $7,
            response_status = $8
        WHERE organization_id = $1
          AND actor_kind = $2::text::actor_kind
          AND actor_id = $3
          AND operation = $4
          AND idempotency_key = $5
          AND status = 'in_progress'
        "#,
    )
    .bind(organization_id.as_str())
    .bind(actor.actor_type.as_str())
    .bind(actor.id.as_str())
    .bind(operation)
    .bind(key.as_str())
    .bind(result.resource_type)
    .bind(result.resource_id)
    .bind(i32::from(result.response_status))
    .execute(&mut **transaction)
    .await
    .map_err(map_constraint_error)?;
    if updated.rows_affected() != 1 {
        return Err(AppError::conflict(
            "idempotent request no longer owns its mutation lease",
        ));
    }
    Ok(())
}
