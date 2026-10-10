//! Account identity types.
//!
//! An [`ActorRef`]'s `id` is the account's **storage key**: its Silicon Accounts uuid
//! (`zQo`) for everything written by DM 0.15, or an IAM-era public id (`c:saket`) on legacy
//! rows not yet linked by `dm-migrate link-identities`. Public ids are resolved to uuids at
//! the API boundary and rendered back from the accounts cache in responses.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use sqlx::Type;
use thiserror::Error;

/// Whether an account is a Carbon or a Silicon.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "actor_kind", rename_all = "snake_case")]
pub enum ActorType {
    /// A Carbon (`c:` id).
    Carbon,
    /// A Silicon (`si:` id).
    Silicon,
}

impl fmt::Display for ActorType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Carbon => formatter.write_str("carbon"),
            Self::Silicon => formatter.write_str("silicon"),
        }
    }
}

impl ActorType {
    /// Stable lowercase database and wire representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Carbon => "carbon",
            Self::Silicon => "silicon",
        }
    }
}

impl FromStr for ActorType {
    type Err = IdentityError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "carbon" => Ok(Self::Carbon),
            "silicon" => Ok(Self::Silicon),
            _ => Err(IdentityError::ActorType),
        }
    }
}

/// Stable public identifier for a Carbon or Silicon.
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct ActorId(String);

/// Storage partition of a conversation and its rows (the `organization_id` column).
///
/// Legacy rows keep the IAM organization they were written in; every conversation created
/// by DM 0.15 lives in [`ACCOUNTS_PARTITION`]. Partitions are internal: no request or
/// response names one.
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct OrganizationId(String);

/// The partition of everything created from DM 0.15 on (IAM handles never contain `@`).
pub const ACCOUNTS_PARTITION: &str = "@accounts";

/// Whether `value` is a canonical UUID or a legacy Accounts alphanumeric key.
#[must_use]
pub fn is_account_uuid(value: &str) -> bool {
    ((3..=64).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_alphanumeric()))
        || (value.len() == 36
            && uuid::Uuid::parse_str(value).is_ok_and(|id| id.to_string() == value))
}

/// A typed actor reference returned by the public API.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct ActorRef {
    /// Actor category.
    #[serde(rename = "type")]
    pub actor_type: ActorType,
    /// Stable actor identifier.
    pub id: ActorId,
}

/// Invalid public identity.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum IdentityError {
    /// Actor type is not part of the protocol.
    #[error("actor type must be carbon or silicon")]
    ActorType,
    /// Identifier has an invalid length or contains a control character.
    #[error("identifier must contain 1 to 255 non-control characters")]
    Invalid,
}

impl ActorId {
    /// Separates an optional ISI from a Silicon address (`isi@si:handle`).
    ///
    /// # Errors
    /// Returns an error for an invalid public ID or ISI prefix.
    pub fn address_parts(&self) -> Result<(&str, Option<&str>), &'static str> {
        let (actor, isi) = match self.0.split_once('@') {
            Some((isi, actor)) => {
                if isi.is_empty()
                    || isi
                        .chars()
                        .any(|c| c.is_whitespace() || matches!(c, ':' | '@'))
                    || !(actor.starts_with("si:") || is_account_uuid(actor))
                {
                    return Err("silicon address must be ISI@si:handle with a non-empty ISI");
                }
                (actor, Some(isi))
            }
            None => (self.0.as_str(), None),
        };
        if is_account_uuid(actor) {
            // Internal form: the account uuid DM stores in place of the public id.
            return Ok((actor, isi));
        }
        let handle = actor
            .strip_prefix("si:")
            .or_else(|| actor.strip_prefix("c:"));
        if !handle.is_some_and(|handle| {
            (3..=if actor.starts_with("c:") { 30 } else { 50 }).contains(&handle.len())
                && handle.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'-' | b'_')
                })
        }) {
            return Err("actor ID must be c:handle or si:handle");
        }
        Ok((actor, isi))
    }
    /// The account (public id or uuid) underlying an optional ISI address.
    ///
    /// # Errors
    /// Returns an error when the routing address or underlying account is invalid.
    pub fn base_actor_id(&self) -> Result<Self, &'static str> {
        Self::try_from(self.address_parts()?.0.to_owned()).map_err(|_| "invalid actor address")
    }
    /// Checks both the underlying account and whether ISI is valid for its kind.
    #[must_use]
    pub fn addresses(&self, actor: &ActorRef) -> bool {
        self.address_parts().is_ok_and(|(base, isi)| {
            base == actor.id.as_str() && (isi.is_none() || actor.actor_type == ActorType::Silicon)
        })
    }

    /// Borrows the normalized identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Wraps a storage key read from DM's own database (an account uuid, which the schema
    /// constrains to `[A-Za-z0-9]{3,64}`), without re-validating it.
    pub(crate) fn from_storage_key(value: String) -> Self {
        Self(value)
    }
}

