//! Speech: every account's voice, spoken messages, rendering through the cache, listening.

use std::{sync::Arc, time::Duration};

use dashmap::DashMap;
use http::StatusCode;
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};

use super::{
    ports::{SpeechProvider, SynthesisRequest},
    state::AppState,
};
use crate::{
    AppError, AppResult,
    domain::{
        Account, ActorRef, Message, MessageCreate, PinnedVoice, Speech, Voice, VoiceCatalog, audio,
        speech::{
            MAX_SCRIPT_CHARACTERS, SpeechScope, cache_key, plain_text_script, validate_script,
        },
    },
    infrastructure::postgres::{
        AudioMeta, ChosenVoice, LimitExceeded, SpeechCharge, SpeechLease, SpeechRenderJob,
        limit_error,
    },
};

/// Characters of script being rendered at once in one process. A render holds its audio in
/// memory several times over while it is received and stored (up to about 22 KB for each
/// character of script, measured: 109 MB for a 4,983-character script), so this keeps
/// rendering under about 170 MB however many long scripts arrive together. One script of the
/// longest kind always fits, beside a few short ones.
const RENDER_CHARACTERS_AT_ONCE: usize = 7_500;
/// Longest a request waits for room to render before it is told to retry.
const RENDER_QUEUE_WAIT: Duration = Duration::from_secs(30);
/// Audio responses streaming at once in one process (each holds one window of the WAV).
const STREAMS_AT_ONCE: usize = 64;
/// Seconds a client is asked to wait when rendering or listening is full.
const BUSY_RETRY_SECONDS: u64 = 5;

/// The speech provider (absent without a Deepgram key), the renders in flight in this process
/// (concurrent requests for the same audio render it once), and the two budgets that keep the
/// process's memory flat whatever arrives: characters being rendered and audio being streamed.
#[derive(Clone)]
pub struct SpeechService {
    provider: Option<Arc<dyn SpeechProvider>>,
    flights: Arc<DashMap<[u8; 32], Arc<Mutex<()>>>>,
    rendering: Arc<Semaphore>,
    streaming: Arc<Semaphore>,
}

impl Default for SpeechService {
    fn default() -> Self {
        Self::new(None)
    }
}

impl SpeechService {
    /// A service rendering through `provider` (`None`: speech is not configured).
    #[must_use]
    pub fn new(provider: Option<Arc<dyn SpeechProvider>>) -> Self {
        Self {
            provider,
            flights: Arc::default(),
            rendering: Arc::new(Semaphore::new(RENDER_CHARACTERS_AT_ONCE)),
            streaming: Arc::new(Semaphore::new(STREAMS_AT_ONCE)),
        }
    }

    /// Room to render `script`: waits for it (`wait` `None`: as long as it takes, for the
    /// worker), else 503 `speech_unavailable` after `wait`.
    async fn render_room(
        &self,
        script: &str,
        wait: Option<Duration>,
    ) -> AppResult<OwnedSemaphorePermit> {
        let characters = script.chars().count().clamp(1, RENDER_CHARACTERS_AT_ONCE);
        let permits = u32::try_from(characters).unwrap_or(u32::MAX);
        let acquire = self.rendering.clone().acquire_many_owned(permits);
        let permit = match wait {
            Some(wait) => tokio::time::timeout(wait, acquire)
                .await
                .map_err(|_| busy("DM is rendering a lot of speech right now"))?,
            None => acquire.await,
        };
        permit.map_err(|_| busy("DM is shutting down"))
    }

