//! Message content, state, and validation.

use serde::{Deserialize, Deserializer, Serialize};
use sqlx::Type;
use time::OffsetDateTime;
use url::Url;
use uuid::Uuid;

use super::{
    ActorRef, BundleRef, MAX_ATTACHMENT_BYTES, MAX_ATTACHMENTS, MAX_TEXT_CHARACTERS,
    MAX_VOICE_DURATION_MILLISECONDS,
};

/// Durable metadata for a generic attachment.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Attachment {
    /// Stable HTTPS resource URL.
    pub permanent_url: Url,
    /// Original display filename.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Declared media type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    /// Declared byte size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

/// Durable metadata for a voice attachment.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct VoiceAttachment {
    /// Stable HTTPS resource URL.
    pub permanent_url: Url,
    /// Original display filename.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Declared media type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    /// Declared byte size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// Client-supplied total duration in milliseconds.
    ///
    /// New writes require this value. `None` is retained only so historical
    /// pre-v2 voice rows with unavailable provider metadata remain readable.
    #[serde(deserialize_with = "deserialize_required_voice_duration")]
    pub duration_milliseconds: Option<u64>,
}

fn deserialize_required_voice_duration<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<u64>::deserialize(deserializer)
}

/// Provider-independent GIF metadata stored with a message.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Gif {
    /// Stable provider identifier.
    pub provider_id: String,
    /// Provider delivery URL.
    pub url: Url,
    /// Optional smaller preview.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_url: Option<Url>,
    /// Optional human-readable title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// Content accepted when creating a message or bundle display message.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct MessageCreate {
    /// Arbitrary caller-owned metadata, preserved on every round trip.
    #[serde(default)]
    pub metadata: serde_json::Map<String, serde_json::Value>,
    /// Message being replied to in the same conversation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to_message_id: Option<Uuid>,
    /// Explicit sender when a connection represents more than one actor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_id: Option<super::ActorId>,
    /// Optional recipient address, including an ISI prefix for a silicon participant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipient_id: Option<super::ActorId>,
    /// Optional text content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Zero or more stable HTTPS attachments.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
    /// Optional stable HTTPS voice attachment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<VoiceAttachment>,
    /// Optional client-provided voice transcript.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_transcript: Option<String>,
    /// Optional GIF.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gif: Option<Gif>,
}

/// Durable aggregate message state.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize, Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "message_status", rename_all = "snake_case")]
pub enum MessageStatus {
    /// Client-side state; normally not persisted by DM.
    Waiting,
    /// Durably accepted by DM.
    Sent,
    /// Every recipient actor has acknowledged delivery on one device.
    Delivered,
    /// Every recipient actor has read the message on one device.
    Read,
    /// Delivery will no longer be retried.
    Failed,
}

/// Receipt states clients may report.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize, Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "receipt_status", rename_all = "snake_case")]
pub enum ReceiptStatus {
    /// The client durably received the message.
    Delivered,
    /// The actor viewed the message. This also implies delivery.
    Read,
}

/// Public durable message representation.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Message {
    /// Content version; edits and deletion advance this independently of receipts.
    #[serde(default = "initial_message_version")]
    pub version: i64,
    /// A deleted message is a content-free tombstone.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub deleted_at: Option<OffsetDateTime>,
    /// Arbitrary caller-owned metadata, preserved on every round trip.
    #[serde(default)]
    pub metadata: serde_json::Map<String, serde_json::Value>,
    /// Message being replied to in the same conversation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to_message_id: Option<Uuid>,
    /// Stable message identifier.
    pub id: Uuid,
    /// Parent conversation.
    pub conversation_id: Uuid,
    /// Typed sender.
    pub sender: ActorRef,
    /// Optional qualified sender address; sender.id always identifies the IAM account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_id: Option<super::ActorId>,
    /// Optional intended recipient address. Conversation visibility is unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipient_id: Option<super::ActorId>,
    /// Stable sequence within the conversation.
    pub sequence: i64,
    /// Aggregate message state.
    pub status: MessageStatus,
    /// Text content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Stable attachment references.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
    /// Stable voice reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<VoiceAttachment>,
    /// Optional client-provided voice transcript.
    #[serde(default)]
    pub voice_transcript: Option<String>,
    /// GIF content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gif: Option<Gif>,
    /// Bundle membership, when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle: Option<BundleRef>,
    /// Creation timestamp assigned by DM.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// First time aggregate delivery was reached.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub delivered_at: Option<OffsetDateTime>,
    /// First time aggregate read was reached.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub read_at: Option<OffsetDateTime>,
    /// Stable operator-facing failure category, never a secret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
}

