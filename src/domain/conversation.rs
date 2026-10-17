//! Conversation representations.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use super::{ActorRef, Message};

/// A direct conversation (one exact participant set) or a group, and its latest message.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Conversation {
    /// Stable conversation identifier (rendered as its public address).
    pub id: Uuid,
    /// Canonically ordered participants.
    pub participants: Vec<ActorRef>,
    /// Custodians who can read this conversation because one of its participants is a
    /// Silicon they look after.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub visible_to_custodians: Vec<ActorRef>,
    /// When the same accounts have a newer canonical conversation, its id. This one stays
    /// readable; new messages go to the canonical one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<Uuid>,
    /// Older conversations of the same accounts (from before DM had one conversation per
    /// set of accounts); their history stays readable by id.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supersedes: Vec<Uuid>,
    /// Named group policy; omitted for existing direct conversations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<super::GroupDetails>,
    /// Latest visible message, if any.
    pub last_message: Option<Message>,
    /// Messages from others the listed account (the caller, or the Silicon a custodian named
    /// with `for`) has not read: not deleted, not bundled away, no read receipt on any device.
    #[serde(default)]
    pub unread_count: i64,
    /// Code of the newest message the listed account has read, if any.
    #[serde(default)]
    pub last_read_message_id: Option<String>,
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

/// How an account may use a conversation or group.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConversationAccess {
    /// A participant of the direct conversation, or an accepted member of the group.
    Participant,
    /// Custodian of a Silicon in it: reads everything; may tombstone that Silicon's
    /// messages and manage its group memberships, never act as it.
    Custodian,
    /// Invited to the group and not yet a member: may see the group's settings only.
    Invitee,
}
