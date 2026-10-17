//! Speech: every account's voice, spoken messages, rendering through the cache, listening.

use std::sync::Arc;

use dashmap::DashMap;
use http::StatusCode;
use tokio::sync::Mutex;

use super::{
    ports::{SpeechProvider, SynthesisRequest},
    state::AppState,
};
use crate::{
    AppError, AppResult,
    domain::{
        Account, ActorRef, Message, MessageCreate, PinnedVoice, Speech, Voice, VoiceCatalog, audio,
        speech::{MAX_SCRIPT_CHARACTERS, cache_key, plain_text_script, validate_script},
    },
    infrastructure::postgres::{
        AudioMeta, ChosenVoice, LimitExceeded, SpeechCharge, SpeechRenderJob, limit_error,
    },
};

/// The speech provider (absent without a Deepgram key) and the renders in flight in this
/// process, so concurrent requests for the same audio render it once.
#[derive(Clone, Default)]
pub struct SpeechService {
    provider: Option<Arc<dyn SpeechProvider>>,
    flights: Arc<DashMap<[u8; 32], Arc<Mutex<()>>>>,
}

impl SpeechService {
    /// A service rendering through `provider` (`None`: speech is not configured).
    #[must_use]
    pub fn new(provider: Option<Arc<dyn SpeechProvider>>) -> Self {
        Self {
            provider,
            flights: Arc::default(),
        }
    }

    /// Whether speech and transcription can run.
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.provider.is_some()
    }

    /// The provider.
    ///
    /// # Errors
    /// 503 `speech_not_configured`.
    pub fn provider(&self) -> AppResult<Arc<dyn SpeechProvider>> {
        self.provider.clone().ok_or_else(not_configured)
    }

    fn flight(&self, key: [u8; 32]) -> Arc<Mutex<()>> {
        self.flights.entry(key).or_default().clone()
    }

    fn land(&self, key: &[u8; 32]) {
        // Only the map still holds the flight: nobody is waiting for this render any more.
        self.flights
            .remove_if(key, |_, flight| Arc::strong_count(flight) <= 1);
    }
}

/// 503 `speech_not_configured`.
#[must_use]
pub fn not_configured() -> AppError {
    AppError::refused(
        StatusCode::SERVICE_UNAVAILABLE,
        "speech_not_configured",
        "This DM service has no speech provider configured (DM_DEEPGRAM_API_KEY is not set), so it cannot render speech or transcribe audio.",
    )
    .with_hint("Send text instead; speech works once the operator of this DM sets DM_DEEPGRAM_API_KEY. GET /api/v1/accounts says whether speech is enabled.")
}

/// 422 `invalid_speech_script`.
#[must_use]
pub fn invalid_script(message: String) -> AppError {
    AppError::refused(
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_speech_script",
        message,
    )
    .with_hint("Scripts are plain words with cues in square brackets before the words they colour, such as [warmly] or [whispers] … [/whispers]; see `dm docs expressive`.")
}

/// 422 `voice_not_found`, listing the valid keys in `details.voices`.
#[must_use]
pub fn voice_not_found(key: &str, catalog: &VoiceCatalog) -> AppError {
    let keys = catalog.keys();
    AppError::refused(
        StatusCode::UNPROCESSABLE_ENTITY,
        "voice_not_found",
        format!(
            "There is no voice `{key}`. The voices are: {}.",
            keys.join(", ")
        ),
    )
    .with_hint("Use one of the keys in details.voices (GET /api/v1/voices describes each).")
    .with_details(serde_json::json!({ "voices": keys }))
}

/// 404 `nothing_to_speak`.
#[must_use]
pub fn nothing_to_speak() -> AppError {
    AppError::refused(
        StatusCode::NOT_FOUND,
        "nothing_to_speak",
        "This message has no speech and no text to read aloud (it holds only attachments, a GIF or a voice note).",
    )
    .with_hint("Play its voice note or open its attachments instead.")
}

/// 410 `message_deleted`.
#[must_use]
pub fn message_deleted() -> AppError {
    AppError::refused(
        StatusCode::GONE,
        "message_deleted",
        "This message was deleted; its content, speech included, is gone.",
    )
}

/// An account's voice now.
#[derive(Clone, Debug)]
pub struct AccountVoice {
    /// The voice it speaks in.
    pub voice: Voice,
    /// The stored choice behind it; `None` for the default voice (also when a stored choice
    /// names a voice that left the catalog).
    pub chosen: Option<ChosenVoice>,
}

/// The voice an account speaks in: its stored choice, else its default.
///
/// # Errors
/// Database failures.
pub async fn account_voice(state: &AppState, account_uuid: &str) -> AppResult<AccountVoice> {
    let catalog = &state.settings.speech.voices;
    if let Some(chosen) = state.store.chosen_voice(account_uuid).await?
        && let Some(voice) = catalog.get(&chosen.voice_key)
    {
        return Ok(AccountVoice {
            voice: voice.clone(),
            chosen: Some(chosen),
        });
    }
    Ok(AccountVoice {
        voice: catalog.default_for(account_uuid).clone(),
        chosen: None,
    })
}

