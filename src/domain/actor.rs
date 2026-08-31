//! Actor and organization identity types.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use sqlx::Type;
use thiserror::Error;

/// Whether an actor is a human Carbon or an AI Silicon.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "actor_kind", rename_all = "snake_case")]
pub enum ActorType {
    /// A human account.
    Carbon,
    /// An AI-agent account.
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

/// Stable public organization identifier.
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct OrganizationId(String);

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
    /// Borrows the normalized identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl OrganizationId {
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
