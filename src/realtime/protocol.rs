//! Versioned WebSocket frame schema.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::domain::{
    Activity, ActorId, IdempotencyKey, Message, MessageCreate, OrganizationId, ReceiptStatus,
    SystemEvent,
};

/// Current application-level WebSocket protocol version.
pub const PROTOCOL_VERSION: u16 = 1;

/// Frames accepted from an authenticated client.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientFrame {
    /// Application heartbeat response. It is never persisted or sequenced.
    Pong {
        /// Echoed server ping identifier.
        ping_id: String,
    },
    /// Cumulative transport acknowledgement for one represented actor.
    Ack {
        /// Delivery stream owner.
        actor_id: ActorId,
        /// Highest contiguously processed delivery sequence.
        through_sequence: i64,
    },
    /// Requests replay after a client-owned durable cursor.
    Resume {
        /// Delivery stream owner.
        actor_id: ActorId,
        /// Last sequence durably processed by the client; zero starts at the beginning.
        after_sequence: i64,
    },
    /// Updates or clears transient activity for one represented actor.
    Presence {
        /// Actor whose activity changed.
        actor_id: ActorId,
        /// Current activity; null clears it while retaining online state.
        activity: Option<Activity>,
    },
    /// Records a device-aware delivered or read receipt.
    Receipt {
        /// Represented recipient actor.
        actor_id: ActorId,
        /// Parent conversation.
        conversation_id: Uuid,
        /// Message being acknowledged.
        message_id: Uuid,
        /// Monotonic receipt state.
        status: ReceiptStatus,
        /// Stable client device identifier.
        device_id: String,
    },
    /// Creates a durable conversation message over the realtime connection.
    SendMessage {
        /// Represented sender.
        actor_id: ActorId,
        /// Organization scope.
        org_id: OrganizationId,
        /// Parent conversation.
        conversation_id: Uuid,
        /// Retry-safe client key.
        idempotency_key: IdempotencyKey,
        /// Normal message content.
        message: Box<MessageCreate>,
    },
}

/// Payload stored in one actor's durable delivery stream.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DeliveryPayload {
    /// Conversation message.
    Message {
        /// Durable message.
        message: Box<Message>,
    },
    /// Hook-originated event.
    SystemEvent {
        /// Durable system event.
        event: Box<SystemEvent>,
    },
    /// Aggregate message state update.
    Receipt {
        /// Stable message identifier.
        message_id: Uuid,
        /// New aggregate status.
        status: crate::domain::MessageStatus,
    },
}

/// Frames emitted by DM.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerFrame {
    /// Initial session metadata and server-observed ACK cursors.
    Ready {
        /// Protocol version used by this connection.
        protocol_version: u16,
        /// Unique connection identifier.
        connection_id: Uuid,
        /// IAM-authorized actors represented by the connection.
        actors: Vec<ActorId>,
        /// Highest server-recorded ACK cursor keyed by actor ID.
        acknowledged_through: BTreeMap<String, i64>,
    },
    /// Application heartbeat request. It is never persisted or sequenced.
    Ping {
        /// Unique heartbeat identifier.
        ping_id: String,
    },
    /// Confirms that a realtime send command is durably accepted.
    MessageAccepted {
        /// Retry-safe client key associated with the command.
        idempotency_key: IdempotencyKey,
        /// Newly accepted or idempotently replayed message.
        message: Box<Message>,
    },
    /// Confirms a monotonic device receipt was stored.
    ReceiptRecorded {
        /// Message whose receipt was stored.
        message_id: Uuid,
        /// Device receipt state accepted by DM.
        status: ReceiptStatus,
    },
    /// Durable conversation-message delivery.
    Message {
        /// Stable actor-delivery identifier used for deduplication.
        delivery_id: Uuid,
        /// Delivery stream owner.
        actor_id: ActorId,
        /// Stable sequence within the actor stream.
        delivery_sequence: i64,
        /// Durable message.
        message: Box<Message>,
    },
    /// Durable Hook-event delivery.
    SystemEvent {
        /// Stable actor-delivery identifier used for deduplication.
        delivery_id: Uuid,
        /// Delivery stream owner.
        actor_id: ActorId,
        /// Stable sequence within the actor stream.
        delivery_sequence: i64,
        /// Durable system event.
        event: Box<SystemEvent>,
    },
    /// Durable aggregate receipt update.
    Receipt {
        /// Stable actor-delivery identifier used for deduplication.
        delivery_id: Uuid,
        /// Delivery stream owner.
        actor_id: ActorId,
        /// Stable sequence within the actor stream.
        delivery_sequence: i64,
        /// Message whose aggregate status changed.
        message_id: Uuid,
        /// New aggregate status.
        status: crate::domain::MessageStatus,
    },
    /// Structured protocol or command failure.
    Error {
        /// Stable machine-readable category.
        code: String,
        /// Safe human-readable summary.
        message: String,
        /// Whether the connection remains usable and the command may be retried.
        recoverable: bool,
    },
}

impl ServerFrame {
    /// Returns the actor stream position for a durable frame.
    #[must_use]
    pub fn delivery_position(&self) -> Option<(&ActorId, i64)> {
        match self {
            Self::Message {
                actor_id,
                delivery_sequence,
                ..
            }
            | Self::SystemEvent {
                actor_id,
                delivery_sequence,
                ..
            }
            | Self::Receipt {
                actor_id,
                delivery_sequence,
                ..
            } => Some((actor_id, *delivery_sequence)),
            Self::Ready { .. }
            | Self::Ping { .. }
            | Self::MessageAccepted { .. }
            | Self::ReceiptRecorded { .. }
            | Self::Error { .. } => None,
        }
    }

    /// Wraps a durable payload in the corresponding public frame family.
    #[must_use]
    pub fn delivery(
        delivery_id: Uuid,
        actor_id: ActorId,
        delivery_sequence: i64,
        payload: DeliveryPayload,
    ) -> Self {
        match payload {
            DeliveryPayload::Message { message } => Self::Message {
                delivery_id,
                actor_id,
                delivery_sequence,
                message,
            },
            DeliveryPayload::SystemEvent { event } => Self::SystemEvent {
                delivery_id,
                actor_id,
                delivery_sequence,
                event,
            },
            DeliveryPayload::Receipt { message_id, status } => Self::Receipt {
                delivery_id,
                actor_id,
                delivery_sequence,
                message_id,
                status,
            },
        }
    }

    /// Creates a safe recoverable error frame.
    #[must_use]
    pub fn recoverable_error(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Error {
            code: code.into(),
            message: message.into(),
            recoverable: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ClientFrame, PROTOCOL_VERSION, ServerFrame};

    #[test]
    fn pong_matches_published_shape() -> Result<(), serde_json::Error> {
        let frame: ClientFrame = serde_json::from_str(r#"{"type":"pong","ping_id":"p-1"}"#)?;
        assert!(matches!(frame, ClientFrame::Pong { ping_id } if ping_id == "p-1"));
        Ok(())
    }

    #[test]
    fn heartbeat_has_no_delivery_sequence() -> Result<(), serde_json::Error> {
        let encoded = serde_json::to_value(ServerFrame::Ping {
            ping_id: "p-1".to_owned(),
        })?;
        assert_eq!(encoded.get("type"), Some(&serde_json::json!("ping")));
        assert!(encoded.get("delivery_sequence").is_none());
        assert_eq!(PROTOCOL_VERSION, 1);
        Ok(())
    }
}