const fn initial_message_version() -> i64 {
    1
}

/// Cursor-paginated messages.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct MessagePage {
    /// Current page.
    pub items: Vec<Message>,
    /// Opaque cursor for the next page.
    pub next_cursor: Option<String>,
}

/// A GIF result page.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct GifPage {
    /// Provider results.
    pub items: Vec<Gif>,
}

impl MessageCreate {
    /// Hashes ISI routing separately from draft content, preserving legacy hashes when absent.
    #[must_use]
    pub fn routing_digest(&self) -> Option<blake3::Hash> {
        let sender = self
            .sender_id
            .as_ref()
            .filter(|id| id.address_parts().is_ok_and(|(_, isi)| isi.is_some()));
        if sender.is_none() && self.recipient_id.is_none() {
            return None;
        }
        Some(blake3::hash(
            serde_json::to_string(&(sender, &self.recipient_id))
                .unwrap_or_default()
                .as_bytes(),
        ))
    }

    /// Checks content-shape and documented resource limits.
    ///
    /// # Errors
    ///
    /// Returns a stable user-facing validation message.
    pub fn validate(&self) -> Result<(), &'static str> {
        for address in [&self.sender_id, &self.recipient_id].into_iter().flatten() {
            address.address_parts()?;
        }
        if self.text.as_ref().is_some_and(String::is_empty) {
            return Err("message text must contain at least one character");
        }
        let has_content = self.text.is_some()
            || !self.attachments.is_empty()
            || self.voice.is_some()
            || self.gif.is_some();
        if !has_content {
            return Err("a message must contain text, an attachment, voice, or a GIF");
        }
        if self
            .text
            .as_ref()
            .is_some_and(|text| text.chars().count() > MAX_TEXT_CHARACTERS)
        {
            return Err("message text exceeds 100,000,000 characters");
        }
        if self.voice.is_none() && self.voice_transcript.is_some() {
            return Err("voice_transcript requires a voice attachment");
        }
        if self
            .voice_transcript
            .as_ref()
            .is_some_and(|text| text.chars().count() > MAX_TEXT_CHARACTERS)
        {
            return Err("voice transcript exceeds 100,000,000 characters");
        }
        validate_attachments(&self.attachments, self.voice.as_ref())?;
        validate_gif(self.gif.as_ref())
    }

    /// Returns a canonical content digest used for safe draft clearing.
    ///
    /// `sender_id` is excluded because it is routing metadata, not draft
    /// content. Serialization failure is impossible for these in-memory domain
    /// values; a stable sentinel still avoids a panic if the shape changes.
    #[must_use]
    pub fn content_digest(&self) -> blake3::Hash {
        content_digest(
            self.text.as_deref(),
            &self.attachments,
            self.voice.as_ref(),
            self.voice_transcript.as_deref(),
            self.gif.as_ref(),
            &self.metadata,
            self.reply_to_message_id,
        )
    }

    /// Returns the v1 digest used by drafts written before voice duration and
    /// client-owned transcripts became part of canonical content.
    #[must_use]
    pub(crate) fn legacy_content_digest(&self) -> blake3::Hash {
        legacy_content_digest(
            self.text.as_deref(),
            &self.attachments,
            self.voice.as_ref(),
            self.gif.as_ref(),
        )
    }
}

/// Hashes canonical user-visible content without cloning potentially large
/// message or draft fields.
pub(crate) fn content_digest(
    text: Option<&str>,
    attachments: &[Attachment],
    voice: Option<&VoiceAttachment>,
    voice_transcript: Option<&str>,
    gif: Option<&Gif>,
    metadata: &serde_json::Map<String, serde_json::Value>,
    reply_to_message_id: Option<Uuid>,
) -> blake3::Hash {
    #[derive(Serialize)]
    struct Canonical<'a> {
        text: Option<&'a str>,
        attachments: &'a [Attachment],
        voice: Option<&'a VoiceAttachment>,
        #[serde(skip_serializing_if = "Option::is_none")]
        voice_transcript: Option<&'a str>,
        gif: Option<&'a Gif>,
        #[serde(skip_serializing_if = "serde_json::Map::is_empty")]
        metadata: &'a serde_json::Map<String, serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        reply_to_message_id: Option<Uuid>,
    }

    let canonical = Canonical {
        text,
        attachments,
        voice,
        voice_transcript,
        gif,
        metadata,
        reply_to_message_id,
    };
    let mut hasher = blake3::Hasher::new();
    if serde_json::to_writer(&mut hasher, &canonical).is_err() {
        return blake3::hash(b"invalid-content");
    }
    hasher.finalize()
}

