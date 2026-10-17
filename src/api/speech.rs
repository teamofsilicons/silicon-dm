//! Voices, listening to messages, rendering scripts and transcribing audio.

use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use axum::{
    Json,
    body::Body,
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse as _, Response},
};
use futures::StreamExt as _;
use serde::Deserialize;
use serde_json::{Value, json};
use time::format_description::well_known::Rfc3339;

use super::{
    extract::{ApiJson, ApiPath, ApiQuery, Authenticated, OptionalIdempotency},
    handlers::{MessagePath, looked_after},
};
use crate::{
    AppError, AppResult,
    application::{
        ports::TranscriptionRequest,
        speech::{
            Rendered, SpeechSource, account_voice, invalid_script, may_change_voice, message_audio,
            render, voice_not_found,
        },
        state::AppState,
    },
    domain::{
        Account, ActorRef, ActorType,
        speech::{DEFAULT_VOICE_RULE, validate_script},
    },
    infrastructure::postgres::limit_error,
};

/// The media types a transcription accepts (aliases clients commonly send map onto them).
const ACCEPTED_AUDIO: [&str; 8] = [
    "audio/webm",
    "audio/ogg",
    "audio/wav",
    "audio/mpeg",
    "audio/mp4",
    "audio/x-m4a",
    "audio/aac",
    "audio/flac",
];

/// `GET /api/v1/voices` (no sign-in): the catalog every account's voice comes from.
pub(super) async fn voices(State(state): State<AppState>) -> Json<Value> {
    let speech = &state.settings.speech;
    Json(json!({
        "model": speech.model,
        "enabled": state.speech.enabled(),
        "items": speech.voices.voices().iter().map(|voice| json!({
            "key": voice.key,
            "name": voice.name,
            "description": voice.description,
            "voice_id": voice.voice_id,
        })).collect::<Vec<_>>(),
        "default_rule": DEFAULT_VOICE_RULE,
    }))
}

