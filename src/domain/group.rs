//! Stable named groups with invitation, tag, and public access rules.
use super::{ActorId, ActorRef};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Editable group policy. Tag UUIDs remain stable across IAM tag renames.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct GroupSettings {
    /// Human-readable group name.
    pub name: String,
    /// Purpose and context for members.
    #[serde(default)]
    pub description: String,
    /// All active organization Carbons have access. Silicons require invitations.
    #[serde(default)]
    pub is_public: bool,
    /// Any matching IAM tag grants access to a private group.
    #[serde(default)]
    pub tag_ids: Vec<Uuid>,
}
impl GroupSettings {
    /// Validates limits and canonicalizes tag IDs for idempotency.
    /// # Errors
    /// Rejects empty names, control characters, excessive description/tags, or nil IDs.
    pub fn validate(&mut self) -> crate::AppResult<()> {
        self.name = self.name.trim().to_owned();
        if self.name.is_empty()
            || self.name.chars().count() > 120
            || self.name.chars().any(char::is_control)
            || self.description.chars().count() > 4000
            || self.description.contains('\0')
            || self.tag_ids.len() > 100
            || self.tag_ids.iter().any(Uuid::is_nil)
        {
            return Err(crate::AppError::validation(
                "group name must be 1–120 characters, description at most 4000, and tag_ids at most 100 non-nil IAM tag UUIDs",
            ));
        }
        self.tag_ids.sort_unstable();
        self.tag_ids.dedup();
        Ok(())
    }
}
/// Group creation input; creator is explicitly invited automatically.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct GroupCreate {
    /// Initial group policy.
    #[serde(flatten)]
    pub settings: GroupSettings,
    /// Explicit invitations for active IAM organization actors.
    #[serde(default)]
    pub member_ids: Vec<ActorId>,
}
/// Group metadata included additively on group conversations.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct GroupDetails {
    /// Current group policy.
    #[serde(flatten)]
    pub settings: GroupSettings,
    /// Optimistic concurrency version for policy changes.
    pub version: i64,
    /// Explicit invitations, independent of tag or public access.
    pub invited_members: Vec<ActorRef>,
}
