//! Shared message validation for REST and realtime commands.

use crate::{AppError, AppResult, application::state::AppState, domain::MessageCreate};

/// Validates supplied content without fetching or rewriting attachment links.
///
/// # Errors
/// Returns a validation error for malformed or oversized content.
pub fn prepare_message_content(
    _state: &AppState,
    content: MessageCreate,
) -> AppResult<MessageCreate> {
    content.validate().map_err(AppError::validation)?;
    Ok(content)
}

/// Validates a stable realtime device/consumer identifier.
///
/// # Errors
///
/// Returns validation failure for an empty, overlong, or control-containing ID.
pub fn validate_device_id(device_id: &str) -> AppResult<()> {
    if device_id.is_empty() || device_id.len() > 255 || device_id.chars().any(char::is_control) {
        return Err(AppError::validation(
            "device_id must contain 1 to 255 non-control characters",
        ));
    }
    Ok(())
}