#[derive(Debug, Deserialize)]
pub(super) struct AccountPath {
    account: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct VoiceInput {
    voice: String,
}

/// `GET /api/v1/voices/{account}`: an account's voice, whether it was chosen and by whom, and
/// whether the caller may change it.
pub(super) async fn voice(
    State(state): State<AppState>,
    Authenticated(context): Authenticated,
    ApiPath(path): ApiPath<AccountPath>,
) -> AppResult<Json<Value>> {
    let account = state.directory().resolve(&path.account).await?;
    voice_view(&state, &context.actor, &account).await.map(Json)
}

/// `PUT /api/v1/voices/{account}` `{"voice": "brittney"}`: the account itself, or a Silicon's
/// custodian, chooses its voice.
pub(super) async fn set_voice(
    State(state): State<AppState>,
    Authenticated(context): Authenticated,
    ApiPath(path): ApiPath<AccountPath>,
    OptionalIdempotency(key): OptionalIdempotency,
    ApiJson(input): ApiJson<VoiceInput>,
) -> AppResult<Json<Value>> {
    let account = state.directory().resolve(&path.account).await?;
    require_may_change(&state, &context.actor, &account).await?;
    let catalog = &state.settings.speech.voices;
    let voice = catalog
        .get(&input.voice)
        .ok_or_else(|| voice_not_found(&input.voice, catalog))?;
    state
        .store
        .change_voice(
            &account.uuid,
            Some(&voice.key),
            &context.actor,
            key.as_ref(),
        )
        .await?;
    voice_view(&state, &context.actor, &account).await.map(Json)
}

/// `DELETE /api/v1/voices/{account}`: back to the account's default voice.
pub(super) async fn reset_voice(
    State(state): State<AppState>,
    Authenticated(context): Authenticated,
    ApiPath(path): ApiPath<AccountPath>,
    OptionalIdempotency(key): OptionalIdempotency,
) -> AppResult<Json<Value>> {
    let account = state.directory().resolve(&path.account).await?;
    require_may_change(&state, &context.actor, &account).await?;
    state
        .store
        .change_voice(&account.uuid, None, &context.actor, key.as_ref())
        .await?;
    voice_view(&state, &context.actor, &account).await.map(Json)
}

async fn require_may_change(
    state: &AppState,
    caller: &ActorRef,
    account: &Account,
) -> AppResult<()> {
    if may_change_voice(caller, account) {
        return Ok(());
    }
    let id = account.display_id();
    let hint = if account.is_silicon() {
        let custodian = match &account.custodian {
            Some(uuid) => state
                .store
                .stored_account(uuid)
                .await?
                .and_then(|stored| stored.current_id)
                .map_or_else(
                    || "its custodian".to_owned(),
                    |id| format!("its custodian {id}"),
                ),
            None => "its custodian".to_owned(),
        };
        format!("Only {id} itself or {custodian} can choose its voice; ask them.")
    } else {
        format!("Only {id} can choose their own voice.")
    };
    Err(AppError::forbidden(
        "voice_change_forbidden",
        format!("You cannot change the voice of {id}."),
    )
    .with_hint(hint))
}

async fn voice_view(state: &AppState, caller: &ActorRef, account: &Account) -> AppResult<Value> {
    let current = account_voice(state, &account.uuid).await?;
    let chosen_by = current.chosen.as_ref().map(|chosen| {
        // The chooser is the account itself or, for a Silicon, its custodian (a Carbon).
        let kind = if chosen.chosen_by_uuid == account.uuid {
            account.kind
        } else {
            ActorType::Carbon
        };
        json!({"type": kind.as_str(), "id": chosen.chosen_by_uuid})
    });
    Ok(json!({
        "account": {"type": account.kind.as_str(), "id": account.uuid},
        "voice": {
            "key": current.voice.key,
            "name": current.voice.name,
            "description": current.voice.description,
        },
        "chosen": current.chosen.is_some(),
        "chosen_by": chosen_by,
        "updated_at": current
            .chosen
            .as_ref()
            .and_then(|chosen| chosen.updated_at.format(&Rfc3339).ok()),
        "can_change": may_change_voice(caller, account),
    }))
}

/// `GET /api/v1/conversations/{conversation}/messages/{message}/speech[?for=si:x]`: the
/// message as audio. A spoken message plays its script in the voice pinned on it; any other
/// message with text plays its text in the sender's current voice.
pub(super) async fn message_speech(
    State(state): State<AppState>,
    Authenticated(context): Authenticated,
    ApiPath(path): ApiPath<MessagePath>,
    ApiQuery(query): ApiQuery<SpeechQuery>,
    headers: HeaderMap,
) -> AppResult<Response> {
    if let Some(silicon) = &query.for_account {
        looked_after(&state, &context, silicon).await?;
    }
    let message = state
        .store
        .get_message(&context.actor, path.conversation_id, path.message_id)
        .await?;
    let (source, rendered) = message_audio(&state, &context.actor, &message).await?;
    if matches!(source, SpeechSource::Speech { .. })
        && message
            .speech
            .as_ref()
            .and_then(crate::domain::Speech::cache_key)
            == Some(rendered.cache_key)
    {
        // Listening rendered (or found) the message's own speech: a pending or failed
        // pre-render is ready now.
        state
            .store
            .complete_speech_render(message.id, &rendered.cache_key, &rendered.meta)
            .await?;
    }
    audio_response(&state, &headers, &rendered, source.voice_key(), true).await
}

/// `?for=si:x`. Other parameters (players add cache busters) are ignored.
#[derive(Debug, Default, Deserialize)]
pub(super) struct SpeechQuery {
    #[serde(rename = "for")]
    for_account: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RenderInput {
    script: String,
    #[serde(default)]
    voice: Option<String>,
}

/// `POST /api/v1/speech` `{"script", "voice"?}`: a script as audio (previews, voice samples,
/// `dm speech render`), in the given voice or the caller's.
pub(super) async fn render_speech(
    State(state): State<AppState>,
    Authenticated(context): Authenticated,
    ApiJson(input): ApiJson<RenderInput>,
) -> AppResult<Response> {
    state.speech.provider()?;
    validate_script(&input.script).map_err(invalid_script)?;
    let catalog = &state.settings.speech.voices;
    let voice = match &input.voice {
        Some(key) => catalog
            .get(key)
            .cloned()
            .ok_or_else(|| voice_not_found(key, catalog))?,
        None => {
            account_voice(&state, context.actor.id.as_str())
                .await?
                .voice
        }
    };
    let rendered = render(
        &state,
        Some(&context.actor),
        &input.script,
        &voice.voice_id,
        &state.settings.speech.model,
    )
    .await?;
    audio_response(&state, &HeaderMap::new(), &rendered, &voice.key, false).await
}

/// One byte range of a WAV.
enum Requested {
    Full,
    Part { start: u64, length: u64 },
    Unsatisfiable,
}

/// A single `bytes=` range (`a-b`, `a-`, `-n`). Several ranges, or anything malformed, are
/// ignored and the whole file is sent, as HTTP allows. A stale `If-Range` also sends it whole.
fn requested_range(headers: &HeaderMap, etag: &str, total: u64) -> Requested {
    let Some(range) = headers
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok())
    else {
        return Requested::Full;
    };
    if let Some(validator) = headers
        .get(header::IF_RANGE)
        .and_then(|value| value.to_str().ok())
        && validator.trim() != etag
    {
        return Requested::Full;
    }
    let Some(spec) = range.trim().strip_prefix("bytes=") else {
        return Requested::Full;
    };
    if spec.contains(',') {
        return Requested::Full;
    }
    let Some((first, last)) = spec.trim().split_once('-') else {
        return Requested::Full;
    };
    let (first, last) = (first.trim(), last.trim());
    let parse = |value: &str| value.parse::<u64>().ok();
    match (first.is_empty(), last.is_empty()) {
        (true, false) => match parse(last) {
            Some(0) => Requested::Unsatisfiable,
            Some(suffix) if total > 0 => {
                let length = suffix.min(total);
                Requested::Part {
                    start: total - length,
                    length,
                }
            }
            Some(_) => Requested::Unsatisfiable,
            None => Requested::Full,
        },
        (false, true) => match parse(first) {
            Some(start) if start < total => Requested::Part {
                start,
                length: total - start,
            },
            Some(_) => Requested::Unsatisfiable,
            None => Requested::Full,
        },
        (false, false) => match (parse(first), parse(last)) {
            (Some(start), Some(end)) if start <= end && start < total => Requested::Part {
                start,
                length: end.min(total - 1) - start + 1,
            },
            (Some(start), Some(end)) if start <= end => Requested::Unsatisfiable,
            _ => Requested::Full,
        },
        (true, true) => Requested::Full,
    }
}

fn matches_etag(headers: &HeaderMap, etag: &str) -> bool {
    headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value.split(',').any(|candidate| {
                matches!(candidate.trim(), "*") || candidate.trim().trim_start_matches("W/") == etag
            })
        })
}

