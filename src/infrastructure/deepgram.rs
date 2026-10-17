//! Deepgram: speech through the Voice Agent (Deepgram's managed `ElevenLabs`) and
//! transcription through `/v1/listen`.
//!
//! Every call is made by the DM service with the key from `DM_DEEPGRAM_API_KEY` (a
//! regular-scope key cannot mint short-lived grants for clients). Both directions opt out of
//! Deepgram's model improvement program (`mip_opt_out`). Scripts, audio and transcripts are
//! never logged.

use std::time::Duration;

use async_trait::async_trait;
use futures::{SinkExt as _, StreamExt as _};
use http::StatusCode;
use reqwest::redirect::Policy;
use secrecy::{ExposeSecret as _, SecretString};
use serde_json::{Value, json};
use tokio::time::{Instant, interval_at, timeout, timeout_at};
use tokio_tungstenite::{
    connect_async_with_config,
    tungstenite::{
        Error as SocketError, Message, client::IntoClientRequest as _, protocol::WebSocketConfig,
    },
};
use url::Url;

use crate::{
    AppError, AppResult,
    application::ports::{
        SpeechProvider, SynthesisRequest, SynthesizedSpeech, Transcription, TranscriptionRequest,
    },
    config::SpeechSettings,
};

/// Opening the Voice Agent connection.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// From connecting to the first audio (measured: 2.5 to 3 s from India, less in us-east-1).
const FIRST_AUDIO_TIMEOUT: Duration = Duration::from_secs(20);
/// Longest silence between two audio frames once audio started.
const IDLE_TIMEOUT: Duration = Duration::from_secs(20);
/// One whole rendering.
const TOTAL_TIMEOUT: Duration = Duration::from_secs(180);
/// The Voice Agent closes idle connections; Deepgram asks for a `KeepAlive` every few seconds.
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(4);
/// Largest rendering DM accepts (about 10 minutes of audio).
const AUDIO_MAX_BYTES: usize = 30 * 1024 * 1024;
/// Largest WebSocket frame or message.
const MAX_FRAME_BYTES: usize = 1024 * 1024;
/// Scripts longer than this are sent as several queued messages, split at sentence ends.
const CHUNK_THRESHOLD: usize = 300;
/// Smallest piece a long script is split into.
const MIN_CHUNK: usize = 90;
/// One transcription request, upload included.
const TRANSCRIBE_TIMEOUT: Duration = Duration::from_secs(600);
/// Largest transcription answer DM reads.
const MAX_LISTEN_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
/// When to retry after Deepgram was unavailable.
const RETRY_AFTER_SECONDS: u64 = 5;

/// The Deepgram adapter.
pub struct DeepgramClient {
    http: reqwest::Client,
    api_key: SecretString,
    listen_url: Url,
    agent_url: Url,
    stt_model: String,
}

impl DeepgramClient {
    /// The adapter, or `None` when no Deepgram key is configured.
    ///
    /// # Errors
    /// An internal error when the HTTP client cannot be built.
    pub fn new(settings: &SpeechSettings) -> AppResult<Option<Self>> {
        let Some(api_key) = settings.deepgram_api_key.clone() else {
            return Ok(None);
        };
        let http = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(TRANSCRIBE_TIMEOUT)
            .redirect(Policy::none())
            .referer(false)
            .user_agent(concat!("silicon-dm/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| {
                AppError::internal(anyhow::anyhow!("failed to build the Deepgram client"))
            })?;
        let listen_url = settings
            .deepgram_url
            .join("v1/listen")
            .map_err(|_| AppError::internal(anyhow::anyhow!("invalid DM_DEEPGRAM_URL")))?;
        Ok(Some(Self {
            http,
            api_key,
            listen_url,
            agent_url: settings.agent_url.clone(),
            stt_model: settings.stt_model.clone(),
        }))
    }
}

/// 503 `speech_unavailable`: Deepgram could not render now; retry shortly.
pub(crate) fn unavailable(message: &str) -> AppError {
    AppError::refused(
        StatusCode::SERVICE_UNAVAILABLE,
        "speech_unavailable",
        format!("{message}. Nothing was lost; retry in a few seconds."),
    )
    .with_retry_after(RETRY_AFTER_SECONDS)
    .with_hint("Retry the same request after the Retry-After delay.")
}

