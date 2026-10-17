//! Transcription: raw audio streams to Deepgram's `/v1/listen` with DM's options, language
//! hints pass through, media types and sizes are checked before anything is sent, and the
//! per-account rate applies.
#![allow(
    clippy::too_many_lines,
    reason = "each test is one scenario, read top to bottom"
)]
mod support;

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use serde_json::Value;
use support::{
    BoxError,
    accounts::TestAccount,
    app::{TestApp, data_of},
    deepgram::{DEEPGRAM_TEST_KEY, FakeDeepgram, ListenMode},
    within_one_minute,
};

async fn signed_in(app: &TestApp) -> String {
    let ada = TestAccount::carbon("a8K", "c:ada");
    app.accounts.add_accounts(&[&ada]).await;
    app.accounts.live_introspection().await;
    app.accounts.token(&ada)
}

async fn transcribe(
    app: &TestApp,
    token: &str,
    query: &str,
    content_type: Option<&str>,
    body: Body,
    content_length: Option<usize>,
) -> Result<(StatusCode, axum::http::HeaderMap, Value), BoxError> {
    let mut request = Request::builder()
        .method(Method::POST)
        .uri(format!("/api/v1/speech/transcriptions{query}"))
        .header("authorization", format!("Bearer {token}"));
    if let Some(content_type) = content_type {
        request = request.header("content-type", content_type);
    }
    if let Some(length) = content_length {
        request = request.header("content-length", length.to_string());
    }
    let (status, headers, bytes) = app.raw(request.body(body)?).await?;
    Ok((status, headers, data_of(&bytes)))
}

fn audio(bytes: usize) -> Vec<u8> {
    (0..bytes)
        .map(|index| u8::try_from(index % 251).unwrap_or(0))
        .collect()
}

