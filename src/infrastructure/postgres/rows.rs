//! Internal database row mappings and domain hydration.

use std::{collections::HashMap, str::FromStr as _};

use sqlx::FromRow;
use time::OffsetDateTime;
use url::Url;
use uuid::Uuid;

use crate::{
    AppError, AppResult,
    domain::{
        ActorId, ActorRef, ActorType, Attachment, BundleRef, BundleRole, Gif, Message,
        MessageStatus,
    },
};

use super::PostgresStore;

#[derive(Debug, FromRow)]
pub(crate) struct MessageRecord {
    pub id: Uuid,
    pub conversation_id: Uuid,
    pub sender_kind: String,
    pub sender_id: String,
    pub sequence: i64,
    pub status: String,
    pub text_content: Option<String>,
    pub voice_transcript: Option<String>,
    pub failure_reason: Option<String>,
    pub created_at: OffsetDateTime,
    pub delivered_at: Option<OffsetDateTime>,
    pub read_at: Option<OffsetDateTime>,
}

#[derive(Debug, FromRow)]
struct AttachmentRecord {
    message_id: Uuid,
    attachment_kind: String,
    permanent_url: String,
    name: Option<String>,
    content_type: Option<String>,
    declared_size_bytes: Option<i64>,
}

#[derive(Debug, FromRow)]
struct GifRecord {
    message_id: Uuid,
    provider_id: String,
    url: String,
    preview_url: Option<String>,
    title: Option<String>,
}

#[derive(Debug, FromRow)]
struct BundleReferenceRecord {
    message_id: Uuid,
    bundle_id: Uuid,
    role: String,
}

impl PostgresStore {
    pub(crate) async fn load_message(&self, message_id: Uuid) -> AppResult<Message> {
        let messages = self.load_messages(&[message_id]).await?;
        messages.into_iter().next().ok_or(AppError::NotFound)
    }

    pub(crate) async fn load_messages(&self, message_ids: &[Uuid]) -> AppResult<Vec<Message>> {
        if message_ids.is_empty() {
            return Ok(Vec::new());
        }
        let records = sqlx::query_as::<_, MessageRecord>(
            r#"
            SELECT
                id,
                conversation_id,
                sender_kind::text AS sender_kind,
                sender_id,
                sequence,
                status::text AS status,
                text_content,
                voice_transcript,
                failure_reason,
                created_at,
                delivered_at,
                read_at
            FROM dm.messages
            WHERE id = ANY($1)
            "#,
        )
        .bind(message_ids)
        .fetch_all(self.pool())
        .await?;
        self.hydrate_messages(records, message_ids).await
    }

    pub(crate) async fn hydrate_messages(
        &self,
        records: Vec<MessageRecord>,
        order: &[Uuid],
    ) -> AppResult<Vec<Message>> {
        if records.is_empty() {
            return Ok(Vec::new());
        }
        let ids = records.iter().map(|record| record.id).collect::<Vec<_>>();
        let attachments = sqlx::query_as::<_, AttachmentRecord>(
            r#"
            SELECT
                message_id,
                attachment_kind::text AS attachment_kind,
                permanent_url,
                name,
                content_type,
                declared_size_bytes
            FROM dm.message_attachments
            WHERE message_id = ANY($1)
            ORDER BY message_id, position
            "#,
        )
        .bind(&ids)
        .fetch_all(self.pool())
        .await?;
        let gifs = sqlx::query_as::<_, GifRecord>(
            r#"
            SELECT message_id, provider_id, url, preview_url, title
            FROM dm.message_gifs
            WHERE message_id = ANY($1)
            "#,
        )
        .bind(&ids)
        .fetch_all(self.pool())
        .await?;
        let bundle_references = sqlx::query_as::<_, BundleReferenceRecord>(
            r#"
            SELECT message_id, bundle_id, role::text AS role
            FROM dm.message_bundle_items
            WHERE message_id = ANY($1)
            "#,
        )
        .bind(&ids)
        .fetch_all(self.pool())
        .await?;

        let mut attachments_by_message: HashMap<Uuid, Vec<AttachmentRecord>> = HashMap::new();
        for attachment in attachments {
            attachments_by_message
                .entry(attachment.message_id)
                .or_default()
                .push(attachment);
        }
        let gifs_by_message = gifs
            .into_iter()
            .map(|gif| (gif.message_id, gif))
            .collect::<HashMap<_, _>>();
        let bundles_by_message = bundle_references
            .into_iter()
            .map(|bundle| (bundle.message_id, bundle))
            .collect::<HashMap<_, _>>();
        let mut records_by_id = records
            .into_iter()
            .map(|record| (record.id, record))
            .collect::<HashMap<_, _>>();
        let mut messages = Vec::with_capacity(records_by_id.len());

        for message_id in order {
            let Some(record) = records_by_id.remove(message_id) else {
                continue;
            };
            let attachment_records = attachments_by_message
                .remove(message_id)
                .unwrap_or_default();
            let gif = gifs_by_message.get(message_id).map(map_gif).transpose()?;
            let bundle = bundles_by_message
                .get(message_id)
                .map(map_bundle_reference)
                .transpose()?;
            messages.push(map_message(record, attachment_records, gif, bundle)?);
        }
        Ok(messages)
    }
}