/// Whether `caller` may choose `target`'s voice: the account itself, or a Silicon's custodian.
#[must_use]
pub fn may_change_voice(caller: &ActorRef, target: &Account) -> bool {
    target.uuid == caller.id.as_str()
        || (target.is_silicon() && target.custodian.as_deref() == Some(caller.id.as_str()))
}

/// Pins the voice of a message's speech, derives its transcript (the script without cues) and
/// returns the rendering to queue with it. `previous` is the message's speech before an
/// edit: an edit that names no voice keeps the voice the message already speaks in.
///
/// # Errors
/// `speech_not_configured`, `invalid_speech_script`, `voice_not_found`, a message with both a
/// voice note and speech, or database failures.
pub async fn prepare_message_speech(
    state: &AppState,
    sender: &ActorRef,
    content: &mut MessageCreate,
    previous: Option<&Speech>,
) -> AppResult<Option<SpeechRenderJob>> {
    let Some(speech) = content.speech.as_mut() else {
        return Ok(None);
    };
    if !state.speech.enabled() {
        return Err(not_configured());
    }
    validate_script(&speech.script).map_err(invalid_script)?;
    if content.voice.is_some() {
        return Err(AppError::validation(
            "a message carries one recording: a voice note (voice) or speech, not both",
        ));
    }
    let settings = &state.settings.speech;
    let pinned = match &speech.voice {
        Some(key) => {
            let voice = settings
                .voices
                .get(key)
                .ok_or_else(|| voice_not_found(key, &settings.voices))?;
            pin(voice, &settings.model)
        }
        None => match previous.and_then(|speech| speech.pinned.clone()) {
            Some(pinned) => pinned,
            None => pin(
                &account_voice(state, sender.id.as_str()).await?.voice,
                &settings.model,
            ),
        },
    };
    let key = cache_key(&pinned.model, &pinned.voice_id, &speech.script);
    let words = speech.spoken_words();
    // The words are the message's transcript, whatever a request said.
    content.voice_transcript = (!words.is_empty()).then_some(words);
    let job = SpeechRenderJob {
        cache_key: key,
        model: pinned.model.clone(),
        voice_id: pinned.voice_id.clone(),
        script: speech.script.clone(),
        cached: state.store.speech_audio_meta(&key).await?,
        charge: charge_for(state, &speech.script),
    };
    speech.pinned = Some(pinned);
    Ok(Some(job))
}

fn pin(voice: &Voice, model: &str) -> PinnedVoice {
    PinnedVoice {
        key: voice.key.clone(),
        name: voice.name.clone(),
        voice_id: voice.voice_id.clone(),
        model: model.to_owned(),
    }
}

fn charge_for(state: &AppState, script: &str) -> SpeechCharge {
    SpeechCharge {
        characters: u64::try_from(script.chars().count()).unwrap_or(u64::MAX),
        renders_per_minute: state.settings.speech.renders_per_minute,
        daily_characters: state.settings.speech.daily_characters,
    }
}

/// A rendering, from the cache or rendered now.
#[derive(Clone, Debug)]
pub struct Rendered {
    /// `sha256(model|voice_id|script)`.
    pub cache_key: [u8; 32],
    /// Its audio facts.
    pub meta: AudioMeta,
    /// The WAV when it was rendered by this request (else read it from the cache).
    pub wav: Option<Vec<u8>>,
}

impl Rendered {
    /// Whether the audio came from the cache.
    #[must_use]
    pub fn hit(&self) -> bool {
        self.wav.is_none()
    }
}

/// The audio of a script in a voice: from the cache, or rendered now and cached. Only a render
/// is charged to `payer` (cache hits never are); concurrent requests for the same audio in
/// this process render it once.
///
/// # Errors
/// `speech_not_configured`, a speech limit (429), `speech_rejected`, `speech_unavailable`,
/// or database failures.
pub async fn render(
    state: &AppState,
    payer: Option<&ActorRef>,
    script: &str,
    voice_id: &str,
    model: &str,
) -> AppResult<Rendered> {
    let key = cache_key(model, voice_id, script);
    if let Some(meta) = state.store.speech_audio_meta(&key).await? {
        return Ok(Rendered {
            cache_key: key,
            meta,
            wav: None,
        });
    }
    let provider = state.speech.provider()?;
    let flight = state.speech.flight(key);
    let guard = flight.lock().await;
    let result = render_once(
        state,
        provider.as_ref(),
        payer,
        key,
        script,
        voice_id,
        model,
    )
    .await;
    drop(guard);
    drop(flight);
    state.speech.land(&key);
    result
}

