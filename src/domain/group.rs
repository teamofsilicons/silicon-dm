//! Named groups: co-equal owners, accepted members and pending invitations.
use super::{ActorId, ActorRef};
use serde::{Deserialize, Serialize};

/// Editable group settings.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GroupSettings {
    /// Human-readable group name.
    pub name: String,
    /// Purpose and context for members.
    #[serde(default)]
    pub description: String,
}
impl GroupSettings {
    /// Validates limits and trims the name.
    /// # Errors
    /// Rejects empty or oversized names, control characters and oversized descriptions.
    pub fn validate(&mut self) -> crate::AppResult<()> {
        self.name = self.name.trim().to_owned();
        if self.name.is_empty()
            || self.name.chars().count() > 120
            || self.name.chars().any(char::is_control)
            || self.description.chars().count() > 4000
            || self.description.contains('\0')
        {
            return Err(crate::AppError::validation(
                "group name must be 1–120 characters without control characters, and the description at most 4000 characters",
            ));
        }
        Ok(())
    }
}
/// Group creation input. The creator becomes the first owner; `member_ids` are invited
/// (each invitee accepts or declines; a custodian's own Silicons join at once).
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GroupCreate {
    /// Name and description.
    #[serde(flatten)]
    pub settings: GroupSettings,
    /// Accounts to invite, by `c:`/`si:` id (or uuid).
    #[serde(default)]
    pub member_ids: Vec<ActorId>,
}
/// Group metadata included on group conversations.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct GroupDetails {
    /// Current settings.
    #[serde(flatten)]
    pub settings: GroupSettings,
    /// Optimistic concurrency version for settings changes.
    pub version: i64,
    /// Co-equal owners; at least one.
    pub owners: Vec<ActorRef>,
    /// Accepted members, owners included.
    pub members: Vec<ActorRef>,
    /// Invitations not yet accepted. Only owners (and the custodians acting for them) see
    /// these; other members get an empty list.
    #[serde(default)]
    pub pending_invitations: Vec<ActorRef>,
}