fn legacy_content_digest(
    text: Option<&str>,
    attachments: &[Attachment],
    voice: Option<&VoiceAttachment>,
    gif: Option<&Gif>,
) -> blake3::Hash {
    #[derive(Serialize)]
    struct LegacyVoiceAttachment<'a> {
        permanent_url: &'a Url,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        content_type: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        size: Option<u64>,
    }

    #[derive(Serialize)]
    struct Canonical<'a> {
        text: Option<&'a str>,
        attachments: &'a [Attachment],
        voice: Option<LegacyVoiceAttachment<'a>>,
        gif: Option<&'a Gif>,
    }

    let canonical = Canonical {
        text,
        attachments,
        voice: voice.map(|voice| LegacyVoiceAttachment {
            permanent_url: &voice.permanent_url,
            name: voice.name.as_deref(),
            content_type: voice.content_type.as_deref(),
            size: voice.size,
        }),
        gif,
    };
    let mut hasher = blake3::Hasher::new();
    if serde_json::to_writer(&mut hasher, &canonical).is_err() {
        return blake3::hash(b"invalid-content");
    }
    hasher.finalize()
}

/// Validates attachment count and declared size.
pub(crate) fn validate_attachments(
    attachments: &[Attachment],
    voice: Option<&VoiceAttachment>,
) -> Result<(), &'static str> {
    if attachments.len() + usize::from(voice.is_some()) > MAX_ATTACHMENTS {
        return Err("a message or draft may contain at most 100 attachment items including voice");
    }
    for attachment in attachments {
        validate_attachment_metadata(
            &attachment.permanent_url,
            attachment.name.as_deref(),
            attachment.content_type.as_deref(),
            attachment.size,
        )?;
    }
    if let Some(voice) = voice {
        validate_attachment_metadata(
            &voice.permanent_url,
            voice.name.as_deref(),
            voice.content_type.as_deref(),
            voice.size,
        )?;
        if !voice
            .duration_milliseconds
            .is_some_and(|duration| (1..=MAX_VOICE_DURATION_MILLISECONDS).contains(&duration))
        {
            return Err("voice duration must contain 1 millisecond to 48 hours");
        }
    }
    Ok(())
}

fn validate_attachment_metadata(
    permanent_url: &Url,
    name: Option<&str>,
    content_type: Option<&str>,
    size: Option<u64>,
) -> Result<(), &'static str> {
    if size.is_some_and(|size| size > MAX_ATTACHMENT_BYTES) {
        return Err("an attachment may not exceed 5 GiB");
    }
    if permanent_url.scheme() != "https"
        || permanent_url.host_str().is_none()
        || !permanent_url.username().is_empty()
        || permanent_url.password().is_some()
        || permanent_url.as_str().len() > 8_192
    {
        return Err(
            "attachments require a credential-free HTTPS permanent URL of at most 8,192 bytes",
        );
    }
    if name.is_some_and(|name| name.is_empty() || name.chars().count() > 1_024) {
        return Err("attachment name must contain 1 to 1,024 characters");
    }
    if content_type.is_some_and(|value| value.is_empty() || value.len() > 255) {
        return Err("attachment content_type must contain 1 to 255 bytes");
    }
    Ok(())
}