impl OrganizationId {
    /// The partition of everything created by DM 0.15.
    #[must_use]
    pub fn accounts() -> Self {
        Self(ACCOUNTS_PARTITION.to_owned())
    }

    /// Borrows the normalized identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

macro_rules! impl_identifier {
    ($type:ty) => {
        impl TryFrom<String> for $type {
            type Error = IdentityError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                let normalized = value.trim();
                if normalized.is_empty()
                    || normalized.len() > 255
                    || normalized.chars().any(char::is_control)
                {
                    return Err(IdentityError::Invalid);
                }
                Ok(Self(normalized.to_owned()))
            }
        }

        impl From<$type> for String {
            fn from(value: $type) -> Self {
                value.0
            }
        }

        impl fmt::Display for $type {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl FromStr for $type {
            type Err = IdentityError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::try_from(value.to_owned())
            }
        }
    };
}

impl_identifier!(ActorId);
impl_identifier!(OrganizationId);

#[cfg(test)]
mod tests {
    use std::str::FromStr as _;

    use super::{ActorId, IdentityError, OrganizationId};

    #[test]
    fn identifiers_trim_surrounding_whitespace() {
        assert_eq!(
            ActorId::from_str("  carbon-1  ").map(|value| value.to_string()),
            Ok("carbon-1".to_owned())
        );
        assert_eq!(
            OrganizationId::from_str(" org ").map(|value| value.to_string()),
            Ok("org".to_owned())
        );
    }

    #[test]
    fn identifiers_reject_controls_and_empty_values() {
        assert_eq!(ActorId::from_str("\n"), Err(IdentityError::Invalid));
        assert_eq!(OrganizationId::from_str(""), Err(IdentityError::Invalid));
    }
}

#[cfg(test)]
mod address_tests {
    use super::*;
    #[test]
    fn isi_is_routing_and_never_identity_authority() -> Result<(), Box<dyn std::error::Error>> {
        let address: ActorId = "deliberate@si:cos".parse()?;
        assert_eq!(address.address_parts()?, ("si:cos", Some("deliberate")));
        assert_eq!(address.base_actor_id()?.as_str(), "si:cos");
        assert!(address.addresses(&ActorRef {
            actor_type: ActorType::Silicon,
            id: "si:cos".parse()?
        }));
        assert!(!address.addresses(&ActorRef {
            actor_type: ActorType::Carbon,
            id: "si:cos".parse()?
        }));
        assert!(!address.addresses(&ActorRef {
            actor_type: ActorType::Silicon,
            id: "si:other".parse()?
        }));
        assert!(
            "human@example.com"
                .parse::<ActorId>()?
                .address_parts()
                .is_err()
        );
        assert_eq!(
            "c:human".parse::<ActorId>()?.address_parts()?,
            ("c:human", None)
        );
        for invalid in ["@si:cos", "a@b@si:cos", "a@:tos", "a@cos:", "a b@si:cos"] {
            assert!(invalid.parse::<ActorId>()?.address_parts().is_err());
        }
        Ok(())
    }
}

#[cfg(test)]
mod uuid128_tests {
    #[test]
    fn accepts_canonical_accounts_uuid_alongside_legacy_keys() {
        for valid in ["a8K", "a750a68a-1bc2-4b3f-888e-0349c9d7289a"] {
            assert!(super::is_account_uuid(valid));
        }
        for invalid in [
            "c:ada",
            "a750a68a-1bc2-4b3f-888e-0349c9d7289z",
            "not-an-account",
        ] {
            assert!(!super::is_account_uuid(invalid));
        }
    }
}