#[tokio::test]
async fn audio_streams_to_deepgram_with_dms_options_and_language_hints() -> Result<(), BoxError> {
    let deepgram = FakeDeepgram::start().await;
    let app = TestApp::start_with_speech(&deepgram, |_| {}).await?;
    let token = signed_in(&app).await;

    let body = audio(48_000);
    let (status, _, data) = transcribe(
        &app,
        &token,
        "",
        Some("audio/webm;codecs=opus"),
        Body::from(body.clone()),
        Some(body.len()),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{data}");
    assert_eq!(data["transcript"], "I heard 48000 bytes.");
    assert_eq!(data["language"], "en", "detected");
    assert_eq!(data["duration_milliseconds"], 1_500);
    assert_eq!(data["confidence"], 0.98);
    let listens = deepgram.listens();
    let request = &listens[0];
    assert_eq!(request.bytes, 48_000, "every byte arrived");
    assert_eq!(request.content_type.as_deref(), Some("audio/webm"));
    assert_eq!(
        request.authorization.as_deref(),
        Some(format!("Token {DEEPGRAM_TEST_KEY}").as_str())
    );
    for (name, value) in [
        ("model", "nova-3"),
        ("smart_format", "true"),
        ("punctuate", "true"),
        ("mip_opt_out", "true"),
        ("detect_language", "true"),
    ] {
        assert_eq!(
            request.query.get(name).map(String::as_str),
            Some(value),
            "{name}"
        );
    }
    assert!(!request.query.contains_key("language"));

    // A language hint replaces detection; a streamed body without a length works too.
    let stream = futures::stream::iter(
        audio(64_000)
            .chunks(8_000)
            .map(|chunk| Ok::<_, std::io::Error>(axum::body::Bytes::copy_from_slice(chunk)))
            .collect::<Vec<_>>(),
    );
    let (status, _, data) = transcribe(
        &app,
        &token,
        "?language=pt-BR",
        Some("audio/x-wav"),
        Body::from_stream(stream),
        None,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{data}");
    assert_eq!(data["language"], "pt-BR");
    let listens = deepgram.listens();
    let request = &listens[1];
    assert_eq!(request.bytes, 64_000);
    assert_eq!(
        request.content_type.as_deref(),
        Some("audio/wav"),
        "aliases are normalised"
    );
    assert_eq!(
        request.query.get("language").map(String::as_str),
        Some("pt-BR")
    );
    assert!(!request.query.contains_key("detect_language"));

    // Silence is a valid answer: the client decides what to do with nothing.
    deepgram.set_listen_mode(ListenMode::Empty);
    let (status, _, data) = transcribe(
        &app,
        &token,
        "",
        Some("audio/ogg"),
        Body::from(audio(1_000)),
        Some(1_000),
    )
    .await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(data["transcript"], "");
    deepgram.set_listen_mode(ListenMode::Unreadable);
    let (status, _, data) = transcribe(
        &app,
        &token,
        "",
        Some("audio/mpeg"),
        Body::from(audio(10)),
        Some(10),
    )
    .await?;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(data["error"]["code"], "audio_unreadable");
    assert!(
        data["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("corrupt or unsupported")),
        "{data}"
    );
    deepgram.set_listen_mode(ListenMode::Unavailable);
    let (status, headers, data) = transcribe(
        &app,
        &token,
        "",
        Some("audio/flac"),
        Body::from(audio(10)),
        Some(10),
    )
    .await?;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(data["error"]["code"], "speech_unavailable");
    assert!(headers.contains_key("retry-after"));
    app.close().await
}

#[tokio::test]
async fn types_sizes_and_languages_are_checked_before_deepgram_is_called() -> Result<(), BoxError> {
    let deepgram = FakeDeepgram::start().await;
    let app = TestApp::start_with_speech(&deepgram, |speech| speech.transcribe_max_bytes = 32_000)
        .await?;
    let token = signed_in(&app).await;
    for content_type in [Some("video/mp4"), Some("application/json"), None] {
        let (status, _, data) = transcribe(
            &app,
            &token,
            "",
            content_type,
            Body::from(audio(100)),
            Some(100),
        )
        .await?;
        assert_eq!(
            status,
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "{content_type:?}"
        );
        assert_eq!(data["error"]["code"], "unsupported_audio_type");
        assert_eq!(data["error"]["details"]["accepted"][0], "audio/webm");
    }
    let (status, _, data) = transcribe(
        &app,
        &token,
        "",
        Some("audio/webm"),
        Body::from(audio(40_000)),
        Some(40_000),
    )
    .await?;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(data["error"]["code"], "audio_too_large");
    assert_eq!(data["error"]["details"]["max_bytes"], 32_000);
    let (status, _, data) = transcribe(
        &app,
        &token,
        "?language=english_please",
        Some("audio/webm"),
        Body::from(audio(100)),
        Some(100),
    )
    .await?;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        data["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("language tag")),
        "{data}"
    );
    let (status, _, data) =
        transcribe(&app, &token, "", Some("audio/webm"), Body::empty(), Some(0)).await?;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(data["error"]["code"], "audio_empty");
    assert!(deepgram.listens().is_empty(), "nothing reached Deepgram");

    // A body without a length is cut off as soon as it passes the limit.
    let stream = futures::stream::iter(
        audio(64_000)
            .chunks(8_000)
            .map(|chunk| Ok::<_, std::io::Error>(axum::body::Bytes::copy_from_slice(chunk)))
            .collect::<Vec<_>>(),
    );
    let (status, _, data) = transcribe(
        &app,
        &token,
        "",
        Some("audio/webm"),
        Body::from_stream(stream),
        None,
    )
    .await?;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{data}");
    assert_eq!(data["error"]["code"], "audio_too_large");
    assert!(
        deepgram
            .listens()
            .iter()
            .all(|request| request.bytes <= 32_000),
        "Deepgram never got more than the limit"
    );
    // Anonymous callers are refused.
    let request = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/speech/transcriptions")
        .header("content-type", "audio/webm")
        .body(Body::from(audio(10)))?;
    let (status, _, _) = app.raw(request).await?;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    app.close().await
}

#[tokio::test]
async fn transcriptions_are_rate_limited_per_account_and_need_a_provider() -> Result<(), BoxError> {
    let deepgram = FakeDeepgram::start().await;
    let app =
        TestApp::start_with_speech(&deepgram, |speech| speech.transcribe_per_minute = 2).await?;
    let token = signed_in(&app).await;
    within_one_minute().await;
    for _ in 0..2 {
        let (status, _, _) = transcribe(
            &app,
            &token,
            "",
            Some("audio/webm"),
            Body::from(audio(100)),
            Some(100),
        )
        .await?;
        assert_eq!(status, StatusCode::OK);
    }
    let (status, headers, data) = transcribe(
        &app,
        &token,
        "",
        Some("audio/webm"),
        Body::from(audio(100)),
        Some(100),
    )
    .await?;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(data["error"]["code"], "transcription_rate_limited");
    assert_eq!(data["error"]["details"]["limit"], 2);
    assert!(headers.contains_key("retry-after"));
    assert_eq!(deepgram.listens().len(), 2);
    app.close().await?;

    let off = TestApp::start().await?;
    let token = signed_in(&off).await;
    let (status, _, data) = transcribe(
        &off,
        &token,
        "",
        Some("audio/webm"),
        Body::from(audio(100)),
        Some(100),
    )
    .await?;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(data["error"]["code"], "speech_not_configured");
    off.close().await
}