async fn render_once(
    state: &AppState,
    provider: &dyn SpeechProvider,
    payer: Option<&ActorRef>,
    key: [u8; 32],
    script: &str,
    voice_id: &str,
    model: &str,
) -> AppResult<Rendered> {
    // Someone may have rendered it while this request waited for the flight.
    if let Some(meta) = state.store.speech_audio_meta(&key).await? {
        return Ok(Rendered {
            cache_key: key,
            meta,
            wav: None,
        });
    }
    if let Some(payer) = payer {
        let charge = charge_for(state, script);
        if let Err(limit) = state.store.charge_speech(payer, charge).await? {
            let allowed = match limit {
                LimitExceeded::RendersPerMinute => u64::from(charge.renders_per_minute),
                LimitExceeded::DailyCharacters | LimitExceeded::TranscriptionsPerMinute => {
                    charge.daily_characters
                }
            };
            return Err(limit_error(limit, allowed));
        }
    }
    let speech = provider
        .synthesize(SynthesisRequest {
            script,
            voice_id,
            model,
        })
        .await?;
    let wav = audio::wav_from_pcm(&speech.pcm);
    let duration = audio::duration_milliseconds(speech.pcm.len());
    let peaks = audio::peaks(&speech.pcm);
    let meta = state
        .store
        .store_speech_audio(&key, &wav, duration, &peaks)
        .await?;
    // When another process cached the same key first, serve its bytes (one audio per key).
    let ours = blake3::hash(&wav).to_hex()[..32].to_owned();
    Ok(Rendered {
        cache_key: key,
        wav: (meta.etag == ours).then_some(wav),
        meta,
    })
}

/// Longest a listener waits for a spoken message's pre-render before rendering it itself.
const PRE_RENDER_WAIT: std::time::Duration = std::time::Duration::from_secs(20);
const PRE_RENDER_POLL: std::time::Duration = std::time::Duration::from_millis(250);

/// The audio of a message for `listener`: a spoken message's own pre-render when it is ready
/// or under way (a listener who opens a message the moment it arrives waits a few seconds
/// instead of rendering it a second time and being charged for it), else a render now.
///
/// # Errors
/// As [`render`] and [`message_speech_source`].
pub async fn message_audio(
    state: &AppState,
    listener: &ActorRef,
    message: &Message,
) -> AppResult<(SpeechSource, Rendered)> {
    let source = message_speech_source(state, message).await?;
    let (script, voice_id, model) = match &source {
        SpeechSource::Speech { script, pinned } => (script, &pinned.voice_id, &pinned.model),
        SpeechSource::Text { script, voice } => {
            (script, &voice.voice_id, &state.settings.speech.model)
        }
    };
    if matches!(source, SpeechSource::Speech { .. }) {
        state.speech.provider()?;
        let key = cache_key(model, voice_id, script);
        let deadline = tokio::time::Instant::now() + PRE_RENDER_WAIT;
        loop {
            if let Some(meta) = state.store.speech_audio_meta(&key).await? {
                return Ok((
                    source,
                    Rendered {
                        cache_key: key,
                        meta,
                        wav: None,
                    },
                ));
            }
            if tokio::time::Instant::now() >= deadline
                || !state
                    .store
                    .speech_render_under_way(message.id, &key)
                    .await?
            {
                break;
            }
            tokio::time::sleep(PRE_RENDER_POLL).await;
        }
    }
    let rendered = render(state, Some(listener), script, voice_id, model).await?;
    Ok((source, rendered))
}

/// What listening to a message plays.
#[derive(Clone, Debug)]
pub enum SpeechSource {
    /// A spoken message: its script in the voice pinned on it.
    Speech {
        /// The script.
        script: String,
        /// The pinned voice.
        pinned: PinnedVoice,
    },
    /// A text message read in its sender's current voice.
    Text {
        /// The text as a script (brackets made literal, links read as "link").
        script: String,
        /// The sender's voice now.
        voice: Voice,
    },
}

impl SpeechSource {
    /// The catalog key of the voice.
    #[must_use]
    pub fn voice_key(&self) -> &str {
        match self {
            Self::Speech { pinned, .. } => &pinned.key,
            Self::Text { voice, .. } => &voice.key,
        }
    }
}

/// What listening to `message` plays.
///
/// # Errors
/// `message_deleted` (410), `nothing_to_speak` (404), `speech_too_long` (422) for text over
/// 5,000 characters, or database failures.
pub async fn message_speech_source(state: &AppState, message: &Message) -> AppResult<SpeechSource> {
    if message.deleted_at.is_some() {
        return Err(message_deleted());
    }
    if let Some(speech) = &message.speech
        && let Some(pinned) = &speech.pinned
    {
        return Ok(SpeechSource::Speech {
            script: speech.script.clone(),
            pinned: pinned.clone(),
        });
    }
    let script = plain_text_script(message.text.as_deref().unwrap_or_default());
    if script.is_empty() {
        return Err(nothing_to_speak());
    }
    let characters = script.chars().count();
    if characters > MAX_SCRIPT_CHARACTERS {
        return Err(AppError::refused(
            StatusCode::UNPROCESSABLE_ENTITY,
            "speech_too_long",
            format!(
                "This message has {characters} characters to read; DM reads at most {MAX_SCRIPT_CHARACTERS} aloud."
            ),
        )
        .with_hint("Read it instead; listening works for messages up to 5,000 characters."));
    }
    Ok(SpeechSource::Text {
        script,
        voice: account_voice(state, message.sender.id.as_str())
            .await?
            .voice,
    })
}