/// 422 `speech_rejected`: the provider refused this voice or script; retrying will not help.
fn rejected(message: &str) -> AppError {
    AppError::refused(
        StatusCode::UNPROCESSABLE_ENTITY,
        "speech_rejected",
        format!("{message}; retrying the same script and voice will not help."),
    )
    .with_hint("Simplify the script (plain words, cues such as [warmly] in square brackets) or choose another voice: dm voices list, or Settings → Voice on the DM website.")
}

fn connection_error(error: &SocketError) -> AppError {
    if let SocketError::Http(response) = error {
        let status = response.status();
        if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
            tracing::warn!(
                dependency = "deepgram",
                status = status.as_u16(),
                "Deepgram refused DM's key for the Voice Agent; check DM_DEEPGRAM_API_KEY"
            );
        } else {
            tracing::warn!(
                dependency = "deepgram",
                status = status.as_u16(),
                "Deepgram refused the Voice Agent connection"
            );
        }
        return unavailable("Deepgram refused the speech connection");
    }
    tracing::warn!(
        dependency = "deepgram",
        "the Voice Agent connection could not be opened"
    );
    unavailable("Deepgram's speech connection could not be opened")
}

/// Keeps short performances together; splits long ones at sentence ends (never inside a
/// cue) so playback can start sooner and each piece stays within what the model renders well.
#[must_use]
pub fn script_chunks(script: &str) -> Vec<String> {
    let script = script.trim();
    if script.chars().count() <= CHUNK_THRESHOLD {
        return vec![script.to_owned()];
    }
    let mut parts = Vec::new();
    let mut start = 0;
    let mut count = 0;
    let mut in_cue = false;
    let end_of_text = script.len();
    let mut characters = script.char_indices().peekable();
    while let Some((index, character)) = characters.next() {
        count += 1;
        match character {
            '[' => in_cue = true,
            ']' => in_cue = false,
            _ => {}
        }
        let boundary = matches!(character, '\n' | '。' | '！' | '？')
            || (matches!(character, '.' | '!' | '?')
                && characters
                    .peek()
                    .is_none_or(|(_, next)| next.is_whitespace()));
        let end = index + character.len_utf8();
        if count >= MIN_CHUNK && !in_cue && boundary && end < end_of_text {
            parts.push(script[start..end].trim().to_owned());
            start = end;
            count = 0;
        }
    }
    parts.push(script[start..].trim().to_owned());
    parts.retain(|part| !part.is_empty());
    parts
}

fn settings_message(request: &SynthesisRequest<'_>) -> String {
    json!({
        "type": "Settings",
        "mip_opt_out": true,
        "flags": {"history": false},
        "audio": {
            "input": {"encoding": "linear16", "sample_rate": 24_000},
            // Only linear16 without a container works for ElevenLabs through Deepgram; DM
            // wraps the PCM in a WAV header itself.
            "output": {"encoding": "linear16", "sample_rate": 24_000, "container": "none"},
        },
        "agent": {"speak": {"provider": {
            "type": "eleven_labs",
            "model_id": request.model,
            "voice_id": request.voice_id,
        }}},
    })
    .to_string()
}

