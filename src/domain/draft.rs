//! Cross-device draft synchronization.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use super::{
    ActorId, Attachment, Gif, MAX_TEXT_CHARACTERS,
    message::{validate_attachments, validate_gif},
};

/// Mutable draft content owned by one actor in one conversation.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct DraftInput {
    /// Draft text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_content: Option<String>,
    /// Permanent Briefcase attachments.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
    /// Permanent Briefcase voice attachment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<Attachment>,
    /// Optional GIF.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gif: Option<Gif>,
}

/// Versioned synchronized draft.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Draft {
    /// Parent conversation.
    pub conversation_id: Uuid,
    /// Private owner.
    pub actor_id: ActorId,
    /// Monotonically increasing version.
    pub version: i64,
    /// Draft text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_content: Option<String>,
    /// Permanent Briefcase attachments.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
    /// Permanent Briefcase voice attachment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<Attachment>,
    /// Optional GIF.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gif: Option<Gif>,
    /// Last successful write time.
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

impl DraftInput {
    /// Checks documented content limits.
    ///
    /// Empty drafts are accepted because a local-first editor may synchronize
    /// an intentionally cleared composition before deleting it.
    ///
    /// # Errors
    ///
    /// Returns a stable validation message.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self
            .message_content
            .as_ref()
            .is_some_and(|text| text.chars().count() > MAX_TEXT_CHARACTERS)
        {
            return Err("draft text exceeds 100,000,000 characters");
        }
        validate_attachments(&self.attachments, self.voice.as_ref())?;
        validate_gif(self.gif.as_ref())
    }

    /// Returns the same canonical content digest used by message creation.
    #[must_use]
    pub fn content_digest(&self) -> blake3::Hash {
        super::MessageCreate {
            sender_id: None,
            text: self.message_content.clone(),
            attachments: self.attachments.clone(),
            voice: self.voice.clone(),
            voice_transcript: None,
            gif: self.gif.clone(),
        }
        .content_digest()
    }
}
