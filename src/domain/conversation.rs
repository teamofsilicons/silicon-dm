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
    /// Version of the listed account's draft here (what `GET …/draft` answers and `If-Match`
    /// takes), or null when it has none: a client asks for the draft only when there is one.
    #[serde(default)]
    pub draft_version: Option<i64>,
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
    /// What is unread across the listed account's conversations (the first page only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unread: Option<UnreadTotal>,
}

/// Unread messages across an account's conversations, for a badge or a window title: counted
/// over its most recently active conversations, with the messages counted up to the same
/// bound as one conversation's (shown as "999+").
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct UnreadTotal {
    /// Conversations with at least one unread message.
    pub conversations: i64,
    /// Unread messages in them, at most 1,000.
    pub messages: i64,
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

/// What happened to a conversation for one account: a conversation event in that account's
/// sync feed (`GET /api/v1/sync?include=conversations`). Events name the conversation and the
/// change only (a `read` event adds the account's unread facts); clients read the
/// conversation itself with their own permissions.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ConversationChange {
    /// A conversation was created with the account in it (it created it, or it is a member
    /// of a new group). Pending invitees of a new group get `Invited` instead.
    Created,
    /// The account has a new pending invitation to a group.
    Invited,
    /// The account was made a member of a group without accepting (its custodian added it).
    Added,
    /// The account accepted its invitation (on any device): the group is in its list now.
    InvitationAccepted,
    /// The account (or its custodian) declined its invitation.
    InvitationDeclined,
    /// An owner withdrew the account's pending invitation.
    InvitationCancelled,
    /// The account left the group (on any device).
    Left,
    /// The account was taken out of the group (by an owner, or by its custodian).
    Removed,
    /// The group's name changed.
    Renamed,
    /// The group's description changed.
    DescriptionChanged,
    /// Someone joined, left, was removed or invited, or an invitation was answered or withdrawn.
    MembersChanged,
    /// Owners were added or removed.
    OwnersChanged,
    /// The account read messages there (on any of its devices): the event carries its unread
    /// count and newest read message afterwards, so its other devices update their badges.
    Read,
}

impl ConversationChange {
    /// Every change, in the order the docs list them.
    pub const ALL: [Self; 13] = [
        Self::Created,
        Self::Invited,
        Self::Added,
        Self::InvitationAccepted,
        Self::InvitationDeclined,
        Self::InvitationCancelled,
        Self::Left,
        Self::Removed,
        Self::Renamed,
        Self::DescriptionChanged,
        Self::MembersChanged,
        Self::OwnersChanged,
        Self::Read,
    ];

    /// The wire name (`change` in a conversation event).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Invited => "invited",
            Self::Added => "added",
            Self::InvitationAccepted => "invitation_accepted",
            Self::InvitationDeclined => "invitation_declined",
            Self::InvitationCancelled => "invitation_cancelled",
            Self::Left => "left",
            Self::Removed => "removed",
            Self::Renamed => "renamed",
            Self::DescriptionChanged => "description_changed",
            Self::MembersChanged => "members_changed",
            Self::OwnersChanged => "owners_changed",
            Self::Read => "read",
        }
    }
}
