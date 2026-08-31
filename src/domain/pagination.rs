//! Validated cursor pagination.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{AppError, AppResult};

/// Validated page request shared by list endpoints.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct PageRequest {
    /// Opaque cursor returned by the preceding page.
    pub cursor: Option<String>,
    /// Requested page length.
    pub limit: Option<u16>,
}

/// Versioned internal cursor value.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Cursor {
    version: u8,
    kind: String,
    position: CursorPosition,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum CursorPosition {
    Sequence {
        value: i64,
    },
    Activity {
        #[serde(with = "time::serde::rfc3339")]
        updated_at: OffsetDateTime,
        id: Uuid,
    },
}

impl PageRequest {
    /// Returns a bounded page size with a default of 50.
    ///
    /// # Errors
    ///
    /// Rejects zero and values above the public maximum of 100.
    pub fn validated_limit(&self) -> AppResult<u16> {
        let limit = self.limit.unwrap_or(50);
        if !(1..=100).contains(&limit) {
            return Err(AppError::validation("limit must be between 1 and 100"));
        }
        Ok(limit)
    }
}

impl Cursor {
    /// Creates a cursor for one endpoint family and numeric position.
    #[must_use]
    pub fn new(kind: impl Into<String>, value: i64) -> Self {
        Self {
            version: 1,
            kind: kind.into(),
            position: CursorPosition::Sequence { value },
        }
    }

    /// Creates a cursor for a descending activity timestamp and UUID tuple.
    #[must_use]
    pub fn activity(kind: impl Into<String>, updated_at: OffsetDateTime, id: Uuid) -> Self {
        Self {
            version: 1,
            kind: kind.into(),
            position: CursorPosition::Activity { updated_at, id },
        }
    }

    /// Encodes the cursor into its opaque public representation.
    ///
    /// # Errors
    ///
    /// Returns an internal error if serialization unexpectedly fails.
    pub fn encode(&self) -> AppResult<String> {
        let bytes = serde_json::to_vec(self).map_err(AppError::internal)?;
        Ok(URL_SAFE_NO_PAD.encode(bytes))
    }

    /// Decodes a cursor and verifies that it belongs to the expected endpoint.
    ///
    /// # Errors
    ///
    /// Returns a validation error for malformed, unsupported, or cross-endpoint
    /// cursors.
    pub fn decode(encoded: &str, expected_kind: &str) -> AppResult<Self> {
        let bytes = URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_| AppError::validation("cursor is invalid"))?;
        let cursor: Self = serde_json::from_slice(&bytes)
            .map_err(|_| AppError::validation("cursor is invalid"))?;
        let valid_position = match cursor.position {
            CursorPosition::Sequence { value } => value >= 0,
            CursorPosition::Activity { .. } => true,
        };
        if cursor.version != 1 || cursor.kind != expected_kind || !valid_position {
            return Err(AppError::validation("cursor is invalid for this resource"));
        }
        Ok(cursor)
    }

    /// Returns a sequence position when this cursor contains one.
    #[must_use]
    pub fn sequence(&self) -> Option<i64> {
        match self.position {
            CursorPosition::Sequence { value } => Some(value),
            CursorPosition::Activity { .. } => None,
        }
    }

    /// Returns an activity ordering tuple when this cursor contains one.
    #[must_use]
    pub fn activity_position(&self) -> Option<(OffsetDateTime, Uuid)> {
        match self.position {
            CursorPosition::Sequence { .. } => None,
            CursorPosition::Activity { updated_at, id } => Some((updated_at, id)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Cursor;

    #[test]
    fn cursor_round_trip_preserves_position() {
        let decoded = Cursor::new("messages", 42)
            .encode()
            .and_then(|value| Cursor::decode(&value, "messages"));
        assert!(matches!(decoded, Ok(cursor) if cursor == Cursor::new("messages", 42)));
    }

    #[test]
    fn cursor_cannot_cross_resource_families() {
        let result = Cursor::new("messages", 42)
            .encode()
            .and_then(|value| Cursor::decode(&value, "conversations"));
        assert!(result.is_err());
    }
}