async fn audio_response(
    state: &AppState,
    headers: &HeaderMap,
    rendered: &Rendered,
    voice_key: &str,
    conditional: bool,
) -> AppResult<Response> {
    let total = rendered.meta.bytes;
    let etag = format!("\"{}\"", rendered.meta.etag);
    let mut response = if conditional && matches_etag(headers, &etag) {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        let requested = if conditional {
            requested_range(headers, &etag, total)
        } else {
            Requested::Full
        };
        let (status, start, length) = match requested {
            Requested::Full => (StatusCode::OK, 0, total),
            Requested::Part { start, length } => (StatusCode::PARTIAL_CONTENT, start, length),
            Requested::Unsatisfiable => {
                let mut response = AppError::refused(
                    StatusCode::RANGE_NOT_SATISFIABLE,
                    "range_not_satisfiable",
                    format!(
                        "The audio is {total} bytes long; the requested range lies outside it."
                    ),
                )
                .into_response();
                response.headers_mut().insert(
                    header::CONTENT_RANGE,
                    header_value(&format!("bytes */{total}"))?,
                );
                return Ok(response);
            }
        };
        let bytes = match &rendered.wav {
            Some(wav) => {
                let start = usize::try_from(start).map_err(AppError::internal)?;
                let end = start + usize::try_from(length).map_err(AppError::internal)?;
                wav.get(start..end).map(<[u8]>::to_vec).ok_or_else(|| {
                    AppError::internal(anyhow::anyhow!("range outside the rendering"))
                })?
            }
            None => state
                .store
                .speech_audio_bytes(&rendered.cache_key, start, length)
                .await?
                .ok_or_else(|| {
                    crate::infrastructure::deepgram::unavailable(
                        "The audio was evicted from the cache while it was being read",
                    )
                })?,
        };
        let mut response = (status, Body::from(bytes)).into_response();
        response
            .headers_mut()
            .insert(header::CONTENT_LENGTH, HeaderValue::from(length));
        if status == StatusCode::PARTIAL_CONTENT {
            response.headers_mut().insert(
                header::CONTENT_RANGE,
                header_value(&format!(
                    "bytes {start}-{}/{total}",
                    start + length.saturating_sub(1)
                ))?,
            );
        }
        response
    };
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("audio/wav"));
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    headers.insert(header::ETAG, header_value(&etag)?);
    // Revalidate with the ETag: an edit can change what a message's URL plays.
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-cache"),
    );
    headers.insert("x-dm-voice", header_value(voice_key)?);
    headers.insert(
        "x-dm-duration-ms",
        HeaderValue::from(rendered.meta.duration_milliseconds),
    );
    headers.insert(
        "x-dm-cache",
        HeaderValue::from_static(if rendered.hit() { "hit" } else { "miss" }),
    );
    Ok(response)
}

