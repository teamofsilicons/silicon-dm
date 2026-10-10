//! What DM knows about a Silicon Accounts account. Accounts is authoritative; this is a
//! cache kept fresh by verified tokens, app lookups and signed webhooks.

use serde::Serialize;

use super::{ActorId, ActorRef, ActorType};

/// One account as DM currently knows it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Account {
    /// Permanent account identifier (the storage key).
    pub uuid: String,
    /// Carbon or Silicon.
    #[serde(rename = "kind")]
    pub kind: ActorType,
    /// Current public id (`c:saket`, `si:scout`); `None` once the account is deleted.
    pub id: Option<String>,
    /// Display name, when Accounts shared one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Profile photo URL, when Accounts shared one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pfp_url: Option<String>,
    /// A Silicon's custodian (uuid).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custodian: Option<String>,
    /// The account was deleted in Silicon Accounts.
    pub deleted: bool,
}

impl Account {
    /// The storage reference of this account.
    #[must_use]
    pub fn actor(&self) -> ActorRef {
        ActorRef {
            actor_type: self.kind,
            id: ActorId::from_storage_key(self.uuid.clone()),
        }
    }

    /// The id people see: the current public id, or the uuid for a deleted account.
    #[must_use]
    pub fn display_id(&self) -> &str {
        self.id.as_deref().unwrap_or(&self.uuid)
    }

    /// Whether this account is a Silicon.
    #[must_use]
    pub fn is_silicon(&self) -> bool {
        self.kind == ActorType::Silicon
    }
}
