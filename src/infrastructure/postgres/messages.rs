//! Message, receipt, and GIF-history persistence.

use serde::Serialize;
use sqlx::{FromRow, Postgres, Transaction};
use url::Url;
use uuid::Uuid;

use crate::{
    AppError, AppResult,
    application::commands::{RecordReceiptCommand, SendMessageCommand},
    domain::{
        ActorRef, ActorType, Cursor, Gif, GifPage, Message, MessageCreate, MessagePage,
        MessageStatus, OrganizationId, PageRequest, ReceiptStatus,
    },
};

use super::{
    PostgresStore,
    directory::refresh_directory_in,
    idempotency::{IdempotencyClaim, IdempotencyResult, claim, complete, request_hash},
    map_constraint_error,
    rows::{
        MESSAGE_PAGE_BYTE_BUDGET, message_payload_bytes, parse_actor_id, parse_actor_type,
        parse_message_status, parse_url,
    },
};

const SEND_MESSAGE_OPERATION: &str = "messages.create";

#[derive(Serialize)]
struct MessageIdempotencyContent<'a> {
    conversation_id: Uuid,
    content_hash: &'a [u8],
}

#[derive(FromRow)]
struct ParticipantRecord {
    actor_kind: String,
    actor_id: String,
}

#[derive(FromRow)]
struct ReceiptRecord {
    id: Uuid,
}

#[derive(FromRow)]
struct MessageSenderRecord {
    sender_kind: String,
    sender_id: String,
    status: String,
}

#[derive(FromRow)]
struct RecentGifRecord {
    provider_id: String,
    url: String,
    preview_url: Option<String>,
    title: Option<String>,
}

struct MessageAttachmentInsert<'a> {
    message_id: Uuid,
    conversation_id: Uuid,
    organization_id: &'a OrganizationId,
    position: usize,
    kind: &'a str,
    permanent_url: &'a Url,
    name: Option<&'a str>,
    content_type: Option<&'a str>,
    size: Option<u64>,
    duration_milliseconds: Option<u64>,
}

impl PostgresStore {
    /// Lists stable conversation history in newest-first sequence order.
    ///
    /// # Errors
    ///
    /// Returns not found for non-participants and validation errors for an
    /// incompatible cursor or limit.
    pub async fn list_messages(
        &self,
        organization_id: &OrganizationId,
        actor: &ActorRef,
        conversation_id: Uuid,
        page: &PageRequest,
        include_bundled_members: bool,
    ) -> AppResult<MessagePage> {
        self.require_participant(organization_id, actor, conversation_id)
            .await?;
        let limit = page.validated_limit()?;
        let cursor = page
            .cursor
            .as_deref()
            .map(|value| Cursor::decode(value, "messages"))
            .transpose()?;
        if cursor
            .as_ref()
            .is_some_and(|value| value.sequence().is_none())
        {
            return Err(AppError::validation(
                "cursor is invalid for message pagination",
            ));
        }
        let before_sequence = cursor.as_ref().and_then(Cursor::sequence);
        // Fetch only positions before choosing a byte-bounded payload page.
        // A lookahead row must never hydrate another maximum-size message.
        let ids = sqlx::query_scalar::<_, Uuid>(
            r#"
            SELECT message.id
            FROM messages AS message
            WHERE message.conversation_id = $1
              AND message.organization_id = $2
              AND ($3::bigint IS NULL OR message.sequence < $3)
              AND (
                    $4
                    OR NOT EXISTS (
                        SELECT 1
                        FROM message_bundle_items AS bundle_item
                        WHERE bundle_item.message_id = message.id
                          AND bundle_item.role = 'member'
                    )
              )
            ORDER BY message.sequence DESC
            LIMIT $5
            "#,
        )
        .bind(conversation_id)
        .bind(organization_id.as_str())
        .bind(before_sequence)
        .bind(include_bundled_members)
        .bind(i64::from(limit) + 1)
        .fetch_all(self.pool())
        .await?;
        let mut items = Vec::with_capacity(usize::from(limit).min(ids.len()));
        let mut payload_bytes: usize = 0;
        for id in ids.iter().take(usize::from(limit)) {
            let message = self.load_message(*id).await?;
            let bytes = message_payload_bytes(&message)?;
            if !items.is_empty() && payload_bytes.saturating_add(bytes) > MESSAGE_PAGE_BYTE_BUDGET {
                break;
            }
            payload_bytes = payload_bytes.saturating_add(bytes);
            items.push(message);
            if payload_bytes >= MESSAGE_PAGE_BYTE_BUDGET {
                break;
            }
        }
        let has_next = items.len() < ids.len();
        let next_cursor = if has_next {
            items
                .last()
                .map(|message| Cursor::new("messages", message.sequence).encode())
                .transpose()?
        } else {
            None
        };
        Ok(MessagePage { items, next_cursor })
    }