#[async_trait]
impl SpeechProvider for DeepgramClient {
    #[allow(
        clippy::too_many_lines,
        reason = "one Voice Agent conversation: connect, welcome, settings, injections and audio"
    )]
    async fn synthesize(&self, request: SynthesisRequest<'_>) -> AppResult<SynthesizedSpeech> {
        let started = Instant::now();
        let deadline = started + TOTAL_TIMEOUT;
        let mut upgrade = self
            .agent_url
            .as_str()
            .into_client_request()
            .map_err(|_| unavailable("the speech endpoint is misconfigured"))?;
        let mut authorization =
            http::HeaderValue::from_str(&format!("Token {}", self.api_key.expose_secret()))
                .map_err(|_| unavailable("the speech credential is invalid"))?;
        authorization.set_sensitive(true);
        upgrade
            .headers_mut()
            .insert(http::header::AUTHORIZATION, authorization);
        let config = WebSocketConfig::default()
            .max_message_size(Some(MAX_FRAME_BYTES))
            .max_frame_size(Some(MAX_FRAME_BYTES));
        let (mut socket, _) = timeout(
            CONNECT_TIMEOUT,
            connect_async_with_config(upgrade, Some(config), false),
        )
        .await
        .map_err(|_| unavailable("Deepgram's speech connection timed out"))?
        .map_err(|error| connection_error(&error))?;

        let first_audio_deadline = started + FIRST_AUDIO_TIMEOUT;
        timeout_at(first_audio_deadline, async {
            loop {
                match socket.next().await {
                    Some(Ok(Message::Text(text))) => {
                        let event: Value = serde_json::from_str(&text)
                            .map_err(|_| unavailable("Deepgram sent an invalid welcome"))?;
                        return if event["type"] == "Welcome" {
                            Ok(())
                        } else {
                            Err(unavailable(
                                "Deepgram did not welcome the speech connection",
                            ))
                        };
                    }
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                    _ => {
                        return Err(unavailable(
                            "the speech connection closed before Deepgram's welcome",
                        ));
                    }
                }
            }
        })
        .await
        .map_err(|_| unavailable("Deepgram's speech welcome timed out"))??;
        timeout_at(
            first_audio_deadline,
            socket.send(Message::Text(settings_message(&request).into())),
        )
        .await
        .map_err(|_| unavailable("sending the speech settings timed out"))?
        .map_err(|_| unavailable("the speech connection failed"))?;

        let chunks = script_chunks(request.script);
        let expected = chunks.len();
        let mut pending = Some(chunks);
        let mut spoken = 0_usize;
        let mut pcm: Vec<u8> = Vec::new();
        let mut first_audio_after = None;
        let mut idle_deadline = first_audio_deadline;
        let mut keepalive = interval_at(started + KEEPALIVE_INTERVAL, KEEPALIVE_INTERVAL);
        let outcome = loop {
            let wait_until = deadline.min(idle_deadline);
            let incoming = tokio::select! {
                () = tokio::time::sleep_until(wait_until) => {
                    break Err(unavailable(if first_audio_after.is_none() {
                        "Deepgram sent no speech audio in time"
                    } else {
                        "Deepgram's speech stream stalled before it finished"
                    }));
                }
                _ = keepalive.tick() => {
                    let sent = timeout_at(
                        wait_until,
                        socket.send(Message::Text(json!({"type": "KeepAlive"}).to_string().into())),
                    )
                    .await;
                    if !matches!(sent, Ok(Ok(()))) {
                        break Err(unavailable("the speech connection failed"));
                    }
                    continue;
                }
                incoming = socket.next() => incoming,
            };
            match incoming {
                Some(Ok(Message::Binary(data))) if !data.is_empty() => {
                    if pending.is_some() {
                        break Err(unavailable(
                            "Deepgram sent audio before applying the settings",
                        ));
                    }
                    if pcm.len().saturating_add(data.len()) > AUDIO_MAX_BYTES {
                        break Err(rejected(
                            "The audio would be larger than DM keeps for one message (30 MB, about ten minutes of speech): split the script into shorter messages",
                        ));
                    }
                    first_audio_after.get_or_insert_with(|| started.elapsed());
                    pcm.extend_from_slice(&data);
                    idle_deadline = Instant::now() + IDLE_TIMEOUT;
                }
                Some(Ok(Message::Text(text))) => {
                    let Ok(event) = serde_json::from_str::<Value>(&text) else {
                        break Err(unavailable("Deepgram sent an invalid speech event"));
                    };
                    match event["type"].as_str() {
                        Some("SettingsApplied") => {
                            let Some(chunks) = pending.take() else {
                                break Err(unavailable("Deepgram applied the settings twice"));
                            };
                            let mut failed = false;
                            for chunk in chunks {
                                let injection = json!({
                                    "type": "InjectAgentMessage",
                                    "message": chunk,
                                    "behavior": "queue",
                                });
                                let sent = timeout_at(
                                    wait_until,
                                    socket.send(Message::Text(injection.to_string().into())),
                                )
                                .await;
                                if !matches!(sent, Ok(Ok(()))) {
                                    failed = true;
                                    break;
                                }
                            }
                            if failed {
                                break Err(unavailable("the speech connection failed"));
                            }
                        }
                        Some("ConversationText") if event["role"] == "assistant" => spoken += 1,
                        // Each queued piece ends with its own AgentAudioDone; a rendering of
                        // several pieces is complete once every piece was spoken.
                        Some("AgentAudioDone")
                            if !pcm.is_empty() && expected > 1 && spoken < expected => {}
                        Some("AgentAudioDone") if !pcm.is_empty() => break Ok(()),
                        Some("AgentAudioDone") => {
                            break Err(unavailable("Deepgram finished without speech audio"));
                        }
                        Some("InjectionRefused") => {
                            break Err(rejected(
                                "The voice could not say this script (Deepgram refused it)",
                            ));
                        }
                        Some("Error") => {
                            break Err(match event["code"].as_str() {
                                Some("INVALID_SETTINGS" | "FAILED_TO_SPEAK") => rejected(
                                    "The voice could not say this script (Deepgram refused the voice or the script)",
                                ),
                                _ => unavailable("Deepgram failed to complete the speech"),
                            });
                        }
                        // Warnings (slow generation and the like) are not failures. Provider
                        // descriptions are never logged: they can quote the script.
                        _ => {}
                    }
                }
                Some(Ok(Message::Close(_))) | None => {
                    break Err(unavailable(
                        "the speech connection closed before the audio finished",
                    ));
                }
                Some(Err(_)) => {
                    break Err(unavailable(
                        "the speech connection failed before the audio finished",
                    ));
                }
                Some(Ok(_)) => {}
            }
        };
        // Best effort: Deepgram ends the session when the socket closes either way.
        let _ = timeout(Duration::from_secs(1), socket.close(None)).await;
        outcome?;
        Ok(SynthesizedSpeech {
            pcm,
            first_audio_after: first_audio_after.unwrap_or_default(),
        })
    }

    async fn transcribe(&self, request: TranscriptionRequest) -> AppResult<Transcription> {
        let mut url = self.listen_url.clone();
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("model", &self.stt_model);
            query.append_pair("smart_format", "true");
            query.append_pair("punctuate", "true");
            query.append_pair("mip_opt_out", "true");
            match &request.language {
                Some(language) => query.append_pair("language", language),
                None => query.append_pair("detect_language", "true"),
            };
        }
        let response = self
            .http
            .post(url)
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Token {}", self.api_key.expose_secret()),
            )
            .header(reqwest::header::CONTENT_TYPE, &request.content_type)
            .body(reqwest::Body::wrap_stream(request.audio))
            .send()
            .await
            .map_err(|error| {
                tracing::warn!(
                    dependency = "deepgram",
                    timeout = error.is_timeout(),
                    "the transcription request failed"
                );
                transcription_unavailable()
            })?;
        let status = response.status();
        let body = read_bounded(response).await?;
        match status {
            StatusCode::OK => parse_transcription(&body, request.language),
            StatusCode::BAD_REQUEST | StatusCode::UNSUPPORTED_MEDIA_TYPE => {
                let reason = serde_json::from_slice::<Value>(&body)
                    .ok()
                    .and_then(|value| value["err_msg"].as_str().map(str::to_owned))
                    .map_or_else(
                        || "the audio is corrupt or in an unsupported format".to_owned(),
                        |reason| reason.chars().take(200).collect::<String>(),
                    );
                Err(AppError::refused(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "audio_unreadable",
                    format!("Deepgram could not read the audio ({reason})."),
                )
                .with_hint("Send a complete recording in one of the accepted formats (webm, ogg, wav, mpeg, mp4, m4a, aac, flac) with a matching Content-Type."))
            }
            StatusCode::PAYLOAD_TOO_LARGE => Err(AppError::refused(
                StatusCode::PAYLOAD_TOO_LARGE,
                "audio_too_large",
                "Deepgram refused the audio as too large.",
            )),
            status => {
                tracing::warn!(
                    dependency = "deepgram",
                    status = status.as_u16(),
                    "Deepgram refused a transcription"
                );
                Err(transcription_unavailable())
            }
        }
    }
}