pub(crate) fn validate_gif(gif: Option<&Gif>) -> Result<(), &'static str> {
    let Some(gif) = gif else {
        return Ok(());
    };
    if gif.provider_id.trim().is_empty() || gif.provider_id.len() > 255 {
        return Err("GIF provider_id must contain 1 to 255 bytes");
    }
    if gif.url.scheme() != "https" || gif.url.as_str().len() > 8_192 {
        return Err("GIF URL must be HTTPS and at most 8,192 bytes");
    }
    if gif
        .preview_url
        .as_ref()
        .is_some_and(|url| url.scheme() != "https" || url.as_str().len() > 8_192)
    {
        return Err("GIF preview URL must be HTTPS and at most 8,192 bytes");
    }
    if gif
        .title
        .as_ref()
        .is_some_and(|title| title.chars().count() > 1_000)
    {
        return Err("GIF title may not exceed 1,000 characters");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Attachment, MessageCreate, VoiceAttachment};

    #[test]
    fn sender_alone_is_not_message_content() {
        let message = MessageCreate::default();
        assert_eq!(
            message.validate(),
            Err("a message must contain text, an attachment, voice, or a GIF")
        );
    }

    #[test]
    fn empty_text_is_not_message_content() {
        let message = MessageCreate {
            text: Some(String::new()),
            ..MessageCreate::default()
        };
        assert_eq!(
            message.validate(),
            Err("message text must contain at least one character")
        );
    }

    #[test]
    fn attachment_size_limit_is_inclusive() {
        let permanent_url = "https://briefcase.example/entries/42"
            .parse()
            .map_err(|error| format!("test URL must parse: {error}"));
        let attachment = permanent_url.map(|permanent_url| Attachment {
            permanent_url,
            name: None,
            content_type: None,
            size: Some(super::MAX_ATTACHMENT_BYTES),
        });
        assert!(attachment.is_ok_and(|attachment| {
            MessageCreate {
                attachments: vec![attachment],
                ..MessageCreate::default()
            }
            .validate()
            .is_ok()
        }));
    }

    #[test]
    fn content_digest_ignores_sender_routing_metadata() {
        let first = MessageCreate {
            text: Some("hello".to_owned()),
            ..MessageCreate::default()
        };
        let mut second = first.clone();
        second.sender_id = "someone".parse().ok();
        assert_eq!(first.content_digest(), second.content_digest());
    }

    #[test]
    fn absent_transcript_preserves_the_existing_canonical_digest_shape() {
        let message = MessageCreate {
            text: Some("hello".to_owned()),
            ..MessageCreate::default()
        };
        assert_eq!(
            message.content_digest(),
            blake3::hash(br#"{"text":"hello","attachments":[],"voice":null,"gif":null}"#)
        );
    }

    #[test]
    fn credentialed_attachment_urls_are_rejected() {
        let attachment = "https://user:secret@example.com/file"
            .parse()
            .map(|permanent_url| Attachment {
                permanent_url,
                name: None,
                content_type: None,
                size: None,
            });
        assert!(attachment.is_ok_and(|attachment| {
            MessageCreate {
                attachments: vec![attachment],
                ..MessageCreate::default()
            }
            .validate()
            .is_err()
        }));
    }

    #[test]
    fn voice_duration_is_required_to_be_within_the_product_limit() {
        for duration_milliseconds in [0, super::MAX_VOICE_DURATION_MILLISECONDS + 1] {
            let voice = "https://media.example/voice.ogg"
                .parse()
                .map(|permanent_url| VoiceAttachment {
                    permanent_url,
                    name: None,
                    content_type: Some("audio/ogg".to_owned()),
                    size: None,
                    duration_milliseconds: Some(duration_milliseconds),
                });
            assert!(voice.is_ok_and(|voice| {
                MessageCreate {
                    voice: Some(voice),
                    ..MessageCreate::default()
                }
                .validate()
                .is_err()
            }));
        }
    }

    #[test]
    fn voice_duration_and_transcript_affect_the_content_digest() {
        let first = "https://media.example/voice.ogg"
            .parse()
            .map(|permanent_url| MessageCreate {
                voice: Some(VoiceAttachment {
                    permanent_url,
                    name: None,
                    content_type: Some("audio/ogg".to_owned()),
                    size: None,
                    duration_milliseconds: Some(1_000),
                }),
                voice_transcript: Some("hello".to_owned()),
                ..MessageCreate::default()
            });
        assert!(first.is_ok_and(|first| {
            let mut changed_duration = first.clone();
            if let Some(voice) = &mut changed_duration.voice {
                voice.duration_milliseconds = voice.duration_milliseconds.map(|value| value + 1);
            }
            let mut changed_transcript = first.clone();
            changed_transcript.voice_transcript = Some("goodbye".to_owned());
            first.content_digest() != changed_duration.content_digest()
                && first.content_digest() != changed_transcript.content_digest()
        }));
    }
}