    /// Durably accepts a message, assigns its sequence, and enqueues all
    /// recipient deliveries in one transaction.
    ///
    /// # Errors
    ///
    /// Rejects invalid content, sender mismatch, non-participant access, and
    /// conflicting idempotency reuse.
    #[allow(
        clippy::too_many_lines,
        reason = "message acceptance, outbox creation, history update, draft clear, and idempotency form one transaction"
    )]
    pub async fn send_message(&self, command: SendMessageCommand) -> AppResult<Message> {
        command.content.validate().map_err(AppError::validation)?;
        if command
            .content
            .sender_id
            .as_ref()
            .is_some_and(|sender_id| *sender_id != command.sender.id)
        {
            return Err(AppError::Forbidden);
        }
        let content_hash = command.content.content_digest();
        let legacy_content_hash = command.content.legacy_content_digest();
        let hash = request_hash(&MessageIdempotencyContent {
            conversation_id: command.conversation_id,
            content_hash: content_hash.as_bytes(),
        })?;
        let mut transaction = self.pool().begin().await?;
        refresh_directory_in(
            &mut transaction,
            &command.organization_id,
            std::slice::from_ref(&command.sender),
        )
        .await?;
        require_participant_in(
            &mut transaction,
            &command.organization_id,
            &command.sender,
            command.conversation_id,
        )
        .await?;
        let claim = claim(
            &mut transaction,
            &command.organization_id,
            &command.sender,
            SEND_MESSAGE_OPERATION,
            &command.idempotency_key,
            &hash,
        )
        .await?;
        let message_id = match claim {
            IdempotencyClaim::Replay(resource_id) => resource_id,
            IdempotencyClaim::Acquired => {
                let message_id = insert_message_in(
                    &mut transaction,
                    &command.organization_id,
                    command.conversation_id,
                    &command.sender,
                    &command.content,
                    &content_hash,
                )
                .await?;
                enqueue_message_deliveries_in(
                    &mut transaction,
                    &command.organization_id,
                    command.conversation_id,
                    message_id,
                    &command.sender,
                )
                .await?;
                update_recent_gif_in(
                    &mut transaction,
                    &command.organization_id,
                    &command.sender,
                    message_id,
                    command.content.gif.as_ref(),
                )
                .await?;
                let matching_draft = sqlx::query(
                    r#"
                    UPDATE drafts
                    SET version = version + 1,
                        content_hash_version = 3
                    WHERE conversation_id = $1
                      AND organization_id = $2
                      AND actor_kind = $3::text::actor_kind
                      AND actor_id = $4
                      AND (
                            (content_hash_version IN (2, 3) AND content_hash = $5)
                            OR (content_hash_version = 1 AND content_hash = $6)
                      )
                    "#,
                )
                .bind(command.conversation_id)
                .bind(command.organization_id.as_str())
                .bind(command.sender.actor_type.as_str())
                .bind(command.sender.id.as_str())
                .bind(content_hash.as_bytes().as_slice())
                .bind(
                    if command.content.metadata.is_empty()
                        && command.content.reply_to_message_id.is_none()
                    {
                        Some(legacy_content_hash.as_bytes().as_slice())
                    } else {
                        None
                    },
                )
                .execute(&mut *transaction)
                .await
                .map_err(map_constraint_error)?;
                if matching_draft.rows_affected() == 1 {
                    sqlx::query(
                        r#"
                        DELETE FROM draft_attachments
                        WHERE conversation_id = $1
                          AND organization_id = $2
                          AND actor_kind = $3::text::actor_kind
                          AND actor_id = $4
                        "#,
                    )
                    .bind(command.conversation_id)
                    .bind(command.organization_id.as_str())
                    .bind(command.sender.actor_type.as_str())
                    .bind(command.sender.id.as_str())
                    .execute(&mut *transaction)
                    .await
                    .map_err(map_constraint_error)?;
                    sqlx::query(
                        r#"
                        DELETE FROM draft_gifs
                        WHERE conversation_id = $1
                          AND organization_id = $2
                          AND actor_kind = $3::text::actor_kind
                          AND actor_id = $4
                        "#,
                    )
                    .bind(command.conversation_id)
                    .bind(command.organization_id.as_str())
                    .bind(command.sender.actor_type.as_str())
                    .bind(command.sender.id.as_str())
                    .execute(&mut *transaction)
                    .await
                    .map_err(map_constraint_error)?;
                    sqlx::query(
                        r#"
                        DELETE FROM drafts
                        WHERE conversation_id = $1
                          AND organization_id = $2
                          AND actor_kind = $3::text::actor_kind
                          AND actor_id = $4
                        "#,
                    )
                    .bind(command.conversation_id)
                    .bind(command.organization_id.as_str())
                    .bind(command.sender.actor_type.as_str())
                    .bind(command.sender.id.as_str())
                    .execute(&mut *transaction)
                    .await
                    .map_err(map_constraint_error)?;
                }
                complete(
                    &mut transaction,
                    &command.organization_id,
                    &command.sender,
                    SEND_MESSAGE_OPERATION,
                    &command.idempotency_key,
                    IdempotencyResult {
                        resource_type: "message",
                        resource_id: message_id,
                        response_status: 202,
                    },
                )
                .await?;
                message_id
            }
        };
        transaction.commit().await.map_err(map_constraint_error)?;
        drop(command);
        self.load_message(message_id).await
    }

    /// Records an idempotent monotonic device receipt and durably notifies the
    /// original sender of a newly reached state.
    ///
    /// # Errors
    ///
    /// Rejects malformed device IDs, sender self-receipts, non-participant
    /// access, missing messages, and durable invariant violations.
    #[allow(
        clippy::too_many_lines,
        reason = "device receipt normalization and aggregate-status outbox transition must be inspected atomically"
    )]
    pub async fn record_receipt(&self, command: RecordReceiptCommand) -> AppResult<Message> {
        let message_id = command.message_id;
        self.record_receipt_without_payload(command).await?;
        self.load_message(message_id).await
    }

    /// Records a receipt when the caller only needs the acknowledged position.
    ///
    /// # Errors
    /// Returns the same validation and storage failures as `record_receipt`.
    #[allow(
        clippy::too_many_lines,
        reason = "receipt persistence and aggregate-status transition are atomic"
    )]
    pub async fn record_receipt_without_payload(
        &self,
        command: RecordReceiptCommand,
    ) -> AppResult<()> {
        validate_device_id(&command.device_id)?;
        let mut transaction = self.pool().begin().await?;
        refresh_directory_in(
            &mut transaction,
            &command.organization_id,
            std::slice::from_ref(&command.recipient),
        )
        .await?;
        require_participant_in(
            &mut transaction,
            &command.organization_id,
            &command.recipient,
            command.conversation_id,
        )
        .await?;
        let message = sqlx::query_as::<_, MessageSenderRecord>(
            r#"
            SELECT
                sender_kind::text AS sender_kind,
                sender_id,
                status::text AS status
            FROM messages
            WHERE id = $1 AND conversation_id = $2 AND organization_id = $3
            FOR UPDATE
            "#,
        )
        .bind(command.message_id)
        .bind(command.conversation_id)
        .bind(command.organization_id.as_str())
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(AppError::NotFound)?;
        let previous_aggregate_status = parse_message_status(&message.status)?;
        let sender = ActorRef {
            actor_type: parse_actor_type(&message.sender_kind)?,
            id: parse_actor_id(&message.sender_id)?,
        };
        if sender == command.recipient {
            return Err(AppError::validation(
                "a message sender cannot acknowledge their own message",
            ));
        }
        let existing = sqlx::query_as::<_, ReceiptRecord>(
            r#"
            SELECT id
            FROM message_receipts
            WHERE message_id = $1
              AND recipient_kind = $2::text::actor_kind
              AND recipient_id = $3
              AND device_id = $4
            FOR UPDATE
            "#,
        )
        .bind(command.message_id)
        .bind(command.recipient.actor_type.as_str())
        .bind(command.recipient.id.as_str())
        .bind(&command.device_id)
        .fetch_optional(&mut *transaction)
        .await?;
        let requested_status = receipt_status_name(command.status);
        let receipt_id = existing
            .as_ref()
            .map_or_else(Uuid::now_v7, |record| record.id);
        sqlx::query(
            r#"
            INSERT INTO message_receipts (
                id,
                message_id,
                conversation_id,
                organization_id,
                recipient_kind,
                recipient_id,
                device_id,
                status,
                read_at
            )
            VALUES (
                $1, $2, $3, $4, $5::text::actor_kind, $6, $7,
                $8::text::receipt_status,
                CASE WHEN $8 = 'read' THEN clock_timestamp() ELSE NULL END
            )
            ON CONFLICT (message_id, recipient_kind, recipient_id, device_id) DO UPDATE
            SET status = CASE
                    WHEN message_receipts.status = 'read' THEN 'read'
                    ELSE EXCLUDED.status
                END,
                read_at = CASE
                    WHEN message_receipts.status = 'read' THEN message_receipts.read_at
                    WHEN EXCLUDED.status = 'read' THEN GREATEST(
                        message_receipts.delivered_at,
                        clock_timestamp()
                    )
                    ELSE NULL
                END
            "#,
        )
        .bind(receipt_id)
        .bind(command.message_id)
        .bind(command.conversation_id)
        .bind(command.organization_id.as_str())
        .bind(command.recipient.actor_type.as_str())
        .bind(command.recipient.id.as_str())
        .bind(&command.device_id)
        .bind(requested_status)
        .execute(&mut *transaction)
        .await
        .map_err(map_constraint_error)?;

        let aggregate_status = sqlx::query_scalar::<_, String>(
            r#"
            SELECT status::text
            FROM messages
            WHERE id = $1
            "#,
        )
        .bind(command.message_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(map_constraint_error)
        .and_then(|status| parse_message_status(&status))?;
        if aggregate_status != previous_aggregate_status
            && matches!(
                aggregate_status,
                MessageStatus::Delivered | MessageStatus::Read
            )
        {
            enqueue_message_status_delivery_in(
                &mut transaction,
                &command.organization_id,
                command.conversation_id,
                command.message_id,
                receipt_id,
                aggregate_status,
                &sender,
            )
            .await?;
        }
        transaction.commit().await.map_err(map_constraint_error)
    }

    /// Returns a Carbon's durable 20-item GIF MRU; Silicons have no history.
    ///
    /// # Errors
    ///
    /// Propagates database failures and reports malformed durable GIF data as
    /// an internal error.
    pub async fn recent_gifs(
        &self,
        organization_id: &OrganizationId,
        actor: &ActorRef,
    ) -> AppResult<GifPage> {
        if actor.actor_type == ActorType::Silicon {
            return Ok(GifPage { items: Vec::new() });
        }
        let records = sqlx::query_as::<_, RecentGifRecord>(
            r#"
            SELECT provider_id, url, preview_url, title
            FROM recent_gifs
            WHERE organization_id = $1 AND carbon_id = $2
            ORDER BY last_used_at DESC, provider_id DESC
            LIMIT 20
            "#,
        )
        .bind(organization_id.as_str())
        .bind(actor.id.as_str())
        .fetch_all(self.pool())
        .await?;
        let items = records
            .into_iter()
            .map(|record| {
                Ok(Gif {
                    provider_id: record.provider_id,
                    url: parse_url(&record.url, "recent GIF URL")?,
                    preview_url: record
                        .preview_url
                        .as_deref()
                        .map(|url| parse_url(url, "recent GIF preview URL"))
                        .transpose()?,
                    title: record.title,
                })
            })
            .collect::<AppResult<Vec<_>>>()?;
        Ok(GifPage { items })
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the helper inserts one sealed message aggregate and all content children in a single transaction"
)]
pub(crate) async fn insert_message_in(
    transaction: &mut Transaction<'_, Postgres>,
    organization_id: &OrganizationId,
    conversation_id: Uuid,
    sender: &ActorRef,
    content: &MessageCreate,
    content_hash: &blake3::Hash,
) -> AppResult<Uuid> {
    let message_id = Uuid::now_v7();
    sqlx::query(
        r#"
        INSERT INTO messages (
            id,
            conversation_id,
            organization_id,
            sender_kind,
            sender_id,
            sequence,
            status,
            text_content,
            voice_transcript,
            content_hash_version,
            content_hash,
            metadata,
            reply_to_message_id
        )
        VALUES (
            $1, $2, $3, $4::text::actor_kind, $5, 1, 'sent', $6, $7,
            3, $8, $9, $10
        )
        "#,
    )
    .bind(message_id)
    .bind(conversation_id)
    .bind(organization_id.as_str())
    .bind(sender.actor_type.as_str())
    .bind(sender.id.as_str())
    .bind(&content.text)
    .bind(&content.voice_transcript)
    .bind(content_hash.as_bytes().as_slice())
    .bind(sqlx::types::Json(&content.metadata))
    .bind(content.reply_to_message_id)
    .execute(&mut **transaction)
    .await
    .map_err(map_constraint_error)?;

    for (position, attachment) in content.attachments.iter().enumerate() {
        insert_attachment_in(
            transaction,
            MessageAttachmentInsert {
                message_id,
                conversation_id,
                organization_id,
                position,
                kind: "attachment",
                permanent_url: &attachment.permanent_url,
                name: attachment.name.as_deref(),
                content_type: attachment.content_type.as_deref(),
                size: attachment.size,
                duration_milliseconds: None,
            },
        )
        .await?;
    }
    if let Some(voice) = &content.voice {
        insert_attachment_in(
            transaction,
            MessageAttachmentInsert {
                message_id,
                conversation_id,
                organization_id,
                position: 100,
                kind: "voice",
                permanent_url: &voice.permanent_url,
                name: voice.name.as_deref(),
                content_type: voice.content_type.as_deref(),
                size: voice.size,
                duration_milliseconds: voice.duration_milliseconds,
            },
        )
        .await?;
    }
    if let Some(gif) = &content.gif {
        sqlx::query(
            r#"
            INSERT INTO message_gifs (
                message_id,
                conversation_id,
                organization_id,
                provider_id,
                url,
                preview_url,
                title
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            "#,
        )
        .bind(message_id)
        .bind(conversation_id)
        .bind(organization_id.as_str())
        .bind(&gif.provider_id)
        .bind(gif.url.as_str())
        .bind(gif.preview_url.as_ref().map(url::Url::as_str))
        .bind(&gif.title)
        .execute(&mut **transaction)
        .await
        .map_err(map_constraint_error)?;
    }
    sqlx::query(
        r#"
        UPDATE conversations
        SET updated_at = transaction_timestamp()
        WHERE id = $1 AND organization_id = $2
        "#,
    )
    .bind(conversation_id)
    .bind(organization_id.as_str())
    .execute(&mut **transaction)
    .await?;
    Ok(message_id)
}

pub(crate) async fn enqueue_message_deliveries_in(
    transaction: &mut Transaction<'_, Postgres>,
    organization_id: &OrganizationId,
    conversation_id: Uuid,
    message_id: Uuid,
    _sender: &ActorRef,
) -> AppResult<()> {
    let participants =
        participant_records_in(transaction, organization_id, conversation_id).await?;
    for participant in participants {
        let target = ActorRef {
            actor_type: parse_actor_type(&participant.actor_kind)?,
            id: parse_actor_id(&participant.actor_id)?,
        };
        sqlx::query(
            r#"
            INSERT INTO actor_deliveries (
                id,
                organization_id,
                target_kind,
                target_id,
                sequence,
                delivery_kind,
                conversation_id,
                message_id
            )
            VALUES ($1, $2, $3::text::actor_kind, $4, 1, 'message', $5, $6)
            ON CONFLICT DO NOTHING
            "#,
        )
        .bind(Uuid::now_v7())
        .bind(organization_id.as_str())
        .bind(target.actor_type.as_str())
        .bind(target.id.as_str())
        .bind(conversation_id)
        .bind(message_id)
        .execute(&mut **transaction)
        .await
        .map_err(map_constraint_error)?;
        notify_actor_in(transaction, organization_id, &target).await?;
    }
    Ok(())
}

async fn insert_attachment_in(
    transaction: &mut Transaction<'_, Postgres>,
    input: MessageAttachmentInsert<'_>,
) -> AppResult<()> {
    let position = i16::try_from(input.position)
        .map_err(|_| AppError::validation("attachment position exceeds supported range"))?;
    let size = input
        .size
        .map(i64::try_from)
        .transpose()
        .map_err(|_| AppError::validation("attachment size exceeds supported range"))?;
    let duration = input
        .duration_milliseconds
        .map(i64::try_from)
        .transpose()
        .map_err(|_| AppError::validation("voice duration exceeds supported range"))?;
    sqlx::query(
        r#"
        INSERT INTO message_attachments (
            message_id,
            conversation_id,
            organization_id,
            position,
            attachment_kind,
            permanent_url,
            name,
            content_type,
            declared_size_bytes,
            duration_milliseconds
        )
        VALUES ($1, $2, $3, $4, $5::text::attachment_kind, $6, $7, $8, $9, $10)
        "#,
    )
    .bind(input.message_id)
    .bind(input.conversation_id)
    .bind(input.organization_id.as_str())
    .bind(position)
    .bind(input.kind)
    .bind(input.permanent_url.as_str())
    .bind(input.name)
    .bind(input.content_type)
    .bind(size)
    .bind(duration)
    .execute(&mut **transaction)
    .await
    .map_err(map_constraint_error)?;
    Ok(())
}

async fn update_recent_gif_in(
    transaction: &mut Transaction<'_, Postgres>,
    organization_id: &OrganizationId,
    sender: &ActorRef,
    message_id: Uuid,
    gif: Option<&Gif>,
) -> AppResult<()> {
    let Some(gif) = gif else {
        return Ok(());
    };
    if sender.actor_type != ActorType::Carbon {
        return Ok(());
    }
    sqlx::query(
        r#"
        INSERT INTO recent_gifs (
            organization_id,
            carbon_id,
            provider_id,
            url,
            preview_url,
            title,
            last_message_id
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        ON CONFLICT (organization_id, carbon_id, provider_id) DO UPDATE
        SET url = EXCLUDED.url,
            preview_url = EXCLUDED.preview_url,
            title = EXCLUDED.title,
            last_message_id = EXCLUDED.last_message_id,
            last_used_at = transaction_timestamp()
        "#,
    )
    .bind(organization_id.as_str())
    .bind(sender.id.as_str())
    .bind(&gif.provider_id)
    .bind(gif.url.as_str())
    .bind(gif.preview_url.as_ref().map(url::Url::as_str))
    .bind(&gif.title)
    .bind(message_id)
    .execute(&mut **transaction)
    .await
    .map_err(map_constraint_error)?;
    Ok(())
}

async fn enqueue_message_status_delivery_in(
    transaction: &mut Transaction<'_, Postgres>,
    organization_id: &OrganizationId,
    conversation_id: Uuid,
    message_id: Uuid,
    causal_receipt_id: Uuid,
    status: MessageStatus,
    sender: &ActorRef,
) -> AppResult<()> {
    let result = sqlx::query(
        r#"
        INSERT INTO actor_deliveries (
            id,
            organization_id,
            target_kind,
            target_id,
            delivery_kind,
            conversation_id,
            message_id,
            receipt_id,
            aggregate_status
        )
        VALUES (
            $1, $2, $3::text::actor_kind, $4, 'message_status', $5, $6,
            $7, $8::text::message_status
        )
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(organization_id.as_str())
    .bind(sender.actor_type.as_str())
    .bind(sender.id.as_str())
    .bind(conversation_id)
    .bind(message_id)
    .bind(causal_receipt_id)
    .bind(message_status_name(status))
    .execute(&mut **transaction)
    .await
    .map_err(map_constraint_error)?;
    if result.rows_affected() == 1 {
        notify_actor_in(transaction, organization_id, sender).await?;
    }
    Ok(())
}

pub(crate) async fn notify_actor_in(
    transaction: &mut Transaction<'_, Postgres>,
    organization_id: &OrganizationId,
    actor: &ActorRef,
) -> AppResult<()> {
    let payload = serde_json::json!({
        "org_id": organization_id.as_str(),
        "actor_type": actor.actor_type.as_str(),
        "actor_id": actor.id.as_str(),
    })
    .to_string();
    sqlx::query("SELECT pg_notify('dm_delivery', $1)")
        .bind(payload)
        .execute(&mut **transaction)
        .await?;
    Ok(())
}

async fn participant_records_in(
    transaction: &mut Transaction<'_, Postgres>,
    organization_id: &OrganizationId,
    conversation_id: Uuid,
) -> AppResult<Vec<ParticipantRecord>> {
    sqlx::query_as::<_, ParticipantRecord>(
        r#"
        SELECT actor_kind::text AS actor_kind, actor_id
        FROM conversation_participants
        WHERE conversation_id = $1 AND organization_id = $2
        ORDER BY actor_kind, actor_id
        "#,
    )
    .bind(conversation_id)
    .bind(organization_id.as_str())
    .fetch_all(&mut **transaction)
    .await
    .map_err(AppError::Database)
}

pub(crate) async fn require_participant_in(
    transaction: &mut Transaction<'_, Postgres>,
    organization_id: &OrganizationId,
    actor: &ActorRef,
    conversation_id: Uuid,
) -> AppResult<()> {
    let allowed = sqlx::query_scalar::<_, bool>(
        r#"
        SELECT EXISTS (
            SELECT 1
            FROM conversation_participants
            WHERE conversation_id = $1
              AND organization_id = $2
              AND actor_kind = $3::text::actor_kind
              AND actor_id = $4
        )
        "#,
    )
    .bind(conversation_id)
    .bind(organization_id.as_str())
    .bind(actor.actor_type.as_str())
    .bind(actor.id.as_str())
    .fetch_one(&mut **transaction)
    .await?;
    if allowed {
        Ok(())
    } else {
        Err(AppError::NotFound)
    }
}

fn validate_device_id(device_id: &str) -> AppResult<()> {
    if device_id.is_empty() || device_id.len() > 255 || device_id.chars().any(char::is_control) {
        return Err(AppError::validation(
            "device_id must contain 1 to 255 non-control characters",
        ));
    }
    Ok(())
}

pub(crate) const fn receipt_status_name(status: ReceiptStatus) -> &'static str {
    match status {
        ReceiptStatus::Delivered => "delivered",
        ReceiptStatus::Read => "read",
    }
}

const fn message_status_name(status: MessageStatus) -> &'static str {
    match status {
        MessageStatus::Waiting => "waiting",
        MessageStatus::Sent => "sent",
        MessageStatus::Delivered => "delivered",
        MessageStatus::Read => "read",
        MessageStatus::Failed => "failed",
    }
}
