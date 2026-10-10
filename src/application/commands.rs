//! Typed commands crossing application and persistence boundaries. Accounts are storage
//! references (uuids); a conversation's storage partition is looked up from the
//! conversation itself, never taken from the caller.

use uuid::Uuid;

use crate::domain::{
    ActorRef, BundleCreate, Draft, DraftInput, IdempotencyKey, MessageCreate, ReceiptStatus,
};

/// Create or resolve the canonical direct conversation of an exact participant set.
pub struct CreateConversationCommand {
    /// Authenticated creator.
    pub creator: ActorRef,
    /// Resolved participants, including the creator.
    pub participants: Vec<ActorRef>,
    /// Retry-safe client key.
    pub idempotency_key: IdempotencyKey,
}

/// Accept one durable message and enqueue recipient deliveries atomically.
pub struct SendMessageCommand {
    /// Parent conversation.
    pub conversation_id: Uuid,
    /// Authenticated sender.
    pub sender: ActorRef,
    /// Validated content, with ISI addresses already resolved to storage keys.
    pub content: MessageCreate,
    /// Retry-safe client key.
    pub idempotency_key: IdempotencyKey,
    /// The app that sent it for the account through a User verification proof.
    pub via_app: Option<String>,
}

/// Record a device-aware receipt and notify the original sender.
pub struct RecordReceiptCommand {
    /// Parent conversation.
    pub conversation_id: Uuid,
    /// Message being acknowledged.
    pub message_id: Uuid,
    /// Authenticated recipient.
    pub recipient: ActorRef,
    /// Stable device identifier.
    pub device_id: String,
    /// Monotonic receipt state.
    pub status: ReceiptStatus,
}

/// Create a flat, append-positioned message bundle atomically.
pub struct CreateBundleCommand {
    /// Parent conversation.
    pub conversation_id: Uuid,
    /// Authenticated Silicon creator.
    pub creator: ActorRef,
    /// Original members and display message.
    pub bundle: BundleCreate,
    /// Retry-safe client key.
    pub idempotency_key: IdempotencyKey,
}

/// Optimistic draft write.
pub struct PutDraftCommand {
    /// Parent conversation.
    pub conversation_id: Uuid,
    /// Private draft owner.
    pub actor: ActorRef,
    /// Last observed version; zero/none may create only.
    pub expected_version: Option<i64>,
    /// Validated draft content.
    pub input: DraftInput,
}

/// Result of an optimistic draft write.
pub enum PutDraftOutcome {
    /// Write committed and version advanced.
    Saved(Draft),
    /// A newer server value won the race.
    Conflict(Draft),
}