fn map_message(
    record: MessageRecord,
    attachment_records: Vec<AttachmentRecord>,
    gif: Option<Gif>,
    bundle: Option<BundleRef>,
) -> AppResult<Message> {
    let mut attachments = Vec::new();
    let mut voice = None;
    for record in attachment_records {
        let is_voice = record.attachment_kind == "voice";
        let attachment = map_attachment(record)?;
        if is_voice {
            voice = Some(attachment);
        } else {
            attachments.push(attachment);
        }
    }
    Ok(Message {
        id: record.id,
        conversation_id: record.conversation_id,
        sender: ActorRef {
            actor_type: parse_actor_type(&record.sender_kind)?,
            id: parse_actor_id(&record.sender_id)?,
        },
        sequence: record.sequence,
        status: parse_message_status(&record.status)?,
        text: record.text_content,
        attachments,
        voice,
        voice_transcript: record.voice_transcript,
        gif,
        bundle,
        created_at: record.created_at,
        delivered_at: record.delivered_at,
        read_at: record.read_at,
        failure_reason: record.failure_reason,
    })
}

fn map_attachment(record: AttachmentRecord) -> AppResult<Attachment> {
    let size = record
        .declared_size_bytes
        .map(u64::try_from)
        .transpose()
        .map_err(|error| data_error("attachment size", error))?;
    Ok(Attachment {
        permanent_url: parse_url(&record.permanent_url, "attachment permanent URL")?,
        name: record.name,
        content_type: record.content_type,
        size,
    })
}

fn map_gif(record: &GifRecord) -> AppResult<Gif> {
    Ok(Gif {
        provider_id: record.provider_id.clone(),
        url: parse_url(&record.url, "GIF URL")?,
        preview_url: record
            .preview_url
            .as_deref()
            .map(|value| parse_url(value, "GIF preview URL"))
            .transpose()?,
        title: record.title.clone(),
    })
}

fn map_bundle_reference(record: &BundleReferenceRecord) -> AppResult<BundleRef> {
    let role = match record.role.as_str() {
        "display" => BundleRole::Display,
        "member" => BundleRole::Member,
        value => return Err(data_error("bundle role", value)),
    };
    Ok(BundleRef {
        id: record.bundle_id,
        role,
    })
}

pub(crate) fn parse_actor_type(value: &str) -> AppResult<ActorType> {
    ActorType::from_str(value).map_err(|error| data_error("actor kind", error))
}

pub(crate) fn parse_actor_id(value: &str) -> AppResult<ActorId> {
    ActorId::from_str(value).map_err(|error| data_error("actor ID", error))
}

pub(crate) fn parse_message_status(value: &str) -> AppResult<MessageStatus> {
    match value {
        "waiting" => Ok(MessageStatus::Waiting),
        "sent" => Ok(MessageStatus::Sent),
        "delivered" => Ok(MessageStatus::Delivered),
        "read" => Ok(MessageStatus::Read),
        "failed" => Ok(MessageStatus::Failed),
        value => Err(data_error("message status", value)),
    }
}

pub(crate) fn parse_url(value: &str, field: &'static str) -> AppResult<Url> {
    Url::parse(value).map_err(|error| data_error(field, error))
}

pub(crate) fn data_error(field: &'static str, error: impl std::fmt::Display) -> AppError {
    AppError::internal(anyhow::anyhow!(
        "invalid {field} loaded from DM database: {error}"
    ))
}