fn transcription_unavailable() -> AppError {
    AppError::refused(
        StatusCode::SERVICE_UNAVAILABLE,
        "speech_unavailable",
        "Deepgram could not transcribe the audio right now. Nothing was stored; retry in a few seconds.",
    )
    .with_retry_after(RETRY_AFTER_SECONDS)
    .with_hint("Retry the same request after the Retry-After delay.")
}

async fn read_bounded(response: reqwest::Response) -> AppResult<Vec<u8>> {
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| transcription_unavailable())?;
        if body.len().saturating_add(chunk.len()) > MAX_LISTEN_RESPONSE_BYTES {
            return Err(transcription_unavailable());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn parse_transcription(body: &[u8], hint: Option<String>) -> AppResult<Transcription> {
    let value: Value = serde_json::from_slice(body).map_err(|_| {
        tracing::warn!(
            dependency = "deepgram",
            "Deepgram returned an unreadable transcription"
        );
        transcription_unavailable()
    })?;
    let channel = &value["results"]["channels"][0];
    let alternative = &channel["alternatives"][0];
    let transcript = alternative["transcript"]
        .as_str()
        .unwrap_or_default()
        .trim()
        .to_owned();
    let language = hint.or_else(|| channel["detected_language"].as_str().map(str::to_owned));
    let duration_milliseconds = value["metadata"]["duration"]
        .as_f64()
        .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
        .map(|seconds| {
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a finite, non-negative duration in seconds"
            )]
            let milliseconds = (seconds * 1_000.0).round() as u64;
            milliseconds
        });
    let confidence = alternative["confidence"]
        .as_f64()
        .filter(|confidence| (0.0..=1.0).contains(confidence));
    Ok(Transcription {
        transcript,
        language,
        duration_milliseconds,
        confidence,
    })
}

