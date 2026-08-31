//! Transactional message bundle persistence.

use std::collections::HashSet;

use serde::Serialize;
use sqlx::{FromRow, Postgres, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{
    AppError, AppResult,
    application::commands::CreateBundleCommand,
    domain::{
        ActorRef, ActorType, Bundle, BundleDetail, MAX_VOICE_DURATION_MILLISECONDS, OrganizationId,
    },
};

use super::{
    PostgresStore,
    directory::refresh_directory_in,
    idempotency::{IdempotencyClaim, IdempotencyResult, claim, complete, request_hash},
    map_constraint_error,
    messages::{enqueue_message_deliveries_in, insert_message_in, require_participant_in},
    rows::{parse_actor_id, parse_actor_type},
};

const CREATE_BUNDLE_OPERATION: &str = "bundles.create";

#[derive(Serialize)]
struct BundleIdempotencyContent<'a> {
    conversation_id: Uuid,
    message_ids: &'a [Uuid],
    display_content_hash: &'a [u8],
}

#[derive(FromRow)]
struct BundleRecord {
    id: Uuid,
    conversation_id: Uuid,
    created_by_kind: String,
    created_by_id: String,
    created_at: OffsetDateTime,
    display_message_id: Uuid,
}

#[derive(FromRow)]
struct BundleMemberRecord {
    message_id: Uuid,
}

impl PostgresStore {
    /// Creates a Silicon-only, flat, non-destructive message bundle.
    ///
    /// # Errors
    ///
    /// Rejects Carbon creators, invalid display content, unavailable or already
    /// bundled members, and conflicting idempotency reuse.
    #[allow(
        clippy::too_many_lines,
        reason = "the transaction keeps bundle validation, display creation, sealing, and outbox writes visibly atomic"
    )]
    pub async fn create_bundle(&self, command: CreateBundleCommand) -> AppResult<Bundle> {
        if command.creator.actor_type != ActorType::Silicon {
            return Err(AppError::Forbidden);
        }
        command.bundle.validate().map_err(AppError::validation)?;
        if command
            .bundle
            .display_message
            .sender_id
            .as_ref()
            .is_some_and(|sender_id| *sender_id != command.creator.id)
        {
            return Err(AppError::Forbidden);
        }
        if command
            .voice_duration_milliseconds
            .is_some_and(|duration| duration > MAX_VOICE_DURATION_MILLISECONDS)
        {
            return Err(AppError::validation(
                "voice message duration may not exceed 48 hours",
            ));
        }
        let display_content_hash = command.bundle.display_message.content_digest();
        let hash = request_hash(&BundleIdempotencyContent {
            conversation_id: command.conversation_id,
            message_ids: &command.bundle.message_ids,
            display_content_hash: display_content_hash.as_bytes(),
        })?;
        let mut transaction = self.pool().begin().await?;
        refresh_directory_in(
            &mut transaction,
            &command.organization_id,
            std::slice::from_ref(&command.creator),
        )
        .await?;
        require_participant_in(
            &mut transaction,
            &command.organization_id,
            &command.creator,
            command.conversation_id,
        )
        .await?;
        let claim = claim(
            &mut transaction,
            &command.organization_id,
            &command.creator,
            CREATE_BUNDLE_OPERATION,
            &command.idempotency_key,
            &hash,
        )
        .await?;
        let bundle_id = match claim {
            IdempotencyClaim::Replay(resource_id) => resource_id,
            IdempotencyClaim::Acquired => {
                lock_bundle_members_in(
                    &mut transaction,
                    &command.organization_id,
                    command.conversation_id,
                    &command.bundle.message_ids,
                )
                .await?;
                let display_message_id = insert_message_in(
                    &mut transaction,
                    &command.organization_id,
                    command.conversation_id,
                    &command.creator,
                    &command.bundle.display_message,
                    command.voice_duration_milliseconds,
                )
                .await?;
                let bundle_id = Uuid::now_v7();
                sqlx::query(
                    r#"
                    INSERT INTO dm.message_bundles (
                        id,
                        conversation_id,
                        organization_id,
                        created_by_kind,
                        created_by_id
                    )
                    VALUES ($1, $2, $3, $4::text::dm.actor_kind, $5)
                    "#,
                )
                .bind(bundle_id)
                .bind(command.conversation_id)
                .bind(command.organization_id.as_str())
                .bind(command.creator.actor_type.as_str())
                .bind(command.creator.id.as_str())
                .execute(&mut *transaction)
                .await
                .map_err(map_constraint_error)?;
                insert_bundle_item_in(
                    &mut transaction,
                    bundle_id,
                    command.conversation_id,
                    &command.organization_id,
                    display_message_id,
                    "display",
                    0,
                )
                .await?;
                for (index, message_id) in command.bundle.message_ids.iter().enumerate() {
                    let position = i16::try_from(index + 1).map_err(|_| {
                        AppError::validation("bundle member position exceeds supported range")
                    })?;
                    insert_bundle_item_in(
                        &mut transaction,
                        bundle_id,
                        command.conversation_id,
                        &command.organization_id,
                        *message_id,
                        "member",
                        position,
                    )
                    .await?;
                }
                enqueue_message_deliveries_in(
                    &mut transaction,
                    &command.organization_id,
                    command.conversation_id,
                    display_message_id,
                    &command.creator,
                )
                .await?;
                complete(
                    &mut transaction,
                    &command.organization_id,
                    &command.creator,
                    CREATE_BUNDLE_OPERATION,
                    &command.idempotency_key,
                    IdempotencyResult {
                        resource_type: "bundle",
                        resource_id: bundle_id,
                        response_status: 201,
                    },
                )
                .await?;
                bundle_id
            }
        };
        transaction.commit().await.map_err(map_constraint_error)?;
        self.load_bundle(&command.organization_id, bundle_id, false)
            .await
            .map(|detail| Bundle {
                id: detail.id,
                conversation_id: detail.conversation_id,
                original_message_ids: detail.original_message_ids,
                display_message: detail.display_message,
                created_by: detail.created_by,
                created_at: detail.created_at,
            })
    }

    /// Retrieves an expanded bundle after conversation participant checks.
    ///
    /// # Errors
    ///
    /// Returns not-found for absent, cross-tenant, or non-participant access,
    /// and reports malformed durable data as an internal error.
    pub async fn get_bundle(
        &self,
        organization_id: &OrganizationId,
        actor: &ActorRef,
        conversation_id: Uuid,
        bundle_id: Uuid,
    ) -> AppResult<BundleDetail> {
        self.require_participant(organization_id, actor, conversation_id)
            .await?;
        let detail = self.load_bundle(organization_id, bundle_id, true).await?;
        if detail.conversation_id != conversation_id {
            return Err(AppError::NotFound);
        }
        Ok(detail)
    }

    async fn load_bundle(
        &self,
        organization_id: &OrganizationId,
        bundle_id: Uuid,
        include_originals: bool,
    ) -> AppResult<BundleDetail> {
        let record = sqlx::query_as::<_, BundleRecord>(
            r#"
            SELECT
                bundle.id,
                bundle.conversation_id,
                bundle.created_by_kind::text AS created_by_kind,
                bundle.created_by_id,
                bundle.created_at,
                display.message_id AS display_message_id
            FROM dm.message_bundles AS bundle
            JOIN dm.message_bundle_items AS display
              ON display.bundle_id = bundle.id
             AND display.role = 'display'
            WHERE bundle.id = $1 AND bundle.organization_id = $2
            "#,
        )
        .bind(bundle_id)
        .bind(organization_id.as_str())
        .fetch_optional(self.pool())
        .await?
        .ok_or(AppError::NotFound)?;
        let member_records = sqlx::query_as::<_, BundleMemberRecord>(
            r#"
            SELECT message_id
            FROM dm.message_bundle_items
            WHERE bundle_id = $1 AND role = 'member'
            ORDER BY position
            "#,
        )
        .bind(bundle_id)
        .fetch_all(self.pool())
        .await?;
        let original_message_ids = member_records
            .into_iter()
            .map(|member| member.message_id)
            .collect::<Vec<_>>();
        let display_message = self.load_message(record.display_message_id).await?;
        let original_messages = if include_originals {
            self.load_messages(&original_message_ids).await?
        } else {
            Vec::new()
        };
        Ok(BundleDetail {
            id: record.id,
            conversation_id: record.conversation_id,
            original_message_ids,
            display_message,
            created_by: ActorRef {
                actor_type: parse_actor_type(&record.created_by_kind)?,
                id: parse_actor_id(&record.created_by_id)?,
            },
            created_at: record.created_at,
            original_messages,
        })
    }
}

