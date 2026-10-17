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
/// Longest group name, in characters (after trimming).
pub const MAX_GROUP_NAME_CHARACTERS: usize = 120;
/// Longest group description, in characters.
pub const MAX_GROUP_DESCRIPTION_CHARACTERS: usize = 4000;

impl GroupSettings {
    /// Validates limits and trims the name.
    /// # Errors
    /// Rejects empty or oversized names, control characters and oversized descriptions,
    /// each with its own words.
    pub fn validate(&mut self) -> crate::AppResult<()> {
        self.name = self.name.trim().to_owned();
        let name_length = self.name.chars().count();
        let description_length = self.description.chars().count();
        let problem = if self.name.is_empty() {
            "A group needs a name of 1 to 120 characters; this one is empty.".to_owned()
        } else if name_length > MAX_GROUP_NAME_CHARACTERS {
            format!(
                "A group's name is at most {MAX_GROUP_NAME_CHARACTERS} characters; this one has {name_length}."
            )
        } else if self.name.chars().any(char::is_control) {
            "A group's name is one line: it cannot contain control characters such as line breaks or tabs.".to_owned()
        } else if description_length > MAX_GROUP_DESCRIPTION_CHARACTERS {
            format!(
                "A group's description is at most {MAX_GROUP_DESCRIPTION_CHARACTERS} characters; this one has {description_length}."
            )
        } else if self.description.contains('\0') {
            "A group's description cannot contain the NUL character (\\u0000).".to_owned()
        } else {
            return Ok(());
        };
        Err(crate::AppError::invalid(problem)
            .with_hint("Fix the name or description and send the request again."))
    }

    /// A new group's address is made from its name: `g:` and its ASCII letters and digits,
    /// lowercased, with a dash for everything else (`Launch 🎉` → `g:launch`). A name
    /// without any of them has no address, so DM refuses it here with words, before the
    /// database would. A rename keeps the address, so this applies only to creation.
    ///
    /// # Errors
    /// A name with no letter or digit from a to z, A to Z or 0 to 9.
    pub fn validate_address(&self) -> crate::AppResult<()> {
        if self.name.chars().any(|c| c.is_ascii_alphanumeric()) {
            return Ok(());
        }
        Err(unaddressable_name(&self.name))
    }
}

/// The refusal for a new group whose name cannot make an address. `name` is empty when
/// the database's own rule caught it (its error does not carry the name).
#[must_use]
pub fn unaddressable_name(name: &str) -> crate::AppError {
    const RULE: &str = "A group's address is made from its name, so the name needs a letter or a digit (a to z, 0 to 9)";
    let message = if name.is_empty() {
        format!("{RULE}.")
    } else {
        let shown: String = name.chars().take(MAX_GROUP_NAME_CHARACTERS).collect();
        format!("{RULE}: \"{shown}\" has none.")
    };
    crate::AppError::invalid(message)
    .with_hint(
        "Add one, for example \"Launch 🎉\" (its address is g:launch). A rename keeps the address, so the group can take any name afterwards.",
    )
    .with_details(serde_json::json!({"field": "name", "rule": "needs_letter_or_digit"}))
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
