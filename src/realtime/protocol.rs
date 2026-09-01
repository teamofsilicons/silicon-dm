//! Versioned WebSocket frame schema.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::domain::{
    Activity, ActorId, IdempotencyKey, Message, MessageCreate, OrganizationId, ReceiptStatus,
};

/// Current application-level WebSocket protocol version.
pub const PROTOCOL_VERSION: u16 = 2;

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
    use std::collections::BTreeMap;

    use uuid::Uuid;

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
        assert_eq!(PROTOCOL_VERSION, 2);
        Ok(())
    }

    #[test]
    fn retired_system_event_payload_is_not_part_of_protocol_v2() {
        let legacy = serde_json::json!({
            "kind": "system_event",
            "event": {
                "event_id": "018f0d52-7b2a-7e29-a41d-7c02b93f6f42"
            }
        });
        assert!(serde_json::from_value::<super::DeliveryPayload>(legacy).is_err());
    }

    #[test]
    fn ready_frame_advertises_protocol_version_two() -> Result<(), Box<dyn std::error::Error>> {
        let encoded = serde_json::to_value(ServerFrame::Ready {
            protocol_version: PROTOCOL_VERSION,
            connection_id: Uuid::nil(),
            actors: vec!["carbon-1".parse()?],
            acknowledged_through: BTreeMap::from([("carbon-1".to_owned(), 7)]),
        })?;
        assert_eq!(encoded.get("type"), Some(&serde_json::json!("ready")));
        assert_eq!(encoded.get("protocol_version"), Some(&serde_json::json!(2)));
        Ok(())
    }

    #[test]
    fn protocol_v2_voice_command_requires_duration_and_preserves_transcript()
    -> Result<(), Box<dyn std::error::Error>> {
        let frame = serde_json::json!({
            "type": "send_message",
            "actor_id": "carbon-1",
            "org_id": "organization-1",
            "conversation_id": "018f0d52-7b2a-7e29-a41d-7c02b93f6f42",
            "idempotency_key": "voice-command-1",
            "message": {
                "voice": {
                    "permanent_url": "https://media.example/voice.ogg",
                    "content_type": "audio/ogg",
                    "duration_milliseconds": 42_000
                },
                "voice_transcript": "client transcript"
            }
        });
        let decoded: ClientFrame = serde_json::from_value(frame.clone())?;
        let ClientFrame::SendMessage { message, .. } = decoded else {
            return Err("voice command decoded as the wrong frame variant".into());
        };
        assert_eq!(
            message
                .voice
                .as_ref()
                .and_then(|voice| voice.duration_milliseconds),
            Some(42_000)
        );
        assert_eq!(
            message.voice_transcript.as_deref(),
            Some("client transcript")
        );
        assert!(message.validate().is_ok());

        let mut missing_duration = frame;
        if let Some(voice) = missing_duration
            .get_mut("message")
            .and_then(|message| message.get_mut("voice"))
            .and_then(serde_json::Value::as_object_mut)
        {
            voice.remove("duration_milliseconds");
        }
        assert!(serde_json::from_value::<ClientFrame>(missing_duration).is_err());
        Ok(())
    }
}