fn header_value(value: &str) -> AppResult<HeaderValue> {
    HeaderValue::from_str(value).map_err(AppError::internal)
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TranscribeQuery {
    language: Option<String>,
}

/// The canonical media type of an accepted upload, from its `Content-Type`.
fn accepted_audio_type(headers: &HeaderMap) -> AppResult<&'static str> {
    let raw = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let media = raw
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let canonical = match media.as_str() {
        "audio/x-wav" | "audio/wave" | "audio/vnd.wave" => "audio/wav",
        "audio/mp3" => "audio/mpeg",
        "audio/m4a" => "audio/x-m4a",
        "audio/x-flac" => "audio/flac",
        other => other,
    };
    ACCEPTED_AUDIO
        .iter()
        .copied()
        .find(|accepted| *accepted == canonical)
        .ok_or_else(|| {
            AppError::refused(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "unsupported_audio_type",
                if media.is_empty() {
                    "The request has no Content-Type; send the audio's media type.".to_owned()
                } else {
                    format!("DM does not transcribe {media}.")
                },
            )
            .with_hint(format!(
                "Send the raw audio with Content-Type one of: {}.",
                ACCEPTED_AUDIO.join(", ")
            ))
            .with_details(json!({ "accepted": ACCEPTED_AUDIO }))
        })
}

