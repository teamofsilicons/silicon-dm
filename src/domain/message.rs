//! Message content, state, and validation.

use serde::{Deserialize, Serialize};
use sqlx::Type;
use time::OffsetDateTime;
use url::Url;
use uuid::Uuid;

use super::{ActorRef, BundleRef, MAX_ATTACHMENT_BYTES, MAX_ATTACHMENTS, MAX_TEXT_CHARACTERS};

/// Durable attachment metadata. `permanent_url` must point to Briefcase.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Attachment {
    /// Stable Briefcase resource URL.
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
    /// Explicit sender when a connection represents more than one actor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_id: Option<super::ActorId>,
    /// Optional text content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Zero or more permanent Briefcase attachments.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
    /// Optional permanent Briefcase voice attachment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<Attachment>,
    /// Transcript, absent/null when transcription failed or has not run.
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
    /// Stable message identifier.
    pub id: Uuid,
    /// Parent conversation.
    pub conversation_id: Uuid,
    /// Typed sender.
    pub sender: ActorRef,
    /// Stable sequence within the conversation.
    pub sequence: i64,
    /// Aggregate message state.
    pub status: MessageStatus,
    /// Text content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Permanent attachment references.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
    /// Permanent voice reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<Attachment>,
    /// Voice transcript, null after a failed transcription.
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
    /// Checks content-shape and documented resource limits.
    ///
    /// # Errors
    ///
    /// Returns a stable user-facing validation message.
    pub fn validate(&self) -> Result<(), &'static str> {
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
        #[derive(Serialize)]
        struct Canonical<'a> {
            text: &'a Option<String>,
            attachments: &'a [Attachment],
            voice: &'a Option<Attachment>,
            gif: &'a Option<Gif>,
        }

        let canonical = Canonical {
            text: &self.text,
            attachments: &self.attachments,
            voice: &self.voice,
            gif: &self.gif,
        };
        let mut hasher = blake3::Hasher::new();
        if serde_json::to_writer(&mut hasher, &canonical).is_err() {
            return blake3::hash(b"invalid-content");
        }
        hasher.finalize()
    }
}

/// Validates attachment count and declared size.
pub(crate) fn validate_attachments(
    attachments: &[Attachment],
    voice: Option<&Attachment>,
) -> Result<(), &'static str> {
    if attachments.len() + usize::from(voice.is_some()) > MAX_ATTACHMENTS {
        return Err("a message or draft may contain at most 100 attachment items including voice");
    }
    for attachment in attachments.iter().chain(voice) {
        if attachment
            .size
            .is_some_and(|size| size > MAX_ATTACHMENT_BYTES)
        {
            return Err("an attachment may not exceed 5 GiB");
        }
        if attachment.permanent_url.scheme() != "https"
            || attachment.permanent_url.as_str().len() > 8_192
        {
            return Err("attachments require an HTTPS permanent URL of at most 8,192 bytes");
        }
        if attachment
            .name
            .as_ref()
            .is_some_and(|name| name.is_empty() || name.chars().count() > 1_024)
        {
            return Err("attachment name must contain 1 to 1,024 characters");
        }
        if attachment
            .content_type
            .as_ref()
            .is_some_and(|value| value.is_empty() || value.len() > 255)
        {
            return Err("attachment content_type must contain 1 to 255 bytes");
        }
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
    use super::{Attachment, MessageCreate};

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
        second.voice_transcript = Some("provider-owned transcript".to_owned());
        assert_eq!(first.content_digest(), second.content_digest());
    }
}
