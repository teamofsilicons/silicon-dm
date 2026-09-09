//! Internal database row mappings and domain hydration.

use std::{collections::HashMap, str::FromStr as _};

use sqlx::FromRow;
use time::OffsetDateTime;
use url::Url;
use uuid::Uuid;

use crate::{
    AppError, AppResult,
    domain::{
        ActorId, ActorRef, ActorType, Attachment, BundleRef, BundleRole, Gif,
        MAX_VOICE_DURATION_MILLISECONDS, Message, MessageCreate, MessageStatus, VoiceAttachment,
    },
};

use super::PostgresStore;

/// Byte budget for a materialized history/replay page. A single larger
/// message is always allowed, so every valid message remains retrievable.
pub(crate) const MESSAGE_PAGE_BYTE_BUDGET: usize = 16 * 1024 * 1024;

/// Counts the full wire payload without allocating a second JSON buffer.
pub(crate) fn message_payload_bytes(message: &Message) -> AppResult<usize> {
    struct ByteCounter(usize);
    impl std::io::Write for ByteCounter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.saturating_add(bytes.len());
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = ByteCounter(0);
    serde_json::to_writer(&mut counter, message).map_err(AppError::internal)?;
    Ok(counter.0)
}

#[derive(Debug, FromRow)]
pub(crate) struct MessageRecord {
    pub id: Uuid,
    pub conversation_id: Uuid,
    pub sender_kind: String,
    pub sender_id: String,
    pub sender_address: Option<String>,
    pub recipient_address: Option<String>,
    pub sequence: i64,
    pub status: String,
    pub text_content: Option<String>,
    pub metadata: sqlx::types::Json<serde_json::Map<String, serde_json::Value>>,
    pub reply_to_message_id: Option<Uuid>,
    pub voice_transcript: Option<String>,
    pub failure_reason: Option<String>,
    pub created_at: OffsetDateTime,
    pub delivered_at: Option<OffsetDateTime>,
    pub read_at: Option<OffsetDateTime>,
}

#[derive(Debug, FromRow)]
struct MessageRevisionRecord {
    message_id: Uuid,
    version: i64,
    content: Option<sqlx::types::Json<MessageCreate>>,
    deleted_at: Option<OffsetDateTime>,
}

#[derive(Debug, FromRow)]
struct AttachmentRecord {
    message_id: Uuid,
    attachment_kind: String,
    permanent_url: String,
    name: Option<String>,
    content_type: Option<String>,
    declared_size_bytes: Option<i64>,
    duration_milliseconds: Option<i64>,
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
                sender_address,
                recipient_address,
                sequence,
                status::text AS status,
                text_content,
                metadata,
                reply_to_message_id,
                voice_transcript,
                failure_reason,
                created_at,
                delivered_at,
                read_at
            FROM messages
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
                declared_size_bytes,
                duration_milliseconds
            FROM message_attachments
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
            FROM message_gifs
            WHERE message_id = ANY($1)
            "#,
        )
        .bind(&ids)
        .fetch_all(self.pool())
        .await?;
        let bundle_references = sqlx::query_as::<_, BundleReferenceRecord>(
            r#"
            SELECT message_id, bundle_id, role::text AS role
            FROM message_bundle_items
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
        self.apply_latest_revisions(&ids, &mut messages).await?;
        Ok(messages)
    }

    async fn apply_latest_revisions(
        &self,
        ids: &[Uuid],
        messages: &mut [Message],
    ) -> AppResult<()> {
        let revisions = sqlx::query_as::<_, MessageRevisionRecord>(
            "SELECT DISTINCT ON (message_id) message_id, version, content, deleted_at FROM message_revisions WHERE message_id = ANY($1) ORDER BY message_id, version DESC"
        ).bind(ids).fetch_all(self.pool()).await?;
        let mut revisions = revisions
            .into_iter()
            .map(|row| (row.message_id, row))
            .collect::<HashMap<_, _>>();
        for message in messages {
            if let Some(revision) = revisions.remove(&message.id) {
                message.version = revision.version;
                message.deleted_at = revision.deleted_at;
                let content = revision
                    .content
                    .map_or_else(MessageCreate::default, |content| content.0);
                message.text = content.text;
                message.attachments = content.attachments;
                message.voice = content.voice;
                message.voice_transcript = content.voice_transcript;
                message.gif = content.gif;
                message.metadata = content.metadata;
                message.reply_to_message_id = content.reply_to_message_id;
            }
        }
        Ok(())
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
        match record.attachment_kind.as_str() {
            "attachment" => attachments.push(map_attachment(record)?),
            "voice" => voice = Some(map_voice_attachment(record)?),
            value => return Err(data_error("attachment kind", value)),
        }
    }
    Ok(Message {
        version: 1,
        deleted_at: None,
        id: record.id,
        conversation_id: record.conversation_id,
        sender: ActorRef {
            actor_type: parse_actor_type(&record.sender_kind)?,
            id: parse_actor_id(&record.sender_id)?,
        },
        sender_id: record
            .sender_address
            .as_deref()
            .map(parse_actor_id)
            .transpose()?,
        recipient_id: record
            .recipient_address
            .as_deref()
            .map(parse_actor_id)
            .transpose()?,
        sequence: record.sequence,
        status: parse_message_status(&record.status)?,
        text: record.text_content,
        metadata: record.metadata.0,
        reply_to_message_id: record.reply_to_message_id,
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
    if record.duration_milliseconds.is_some() {
        return Err(data_error(
            "attachment duration",
            "non-voice attachment has a duration",
        ));
    }
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

fn map_voice_attachment(record: AttachmentRecord) -> AppResult<VoiceAttachment> {
    let size = record
        .declared_size_bytes
        .map(u64::try_from)
        .transpose()
        .map_err(|error| data_error("voice attachment size", error))?;
    let duration_milliseconds = record
        .duration_milliseconds
        .map(u64::try_from)
        .transpose()
        .map_err(|error| data_error("voice duration", error))?;
    if duration_milliseconds
        .is_some_and(|duration| !(1..=MAX_VOICE_DURATION_MILLISECONDS).contains(&duration))
    {
        return Err(data_error(
            "voice duration",
            "duration is outside the supported range",
        ));
    }
    Ok(VoiceAttachment {
        permanent_url: parse_url(&record.permanent_url, "voice attachment permanent URL")?,
        name: record.name,
        content_type: record.content_type,
        size,
        duration_milliseconds,
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