#[cfg(test)]
mod tests {
    use super::{parse_transcription, script_chunks, settings_message};
    use crate::application::ports::SynthesisRequest;

    #[test]
    fn short_scripts_stay_whole_and_long_ones_split_at_sentence_ends() {
        assert_eq!(
            script_chunks("[excited] It works!"),
            vec!["[excited] It works!"]
        );
        let sentence = "This sentence is long enough to be a piece of its own, with words. ";
        let long = format!("[warmly] {}", sentence.repeat(8));
        let chunks = script_chunks(&long);
        assert!(chunks.len() > 1, "{chunks:?}");
        assert!(chunks.iter().all(|chunk| !chunk.is_empty()));
        assert_eq!(
            chunks.join(" ").split_whitespace().collect::<Vec<_>>(),
            long.split_whitespace().collect::<Vec<_>>(),
            "nothing is lost or reordered"
        );
        // A sentence end inside a cue never splits it.
        let cue = format!(
            "{}[sighs. then laughs. a lot.] And done.",
            "Word ".repeat(70)
        );
        let chunks = script_chunks(&cue);
        assert!(
            chunks
                .iter()
                .all(|chunk| chunk.matches('[').count() == chunk.matches(']').count()),
            "{chunks:?}"
        );
    }

    #[test]
    fn settings_ask_for_raw_pcm_from_eleven_labs_without_history() {
        let settings: serde_json::Value =
            serde_json::from_str(&settings_message(&SynthesisRequest {
                script: "x",
                voice_id: "UgBBYS2sOqTuMpoF3BR0",
                model: "eleven_v4",
            }))
            .unwrap_or_default();
        assert_eq!(settings["type"], "Settings");
        assert_eq!(settings["mip_opt_out"], true);
        assert_eq!(settings["flags"]["history"], false);
        assert_eq!(settings["audio"]["output"]["container"], "none");
        assert_eq!(settings["audio"]["output"]["sample_rate"], 24_000);
        assert_eq!(
            settings["agent"]["speak"]["provider"]["type"],
            "eleven_labs"
        );
        assert_eq!(
            settings["agent"]["speak"]["provider"]["model_id"],
            "eleven_v4"
        );
        assert_eq!(
            settings["agent"]["speak"]["provider"]["voice_id"],
            "UgBBYS2sOqTuMpoF3BR0"
        );
    }

    #[test]
    fn transcriptions_read_words_language_duration_and_confidence() -> crate::AppResult<()> {
        let body = serde_json::json!({
            "metadata": {"duration": 9.6},
            "results": {"channels": [{"detected_language": "en",
                "alternatives": [{"transcript": " Hello there. ", "confidence": 0.98}]}]}
        })
        .to_string();
        let parsed = parse_transcription(body.as_bytes(), None)?;
        assert_eq!(parsed.transcript, "Hello there.");
        assert_eq!(parsed.language.as_deref(), Some("en"));
        assert_eq!(parsed.duration_milliseconds, Some(9_600));
        assert_eq!(parsed.confidence, Some(0.98));
        let hinted = parse_transcription(body.as_bytes(), Some("fr".to_owned()))?;
        assert_eq!(hinted.language.as_deref(), Some("fr"), "the hint wins");
        let empty = parse_transcription(
            br#"{"metadata":{"duration":1.0},"results":{"channels":[{"alternatives":[{"transcript":"","confidence":0}]}]}}"#,
            None,
        )?;
        assert_eq!(empty.transcript, "");
        assert!(parse_transcription(b"not json", None).is_err());
        Ok(())
    }
}
