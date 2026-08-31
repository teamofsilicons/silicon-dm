//! Shared message-content preparation for REST and realtime commands.

use url::Url;
use uuid::Uuid;

use crate::{
    AppError, AppResult,
    application::{auth::AuthContext, ports::DelegationRequest, state::AppState},
    domain::{Attachment, DraftInput, MessageCreate},
};

/// Validates permanent content references and completes blocking voice
/// transcription before a message is durably accepted.
///
/// # Errors
///
/// Returns validation or dependency failures without persisting partial state.
pub async fn prepare_message_content(
    state: &AppState,
    authority: &AuthContext,
    mut content: MessageCreate,
    idempotency_key: &str,
) -> AppResult<(MessageCreate, Option<u64>)> {
    content.validate().map_err(AppError::validation)?;
    validate_message_urls(&content, &state.settings.providers.briefcase_base_url)?;
    let transcription = match content.voice.as_ref() {
        Some(voice) => {
            let credential = state
                .identity
                .exchange_actor_credential(
                    authority,
                    &DelegationRequest::waveform_stt(
                        &state.settings.providers.waveform_iam_audience,
                    ),
                )
                .await?;
            Some(
                state
                    .transcription
                    .transcribe(
                        &voice.permanent_url,
                        &authority.organization_id,
                        &credential,
                        idempotency_key,
                    )
                    .await?,
            )
        }
        None => None,
    };
    let duration = transcription
        .as_ref()
        .and_then(|result| result.duration_milliseconds);
    if let Some(transcription) = transcription {
        content.voice_transcript = transcription.transcript;
        content.validate().map_err(AppError::validation)?;
    }
    Ok((content, duration))
}

/// Validates every permanent Briefcase reference in a draft.
///
/// # Errors
///
/// Returns validation failure for a non-canonical entry URL.
pub fn validate_draft_urls(input: &DraftInput, briefcase_base_url: &Url) -> AppResult<()> {
    validate_attachment_urls(&input.attachments, input.voice.as_ref(), briefcase_base_url)
}

/// Validates a canonical permanent Briefcase entry URL without issuing a
/// temporary-URL request.
///
/// # Errors
///
/// Returns validation failure for a different origin, credentials, query,
/// fragment, base path, or non-UUID entry identifier.
pub fn validate_briefcase_permanent_url(permanent_url: &Url, base_url: &Url) -> AppResult<Uuid> {
    if permanent_url.scheme() != "https"
        || permanent_url.host_str() != base_url.host_str()
        || permanent_url.port_or_known_default() != base_url.port_or_known_default()
        || !permanent_url.username().is_empty()
        || permanent_url.password().is_some()
        || permanent_url.query().is_some()
        || permanent_url.fragment().is_some()
        || permanent_url.path().ends_with('/')
    {
        return Err(AppError::validation(
            "attachment URL must be a canonical Briefcase permanent URL",
        ));
    }

    let base_path = base_url.path().trim_end_matches('/');
    let prefix = if base_path.is_empty() {
        "/entries/".to_owned()
    } else {
        format!("{base_path}/entries/")
    };
    let entry_id = permanent_url
        .path()
        .strip_prefix(&prefix)
        .filter(|value| !value.is_empty() && !value.contains('/'))
        .ok_or_else(|| AppError::validation("attachment URL must identify one Briefcase entry"))?;
    Uuid::parse_str(entry_id)
        .map_err(|_| AppError::validation("attachment URL contains an invalid Briefcase entry ID"))
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

fn validate_message_urls(content: &MessageCreate, briefcase_base_url: &Url) -> AppResult<()> {
    validate_attachment_urls(
        &content.attachments,
        content.voice.as_ref(),
        briefcase_base_url,
    )
}

fn validate_attachment_urls(
    attachments: &[Attachment],
    voice: Option<&Attachment>,
    briefcase_base_url: &Url,
) -> AppResult<()> {
    for attachment in attachments.iter().chain(voice) {
        validate_briefcase_permanent_url(&attachment.permanent_url, briefcase_base_url)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{validate_briefcase_permanent_url, validate_device_id};

    #[test]
    fn permanent_url_is_scoped_to_the_configured_entry_collection() {
        let base = "https://briefcase.example/api/v1".parse();
        let accepted =
            "https://briefcase.example/api/v1/entries/018f0d52-7b2a-7e29-a41d-7c02b93f6f42".parse();
        let rejected =
            "https://briefcase.example/entries/018f0d52-7b2a-7e29-a41d-7c02b93f6f42".parse();
        assert!(
            base.as_ref()
                .ok()
                .zip(accepted.as_ref().ok())
                .is_some_and(|(base, accepted)| {
                    validate_briefcase_permanent_url(accepted, base).is_ok()
                })
        );
        assert!(
            base.as_ref()
                .ok()
                .zip(rejected.as_ref().ok())
                .is_some_and(|(base, rejected)| {
                    validate_briefcase_permanent_url(rejected, base).is_err()
                })
        );
    }

    #[test]
    fn device_ids_reject_control_characters() {
        assert!(validate_device_id("device-1").is_ok());
        assert!(validate_device_id("bad\ndevice").is_err());
    }
}
