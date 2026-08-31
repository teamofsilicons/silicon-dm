//! Durable events delivered from Silicon Hook.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

use super::{ActorId, OrganizationId};

/// Versioned Hook event envelope.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SystemEvent {
    /// Stable idempotent Hook event identifier.
    pub event_id: Uuid,
    /// Organization scope.
    pub org_id: OrganizationId,
    /// Target Silicon.
    pub silicon_id: ActorId,
    /// Versioned event type.
    #[serde(rename = "type")]
    pub event_type: String,
    /// Optional distributed trace identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    /// Arbitrary Hook payload object.
    pub payload: Map<String, Value>,
}

impl SystemEvent {
    /// Checks event metadata and a defensive serialized payload limit.
    ///
    /// # Errors
    ///
    /// Returns a stable validation message.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.event_type.trim().is_empty()
            || self.event_type.chars().count() > 255
            || self.event_type.chars().any(char::is_control)
        {
            return Err("system event type must contain 1 to 255 non-control characters");
        }
        let payload_size =
            serde_json::to_vec(&self.payload).map_or(usize::MAX, |bytes| bytes.len());
        if payload_size > 1024 * 1024 {
            return Err("system event payload may not exceed 1 MiB");
        }
        Ok(())
    }
}
