//! Versioned WebSocket frame schema.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::domain::{Activity, ActorId, IdempotencyKey, Message, OrganizationId, ReceiptStatus};

/// Current application-level WebSocket protocol version.
pub const PROTOCOL_VERSION: u16 = silicon_dm_protocol::WEBSOCKET_VERSION;

/// Frames accepted from an authenticated client.
#[derive(Clone, Debug)]
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
        conversation_id: String,
        /// Message being acknowledged.
        message_id: String,
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
        conversation_id: String,
        /// Retry-safe client key.
        idempotency_key: IdempotencyKey,
        /// Normal message content.
        message: Box<serde_json::Value>,
    },
    /// Creates an atomic bundle with caller-supplied display content.
    CreateBundle {
        /// Authorized creator.
        actor_id: ActorId,
        /// Authorized organization.
        org_id: OrganizationId,
        /// Existing conversation address.
        conversation_id: String,
        /// Stable retry key.
        idempotency_key: IdempotencyKey,
        /// Bundle input with public message codes.
        bundle: Box<serde_json::Value>,
    },
    /// Failed heartbeat; does not refresh the connection lease.
    PingError {
        /// Ping being reported.
        ping_id: String,
    },
}

#[derive(Deserialize)]
#[serde(remote = "ClientFrame")]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
enum ClientFrameWire {
    /// Application heartbeat response. It is never persisted or sequenced.
    #[serde(rename = "ping.success")]
    Pong {
        /// Echoed server ping identifier.
        ping_id: String,
    },
    /// Cumulative transport acknowledgement for one represented actor.
    Ack {
        /// Delivery stream owner.
        #[serde(rename = "member_id")]
        actor_id: ActorId,
        /// Highest contiguously processed delivery sequence.
        through_sequence: i64,
    },
    /// Requests replay after a client-owned durable cursor.
    Resume {
        /// Delivery stream owner.
        #[serde(rename = "member_id")]
        actor_id: ActorId,
        /// Last sequence durably processed by the client; zero starts at the beginning.
        after_sequence: i64,
    },
    /// Updates or clears transient activity for one represented actor.
    Presence {
        /// Actor whose activity changed.
        #[serde(rename = "member_id")]
        actor_id: ActorId,
        /// Current activity; null clears it while retaining online state.
        activity: Option<Activity>,
    },
    /// Records a device-aware delivered or read receipt.
    Receipt {
        /// Represented recipient actor.
        #[serde(rename = "member_id")]
        actor_id: ActorId,
        /// Parent conversation.
        conversation_id: String,
        /// Message being acknowledged.
        message_id: String,
        /// Monotonic receipt state.
        status: ReceiptStatus,
        /// Stable client device identifier.
        device_id: String,
    },
    /// Creates a durable conversation message over the realtime connection.
    #[serde(rename = "message.create")]
    SendMessage {
        /// Represented sender.
        #[serde(rename = "member_id")]
        actor_id: ActorId,
        /// Organization scope.
        org_id: OrganizationId,
        /// Parent conversation.
        conversation_id: String,
        /// Retry-safe client key.
        idempotency_key: IdempotencyKey,
        /// Normal message content.
        #[serde(flatten)]
        message: Box<serde_json::Value>,
    },
    /// Creates an atomic bundle with caller-supplied display content.
    #[serde(rename = "bundle")]
    CreateBundle {
        /// Authorized creator.
        #[serde(rename = "member_id")]
        actor_id: ActorId,
        /// Authorized organization.
        org_id: OrganizationId,
        /// Existing conversation address.
        conversation_id: String,
        /// Stable retry key.
        idempotency_key: IdempotencyKey,
        /// Bundle input with public message codes.
        #[serde(flatten)]
        bundle: Box<serde_json::Value>,
    },
    /// Failed heartbeat; does not refresh the connection lease.
    #[serde(rename = "ping.error")]
    PingError {
        /// Ping being reported.
        ping_id: String,
    },
}