async fn lock_bundle_members_in(
    transaction: &mut Transaction<'_, Postgres>,
    organization_id: &OrganizationId,
    conversation_id: Uuid,
    message_ids: &[Uuid],
) -> AppResult<()> {
    let available = sqlx::query_scalar::<_, Uuid>(
        r#"
        SELECT message.id
        FROM dm.messages AS message
        WHERE message.id = ANY($1)
          AND message.conversation_id = $2
          AND message.organization_id = $3
          AND NOT EXISTS (
              SELECT 1
              FROM dm.message_bundle_items AS item
              WHERE item.message_id = message.id
          )
        ORDER BY message.id
        FOR UPDATE
        "#,
    )
    .bind(message_ids)
    .bind(conversation_id)
    .bind(organization_id.as_str())
    .fetch_all(&mut **transaction)
    .await?;
    let available = available.into_iter().collect::<HashSet<_>>();
    if available.len() != message_ids.len()
        || message_ids
            .iter()
            .any(|message_id| !available.contains(message_id))
    {
        return Err(AppError::conflict(
            "one or more bundle messages are unavailable or already bundled",
        ));
    }
    Ok(())
}

async fn insert_bundle_item_in(
    transaction: &mut Transaction<'_, Postgres>,
    bundle_id: Uuid,
    conversation_id: Uuid,
    organization_id: &OrganizationId,
    message_id: Uuid,
    role: &str,
    position: i16,
) -> AppResult<()> {
    sqlx::query(
        r#"
        INSERT INTO dm.message_bundle_items (
            bundle_id,
            conversation_id,
            organization_id,
            message_id,
            role,
            position
        )
        VALUES ($1, $2, $3, $4, $5::text::dm.bundle_role, $6)
        "#,
    )
    .bind(bundle_id)
    .bind(conversation_id)
    .bind(organization_id.as_str())
    .bind(message_id)
    .bind(role)
    .bind(position)
    .execute(&mut **transaction)
    .await
    .map_err(map_constraint_error)?;
    Ok(())
}
