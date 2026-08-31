//! Non-destructive message bundles.

use serde::{Deserialize, Serialize};
use sqlx::Type;
use time::OffsetDateTime;
use uuid::Uuid;

use super::{ActorRef, Message, MessageCreate};

/// A message's role in one flat bundle.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "bundle_role", rename_all = "snake_case")]
pub enum BundleRole {
    /// Collapsed representation shown in the default timeline.
    Display,
    /// Original message hidden by the default timeline.
    Member,
}

/// Stable bundle reference embedded in a message.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BundleRef {
    /// Bundle identifier.
    pub id: Uuid,
    /// Message role.
    pub role: BundleRole,
}

/// Bundle creation request.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct BundleCreate {
    /// Unique original message identifiers.
    pub message_ids: Vec<Uuid>,
    /// New display message.
    pub display_message: MessageCreate,
}

/// Public bundle representation.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Bundle {
    /// Stable bundle identifier.
    pub id: Uuid,
    /// Parent conversation.
    pub conversation_id: Uuid,
    /// Original messages in caller-supplied order.
    pub original_message_ids: Vec<Uuid>,
    /// Newly created display message.
    pub display_message: Message,
    /// Silicon that created the bundle.
    pub created_by: ActorRef,
    /// Creation time.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// Expanded bundle containing original messages.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct BundleDetail {
    /// Stable bundle identifier.
    pub id: Uuid,
    /// Parent conversation.
    pub conversation_id: Uuid,
    /// Original message identifiers.
    pub original_message_ids: Vec<Uuid>,
    /// Display message.
    pub display_message: Message,
    /// Silicon that created the bundle.
    pub created_by: ActorRef,
    /// Creation time.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// Original messages with content and state intact.
    pub original_messages: Vec<Message>,
}

impl BundleCreate {
    /// Checks cardinality, uniqueness, and display content.
    ///
    /// # Errors
    ///
    /// Returns a stable validation message.
    pub fn validate(&self) -> Result<(), &'static str> {
        if !(1..=100).contains(&self.message_ids.len()) {
            return Err("a bundle must contain between 1 and 100 messages");
        }
        let unique = self
            .message_ids
            .iter()
            .collect::<std::collections::HashSet<_>>();
        if unique.len() != self.message_ids.len() {
            return Err("bundle message_ids must be unique");
        }
        self.display_message.validate()
    }
}