impl<'de> Deserialize<'de> for ClientFrame {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        if value
            .as_object()
            .is_none_or(|o| o.len() != 2 || !o.contains_key("type") || !o.contains_key("data"))
        {
            return Err(serde::de::Error::custom(
                "expected a type/data command envelope",
            ));
        }
        ClientFrameWire::deserialize(value).map_err(serde::de::Error::custom)
    }
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
#[serde(
    tag = "type",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ServerFrame {
    /// Initial session metadata and server-observed ACK cursors.
    #[serde(rename = "connection.ready")]
    Ready {
        /// Protocol version used by this connection.
        protocol_version: u16,
        /// Selected test data generation; null for production. Changed values reset local cursors.
        testing_generation: Option<i64>,
        /// Unique connection identifier.
        connection_id: Uuid,
        /// IAM-authorized actors represented by the connection.
        #[serde(rename = "members")]
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
    #[serde(rename = "message.create.success")]
    MessageAccepted {
        /// Retry-safe client key associated with the command.
        idempotency_key: IdempotencyKey,
        /// Newly accepted or idempotently replayed message.
        #[serde(flatten)]
        message: Box<Message>,
    },
    /// Confirms a monotonic device receipt was stored.
    #[serde(rename = "receipt.success")]
    ReceiptRecorded {
        /// Message whose receipt was stored.
        message_id: Uuid,
        /// Device receipt state accepted by DM.
        status: ReceiptStatus,
    },
    /// Durable conversation-message delivery.
    #[serde(rename = "new_message")]
    Message {
        /// Stable actor-delivery identifier used for deduplication.
        delivery_id: Uuid,
        /// Delivery stream owner.
        actor_id: ActorId,
        /// Stable sequence within the actor stream.
        delivery_sequence: i64,
        /// Durable message.
        #[serde(flatten)]
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
    #[serde(rename = "connection.error")]
    Error {
        /// Stable machine-readable category.
        code: String,
        /// Safe human-readable summary.
        message: String,
        /// Whether the connection remains usable and the command may be retried.
        recoverable: bool,
    },
    /// Explicit command response whose fields are already in the public contract.
    #[serde(untagged)]
    CommandResponse(serde_json::Value),
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
            | Self::Error { .. }
            | Self::CommandResponse(_) => None,
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
        let frame: ClientFrame =
            serde_json::from_str(r#"{"type":"ping.success","data":{"ping_id":"p-1"}}"#)?;
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
        assert_eq!(PROTOCOL_VERSION, 5);
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
    fn ready_frame_advertises_current_protocol_version() -> Result<(), Box<dyn std::error::Error>> {
        let encoded = serde_json::to_value(ServerFrame::Ready {
            testing_generation: None,
            protocol_version: PROTOCOL_VERSION,
            connection_id: Uuid::nil(),
            actors: vec!["carbon-1".parse()?],
            acknowledged_through: BTreeMap::from([("carbon-1".to_owned(), 7)]),
        })?;
        assert_eq!(
            encoded.get("type"),
            Some(&serde_json::json!("connection.ready"))
        );
        assert_eq!(
            encoded.pointer("/data/protocol_version"),
            Some(&serde_json::json!(5))
        );
        Ok(())
    }

    #[test]
    fn protocol_v2_voice_command_requires_duration_and_preserves_transcript()
    -> Result<(), Box<dyn std::error::Error>> {
        let frame = serde_json::json!({
            "type": "message.create",
            "data": {
            "member_id": "carbon-1",
            "org_id": "organization-1",
            "conversation_id": "018f0d52-7b2a-7e29-a41d-7c02b93f6f42",
            "idempotency_key": "voice-command-1",
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
        let message: crate::domain::MessageCreate = serde_json::from_value(*message)?;
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
            .get_mut("data")
            .and_then(|data| data.get_mut("voice"))
            .and_then(serde_json::Value::as_object_mut)
        {
            voice.remove("duration_milliseconds");
        }
        assert!(
            serde_json::from_value::<crate::domain::MessageCreate>(
                missing_duration["data"].clone()
            )
            .is_err()
        );
        Ok(())
    }
}
