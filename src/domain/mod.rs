//! Domain types and invariants.

mod account;
mod actor;
pub mod audio;
mod bundle;
mod conversation;
mod draft;
mod group;
mod idempotency;
mod message;
mod pagination;
mod presence;
pub mod speech;

pub use account::Account;
pub use actor::{
    ACCOUNTS_PARTITION, ActorId, ActorRef, ActorType, OrganizationId, is_account_uuid,
};
pub use bundle::{Bundle, BundleCreate, BundleDetail, BundleRef, BundleRole};
pub use conversation::{Conversation, ConversationAccess, ConversationPage};
pub use draft::{Draft, DraftInput};
pub use group::{
    GroupCreate, GroupDetails, GroupSettings, MAX_GROUP_DESCRIPTION_CHARACTERS,
    MAX_GROUP_NAME_CHARACTERS, unaddressable_name,
};
pub use idempotency::{IdempotencyKey, IdempotencyKeyError};
pub use message::{
    Attachment, Gif, GifPage, Message, MessageCreate, MessagePage, MessageStatus, ReceiptStatus,
    VoiceAttachment,
};
pub use pagination::{Cursor, PageRequest};
pub use presence::{Activity, Availability, Presence};
pub use speech::{PinnedVoice, Speech, Voice, VoiceCatalog};

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
