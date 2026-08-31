//! Conversation representations.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use super::{ActorRef, Message};

/// Organization-scoped participant set and its latest message.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Conversation {
    /// Stable conversation identifier.
    pub id: Uuid,
    /// Owning organization.
    pub org_id: super::OrganizationId,
    /// Canonically ordered participants.
    pub participants: Vec<ActorRef>,
    /// Latest visible message, if any.
    pub last_message: Option<Message>,
    /// Creation time.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// Last durable activity time.
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

/// Cursor-paginated conversations.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ConversationPage {
    /// Current page.
    pub items: Vec<Conversation>,
    /// Opaque cursor for the next page.
    pub next_cursor: Option<String>,
}
