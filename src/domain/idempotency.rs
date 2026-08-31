//! Idempotency-key validation.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Client-generated key used to make a mutation retry-safe.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct IdempotencyKey(String);

/// Invalid idempotency key.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[error("idempotency key must contain 8 to 255 visible ASCII characters")]
pub struct IdempotencyKeyError;

impl IdempotencyKey {
    /// Borrows the key.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for IdempotencyKey {
    type Error = IdempotencyKeyError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if !(8..=255).contains(&value.len()) || !value.as_bytes().iter().all(u8::is_ascii_graphic) {
            return Err(IdempotencyKeyError);
        }
        Ok(Self(value))
    }
}

impl From<IdempotencyKey> for String {
    fn from(value: IdempotencyKey) -> Self {
        value.0
    }
}

impl FromStr for IdempotencyKey {
    type Err = IdempotencyKeyError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::try_from(value.to_owned())
    }
}

impl fmt::Display for IdempotencyKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}
