//! Domain types and invariants.

mod actor;
mod bundle;
mod conversation;
mod draft;
mod idempotency;
mod message;
mod pagination;
mod presence;

pub use actor::{ActorId, ActorRef, ActorType, OrganizationId};
pub use bundle::{Bundle, BundleCreate, BundleDetail, BundleRef, BundleRole};
pub use conversation::{Conversation, ConversationPage};
pub use draft::{Draft, DraftInput};
pub use idempotency::IdempotencyKey;
pub use message::{
    Attachment, Gif, GifPage, Message, MessageCreate, MessagePage, MessageStatus, ReceiptStatus,
    VoiceAttachment,
};
pub use pagination::{Cursor, PageRequest};
pub use presence::{Activity, Availability, Presence};

/// Maximum number of Unicode scalar values in message or draft text.
pub const MAX_TEXT_CHARACTERS: usize = 100_000_000;
/// Maximum declared size of one attachment in bytes.
pub const MAX_ATTACHMENT_BYTES: u64 = 5 * 1024 * 1024 * 1024;
/// Defensive maximum number of attachments in one message or draft.
pub const MAX_ATTACHMENTS: usize = 100;
/// Defensive maximum number of actors in one conversation, including creator.
pub const MAX_CONVERSATION_PARTICIPANTS: usize = 100;
/// Maximum duration of a voice message in milliseconds.
pub const MAX_VOICE_DURATION_MILLISECONDS: u64 = 48 * 60 * 60 * 1_000;