    /// Room to stream one audio response, or 503 `speech_unavailable` when this process is
    /// already streaming as many as it allows.
    ///
    /// # Errors
    /// 503 `speech_unavailable` with `Retry-After`.
    pub fn stream_room(&self) -> AppResult<OwnedSemaphorePermit> {
        self.streaming
            .clone()
            .try_acquire_owned()
            .map_err(|_| busy("Many people are listening to DM right now"))
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

/// 503 `speech_unavailable` for a process that has no room for more speech right now.
fn busy(reason: &str) -> AppError {
    AppError::refused(
        StatusCode::SERVICE_UNAVAILABLE,
        "speech_unavailable",
        format!("{reason}, so it cannot start this one. Nothing was lost or charged."),
    )
    .with_retry_after(BUSY_RETRY_SECONDS)
    .with_hint("Retry the same request after the Retry-After delay.")
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
/// returns the rendering to queue with it (its audio is kept for `conversation`). `previous`
/// is the message's speech before an edit: an edit that names no voice keeps the voice the
/// message already speaks in.
///
/// # Errors
/// `speech_not_configured`, `invalid_speech_script`, `voice_not_found`, a message with both a
/// voice note and speech, or database failures.
pub async fn prepare_message_speech(
    state: &AppState,
    sender: &ActorRef,
    conversation: uuid::Uuid,
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
    let key = cache_key(
        SpeechScope::Conversation(conversation),
        &pinned.model,
        &pinned.voice_id,
        &speech.script,
    );
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

/// A rendering, from the cache or rendered now; its audio is read from the cache.
#[derive(Clone, Debug)]
pub struct Rendered {
    /// The cache key (`domain::speech::cache_key`).
    pub cache_key: [u8; 32],
    /// Its audio facts.
    pub meta: AudioMeta,
    /// Rendered by this request (else found in the cache).
    pub fresh: bool,
}

impl Rendered {
    /// Whether the audio came from the cache.
    #[must_use]
    pub fn hit(&self) -> bool {
        !self.fresh
    }
}

/// The audio under `key` (`script` in `voice_id`): from the cache, or rendered now and cached.
/// Only a render is charged to `payer` (cache hits never are), and a render the provider
/// fails gives the day's characters back. Concurrent requests for the same audio in this
/// process render it once. `wait` bounds how long a request waits for room to render (`None`:
/// the worker, which waits its turn).
///
/// # Errors
/// `speech_not_configured`, a speech limit (429), `speech_rejected`, `speech_unavailable`,
/// or database failures.
pub async fn render(
    state: &AppState,
    payer: Option<&ActorRef>,
    key: [u8; 32],
    script: &str,
    voice_id: &str,
    model: &str,
    wait: Option<Duration>,
) -> AppResult<Rendered> {
    if let Some(meta) = state.store.speech_audio_meta(&key).await? {
        return Ok(Rendered {
            cache_key: key,
            meta,
            fresh: false,
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
        wait,
    )
    .await;
    drop(guard);
    drop(flight);
    state.speech.land(&key);
    result
}

#[allow(
    clippy::too_many_arguments,
    reason = "one render: who pays, what to say, in which voice, and how long to queue"
)]
async fn render_once(
    state: &AppState,
    provider: &dyn SpeechProvider,
    payer: Option<&ActorRef>,
    key: [u8; 32],
    script: &str,
    voice_id: &str,
    model: &str,
    wait: Option<Duration>,
) -> AppResult<Rendered> {
    // Someone may have rendered it while this request waited for the flight.
    if let Some(meta) = state.store.speech_audio_meta(&key).await? {
        return Ok(Rendered {
            cache_key: key,
            meta,
            fresh: false,
        });
    }
    let charged = match payer {
        Some(payer) => {
            let charge = charge_for(state, script);
            match state.store.charge_speech(payer, charge).await? {
                Ok(charged) => Some((payer, charged)),
                Err(limit) => {
                    let allowed = match limit {
                        LimitExceeded::RendersPerMinute => u64::from(charge.renders_per_minute),
                        LimitExceeded::DailyCharacters | LimitExceeded::TranscriptionsPerMinute => {
                            charge.daily_characters
                        }
                    };
                    return Err(limit_error(limit, allowed));
                }
            }
        }
        None => None,
    };
    let rendered = async {
        let room = state.speech.render_room(script, wait).await?;
        let speech = provider
            .synthesize(SynthesisRequest {
                script,
                voice_id,
                model,
            })
            .await?;
        let duration = audio::duration_milliseconds(speech.pcm.len());
        let peaks = audio::peaks(&speech.pcm);
        let wav = audio::wav_from_pcm_buffer(speech.pcm);
        let meta = state
            .store
            .store_speech_audio(&key, &wav, duration, &peaks)
            .await?;
        drop(wav);
        drop(room);
        Ok::<_, AppError>(meta)
    }
    .await;
    match rendered {
        Ok(meta) => Ok(Rendered {
            cache_key: key,
            meta,
            fresh: true,
        }),
        Err(error) => {
            // Nothing was rendered: the characters are the account's again (the render still
            // counts towards the per-minute limit, which paces retries).
            if let Some((payer, charged)) = &charged
                && let Err(refund) = state.store.refund_speech_characters(payer, charged).await
            {
                tracing::warn!(
                    code = refund.code(),
                    "giving back a failed render's characters failed"
                );
            }
            Err(error)
        }
    }
}

/// Longest a listener waits for a spoken message's render under way elsewhere (a worker in
/// any process, or another listener) before it is told to retry; renders take seconds, the
/// longest scripts a minute or two, and the route is cut at 200 seconds.
const PRE_RENDER_WAIT: Duration = Duration::from_secs(150);
const PRE_RENDER_POLL: Duration = Duration::from_millis(500);
/// How long a listener's own render of a spoken message holds the rendering's lease.
const LISTENER_LEASE: Duration = Duration::from_secs(240);
/// How long a listener leaves a pre-render that is due now to the worker about to take it
/// (workers poll every two seconds, and are woken at once) before rendering it itself.
const WORKER_GRACE: Duration = Duration::from_secs(5);

/// The audio of a message for `listener`. A spoken message plays its own rendering: from the
/// cache, or (while a worker or another listener renders it, in any process) as soon as that
/// render lands, or rendered now by this request under the rendering's lease, so it is never
/// rendered twice at once. The sender paid for it when sending, so the listener is never
/// charged for it. A text message is read in its sender's voice, rendered for and charged to
/// the listener.
///
/// # Errors
/// As [`render`] and [`message_speech_source`]; `speech_unavailable` when another render of
/// the same speech is still running after a long wait.
pub async fn message_audio(
    state: &AppState,
    listener: &ActorRef,
    message: &Message,
) -> AppResult<(SpeechSource, Rendered)> {
    let source = message_speech_source(state, message).await?;
    let scope = SpeechScope::Conversation(message.conversation_id);
    let rendered = match &source {
        SpeechSource::Speech { script, pinned } => {
            state.speech.provider()?;
            let key = cache_key(scope, &pinned.model, &pinned.voice_id, script);
            spoken_audio(state, message.id, key, script, pinned).await?
        }
        SpeechSource::Text { script, voice } => {
            let model = &state.settings.speech.model;
            let key = cache_key(scope, model, &voice.voice_id, script);
            render(
                state,
                Some(listener),
                key,
                script,
                &voice.voice_id,
                model,
                Some(RENDER_QUEUE_WAIT),
            )
            .await?
        }
    };
    Ok((source, rendered))
}

async fn spoken_audio(
    state: &AppState,
    message_id: uuid::Uuid,
    key: [u8; 32],
    script: &str,
    pinned: &PinnedVoice,
) -> AppResult<Rendered> {
    let started = tokio::time::Instant::now();
    let deadline = started + PRE_RENDER_WAIT;
    loop {
        if let Some(meta) = state.store.speech_audio_meta(&key).await? {
            return Ok(Rendered {
                cache_key: key,
                meta,
                fresh: false,
            });
        }
        match state
            .store
            .lease_speech_render(
                message_id,
                &key,
                LISTENER_LEASE,
                started.elapsed() < WORKER_GRACE,
            )
            .await?
        {
            SpeechLease::Busy => {}
            lease => {
                let rendered = render(
                    state,
                    None,
                    key,
                    script,
                    &pinned.voice_id,
                    &pinned.model,
                    Some(RENDER_QUEUE_WAIT),
                )
                .await;
                if let SpeechLease::Taken(lease_id) = lease {
                    let store = &state.store;
                    let finished = match &rendered {
                        Ok(rendered) => {
                            store
                                .complete_speech_render(message_id, &key, &rendered.meta)
                                .await
                        }
                        Err(_) => Ok(()),
                    };
                    let released = store.release_speech_lease(message_id, &key, lease_id).await;
                    for outcome in [finished, released] {
                        // The audio is cached either way; the next listener or the worker
                        // sets the record straight.
                        if let Err(error) = outcome {
                            tracing::warn!(
                                code = error.code(),
                                "recording a listener's render failed"
                            );
                        }
                    }
                }
                return rendered;
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(crate::infrastructure::deepgram::unavailable(
                "This spoken message is still being rendered",
            ));
        }
        tokio::time::sleep(PRE_RENDER_POLL).await;
    }
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
