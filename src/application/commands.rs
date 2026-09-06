//! Typed commands crossing application and persistence boundaries.

use time::OffsetDateTime;
use uuid::Uuid;

use crate::{
    domain::{
        Activity, ActorId, ActorRef, BundleCreate, Draft, DraftInput, IdempotencyKey,
        MessageCreate, OrganizationId, ReceiptStatus,
    },
    realtime::DeliveryPayload,
};

/// Create or resolve an exact-participant-set conversation.
pub struct CreateConversationCommand {
    /// Organization scope.
    pub organization_id: OrganizationId,
    /// Authenticated creator.
    pub creator: ActorRef,
    /// IAM-resolved canonical participants, including the creator.
    pub participants: Vec<ActorRef>,
    /// Retry-safe client key.
    pub idempotency_key: IdempotencyKey,
}

/// Accept one durable message and enqueue recipient deliveries atomically.
pub struct SendMessageCommand {
    /// Organization scope.
    pub organization_id: OrganizationId,
    /// Parent conversation.
    pub conversation_id: Uuid,
    /// Authenticated sender.
    pub sender: ActorRef,
    /// Validated content, including any client-provided voice transcript.
    pub content: MessageCreate,
    /// Retry-safe client key.
    pub idempotency_key: IdempotencyKey,
}

/// Record a device-aware receipt and notify the original sender.
pub struct RecordReceiptCommand {
    /// Organization scope.
    pub organization_id: OrganizationId,
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
    /// Organization scope.
    pub organization_id: OrganizationId,
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
    /// Organization scope.
    pub organization_id: OrganizationId,
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

/// Lightweight durable delivery position used to wake local sockets.
#[derive(Clone, Debug)]
pub struct DeliveryNotice {
    /// Stable delivery identifier.
    pub id: Uuid,
    /// Organization scope.
    pub organization_id: OrganizationId,
    /// Target actor.
    pub target: ActorRef,
    /// Actor-stream sequence.
    pub sequence: i64,
}

/// One replayable actor-delivery row.
#[derive(Clone, Debug)]
pub struct ActorDelivery {
    /// Stable delivery identifier.
    pub id: Uuid,
    /// Organization scope.
    pub organization_id: OrganizationId,
    /// Target actor.
    pub target: ActorRef,
    /// Actor-stream sequence.
    pub sequence: i64,
    /// Immutable payload snapshot or durable source expansion.
    pub payload: DeliveryPayload,
}

/// Creates a cross-instance presence lease for one socket.
pub struct OpenRealtimeSessionCommand {
    /// Application-generated session/connection ID.
    pub session_id: Uuid,
    /// API process identity.
    pub instance_id: String,
    /// Stable device/consumer identifier.
    pub consumer_id: String,
    /// IAM subject retained without credentials.
    pub authenticated_subject: String,
    /// Organization scope.
    pub organization_id: OrganizationId,
    /// IAM-authorized represented actors.
    pub actors: Vec<ActorRef>,
    /// Initial lease expiry.
    pub lease_expires_at: OffsetDateTime,
}

/// Refreshes heartbeat and optional activity for a represented actor.
pub struct UpdateRealtimeActivityCommand {
    /// Session identifier.
    pub session_id: Uuid,
    /// Organization scope.
    pub organization_id: OrganizationId,
    /// Represented actor.
    pub actor_id: ActorId,
    /// Current activity; null clears it.
    pub activity: Option<Activity>,
    /// Activity expiry when present.
    pub activity_expires_at: Option<OffsetDateTime>,
}