fn valid_language(language: &str) -> bool {
    let mut parts = language.split('-');
    let primary = parts.next().unwrap_or_default();
    language.len() <= 35
        && (2..=3).contains(&primary.len())
        && primary.bytes().all(|byte| byte.is_ascii_alphabetic())
        && parts.all(|part| {
            (2..=8).contains(&part.len()) && part.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
}

/// `POST /api/v1/speech/transcriptions[?language=en]` with the raw audio as the body: what was
/// said. The audio streams through to Deepgram and is never stored or logged.
pub(super) async fn transcribe(
    State(state): State<AppState>,
    Authenticated(context): Authenticated,
    ApiQuery(query): ApiQuery<TranscribeQuery>,
    headers: HeaderMap,
    body: Body,
) -> AppResult<Json<Value>> {
    let provider = state.speech.provider()?;
    let content_type = accepted_audio_type(&headers)?;
    let language = query
        .language
        .map(|language| language.trim().to_owned())
        .filter(|language| !language.is_empty());
    if let Some(language) = &language
        && !valid_language(language)
    {
        return Err(AppError::validation(format!(
            "language `{language}` is not a language tag such as en, pt-BR or zh-Hant; omit it to detect the language"
        )));
    }
    let cap = state.settings.speech.transcribe_max_bytes;
    let too_large = || {
        AppError::refused(
            StatusCode::PAYLOAD_TOO_LARGE,
            "audio_too_large",
            format!("The audio is larger than {cap} bytes, the most DM transcribes at once."),
        )
        .with_hint("Send a shorter recording, or split it and transcribe each part.")
        .with_details(json!({ "max_bytes": cap }))
    };
    match headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
    {
        Some(0) => {
            return Err(AppError::refused(
                StatusCode::UNPROCESSABLE_ENTITY,
                "audio_empty",
                "The request body is empty; send the recording's bytes as the body.",
            ));
        }
        Some(length) if length > cap => return Err(too_large()),
        _ => {}
    }
    if let Err(limit) = state
        .store
        .charge_transcription(&context.actor, state.settings.speech.transcribe_per_minute)
        .await?
    {
        return Err(limit_error(
            limit,
            u64::from(state.settings.speech.transcribe_per_minute),
        ));
    }
    let exceeded = Arc::new(AtomicBool::new(false));
    let seen = Arc::new(AtomicU64::new(0));
    let audio = {
        let exceeded = exceeded.clone();
        body.into_data_stream()
            .map(move |chunk| {
                let chunk = chunk.map_err(|_| io::Error::other("the upload was interrupted"))?;
                let length = u64::try_from(chunk.len()).unwrap_or(u64::MAX);
                let total = seen
                    .fetch_add(length, Ordering::Relaxed)
                    .saturating_add(length);
                if total > cap {
                    exceeded.store(true, Ordering::Relaxed);
                    return Err(io::Error::other(
                        "the audio exceeds DM_TRANSCRIBE_MAX_BYTES",
                    ));
                }
                Ok(chunk)
            })
            .boxed()
    };
    let transcription = provider
        .transcribe(TranscriptionRequest {
            audio,
            content_type: content_type.to_owned(),
            language,
        })
        .await;
    if exceeded.load(Ordering::Relaxed) {
        return Err(too_large());
    }
    let transcription = transcription?;
    Ok(Json(json!({
        "transcript": transcription.transcript,
        "language": transcription.language,
        "duration_milliseconds": transcription.duration_milliseconds,
        "confidence": transcription.confidence,
    })))
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue, header};

    use super::{Requested, accepted_audio_type, requested_range, valid_language};

    fn range(value: &str, total: u64) -> Requested {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::RANGE,
            HeaderValue::from_str(value).unwrap_or_else(|_| HeaderValue::from_static("")),
        );
        requested_range(&headers, "\"etag\"", total)
    }

    fn part(requested: &Requested) -> Option<(u64, u64)> {
        match requested {
            Requested::Part { start, length } => Some((*start, *length)),
            _ => None,
        }
    }

    #[test]
    fn single_byte_ranges_are_honoured_and_others_send_the_whole_file() {
        assert_eq!(part(&range("bytes=0-99", 1000)), Some((0, 100)));
        assert_eq!(part(&range("bytes=900-", 1000)), Some((900, 100)));
        assert_eq!(part(&range("bytes=-100", 1000)), Some((900, 100)));
        assert_eq!(part(&range("bytes=-5000", 1000)), Some((0, 1000)));
        assert_eq!(part(&range("bytes=990-2000", 1000)), Some((990, 10)));
        assert!(matches!(
            range("bytes=1000-", 1000),
            Requested::Unsatisfiable
        ));
        assert!(matches!(
            range("bytes=2000-3000", 1000),
            Requested::Unsatisfiable
        ));
        assert!(matches!(range("bytes=-0", 1000), Requested::Unsatisfiable));
        assert!(matches!(range("bytes=0-1,5-6", 1000), Requested::Full));
        assert!(matches!(range("items=0-1", 1000), Requested::Full));
        assert!(matches!(range("bytes=9-1", 1000), Requested::Full));
        let mut stale = HeaderMap::new();
        stale.insert(header::RANGE, HeaderValue::from_static("bytes=0-1"));
        stale.insert(header::IF_RANGE, HeaderValue::from_static("\"other\""));
        assert!(matches!(
            requested_range(&stale, "\"etag\"", 10),
            Requested::Full
        ));
    }

    #[test]
    fn audio_types_and_language_tags_are_checked() {
        let with = |value: &'static str| {
            let mut headers = HeaderMap::new();
            headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(value));
            accepted_audio_type(&headers).ok()
        };
        assert_eq!(with("audio/webm;codecs=opus"), Some("audio/webm"));
        assert_eq!(with("audio/x-wav"), Some("audio/wav"));
        assert_eq!(with("AUDIO/MPEG"), Some("audio/mpeg"));
        assert_eq!(with("video/mp4"), None);
        assert_eq!(with("application/json"), None);
        assert!(accepted_audio_type(&HeaderMap::new()).is_err());
        for good in ["en", "pt-BR", "zh-Hant", "yue"] {
            assert!(valid_language(good), "{good}");
        }
        for bad in [
            "",
            "e",
            "english-language-tag-that-is-way-too-long",
            "en_US",
            "e1",
        ] {
            assert!(!valid_language(bad), "{bad}");
        }
    }
}
