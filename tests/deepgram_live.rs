//! A smoke test against the real Deepgram (ignored by default): one script rendered through
//! the Voice Agent (managed `ElevenLabs`) and its audio transcribed through `/v1/listen`, both
//! through DM's own routes. It reads the key at run time from the file named by
//! `DM_SMOKE_ENV_FILE` (default: the silicon-waveform `.env`, variable
//! `WAVEFORM_DEEPGRAM_API_KEY`) and never prints it.
//!
//! ```sh
//! DM_TEST_POSTGRES_URL=postgres://postgres@127.0.0.1:5500/postgres \
//!   cargo test -p silicon-dm --test deepgram_live -- --ignored --nocapture
//! ```
mod support;

use std::time::Instant;

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use secrecy::SecretString;
use serde_json::json;
use support::{
    BoxError, TestDatabase,
    accounts::TestAccount,
    app::{TestApp, data_of},
};

const DEFAULT_ENV_FILE: &str = "/Users/codanium/Documents/silicon/silicon-waveform/.env";
const KEY_VARIABLE: &str = "WAVEFORM_DEEPGRAM_API_KEY";

/// The key, read by variable name from the env file; never printed.
fn deepgram_key() -> Result<SecretString, BoxError> {
    let path = std::env::var("DM_SMOKE_ENV_FILE").unwrap_or_else(|_| DEFAULT_ENV_FILE.to_owned());
    let text =
        std::fs::read_to_string(&path).map_err(|error| format!("cannot read {path}: {error}"))?;
    let value = text
        .lines()
        .find_map(|line| line.trim().strip_prefix(&format!("{KEY_VARIABLE}=")))
        .map(|value| value.trim().trim_matches(['"', '\'']).to_owned())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("{KEY_VARIABLE} is not set in {path}"))?;
    Ok(SecretString::from(value))
}

#[tokio::test]
#[ignore = "calls the real Deepgram with a key read from silicon-waveform/.env"]
async fn a_script_renders_through_the_voice_agent_and_transcribes_back() -> Result<(), BoxError> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let key = deepgram_key()?;
    let app = TestApp::start_configured(TestDatabase::start().await?, "", move |settings| {
        settings.speech.deepgram_api_key = Some(key);
    })
    .await?;
    let ada = TestAccount::carbon("a8K", "c:ada");
    app.accounts.add_accounts(&[&ada]).await;
    app.accounts.live_introspection().await;
    let token = app.accounts.token(&ada);
    let script = "[excited] It works! [laughs] Voice messages are live in DM. [whispers] Nobody else knows yet. [/whispers]";
    let mut report = Vec::new();
    for voice in ["mark", "brittney", "jamal", "jessa", "hope"] {
        let started = Instant::now();
        let (status, headers, wav) = app
            .post_envelope(
                "/api/v1/speech",
                &token,
                &json!({"type": "speech.render", "data": {"script": script, "voice": voice}}),
            )
            .await?;
        let render_seconds = started.elapsed().as_secs_f64();
        assert_eq!(
            status,
            StatusCode::OK,
            "{voice}: {}",
            String::from_utf8_lossy(&wav)
        );
        assert_eq!(headers["x-dm-cache"], "miss");
        let audio_seconds = headers["x-dm-duration-ms"].to_str()?.parse::<f64>()? / 1_000.0;
        assert!(audio_seconds > 1.0, "{voice}: {audio_seconds} s of audio");

        let started = Instant::now();
        let request = Request::builder()
            .method(Method::POST)
            .uri("/api/v1/speech/transcriptions?language=en")
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "audio/wav")
            .header("content-length", wav.len().to_string())
            .body(Body::from(wav.clone()))?;
        let (status, _, body) = app.raw(request).await?;
        let transcribe_seconds = started.elapsed().as_secs_f64();
        let transcription = data_of(&body);
        assert_eq!(status, StatusCode::OK, "{voice}: {transcription}");
        let transcript = transcription["transcript"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let lower = transcript.to_lowercase();
        assert!(lower.contains("works"), "{voice}: {transcript}");
        for cue in ["excited", "laughs", "whispers"] {
            assert!(
                !lower.contains(cue),
                "{voice} read the cue {cue} aloud: {transcript}"
            );
        }
        report.push(format!(
            "{voice}: render {render_seconds:.2} s, {audio_seconds:.2} s of audio, transcription {transcribe_seconds:.2} s, confidence {}: {transcript}",
            transcription["confidence"]
        ));
    }
    for line in report {
        println!("{line}");
    }
    app.close().await
}
