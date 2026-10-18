//! Speech: spoken messages pinned to a voice and pre-rendered by the worker, listening to
//! any message (cache hits, misses, ranges, `ETag`s), rendering scripts, the per-account limits,
//! provider failures, and a DM without a speech provider.
#![allow(
    clippy::too_many_lines,
    reason = "each test is one scenario, read top to bottom"
)]
mod support;

use axum::http::{Method, StatusCode};
use serde_json::{Value, json};
use silicon_dm::{domain::VoiceCatalog, worker::SpeechWorker};
use support::{
    BoxError,
    accounts::TestAccount,
    app::{TestApp, data_of, expect_status},
    deepgram::{AgentMode, DEEPGRAM_TEST_KEY, FakeDeepgram},
    within_one_minute,
};

struct People {
    ada: TestAccount,
    bob: TestAccount,
    scout: TestAccount,
}

async fn people(app: &TestApp) -> People {
    let ada = TestAccount::carbon("a8K", "c:ada");
    let bob = TestAccount::carbon("Bb2", "c:bob");
    let scout = TestAccount::silicon("Sc0", "si:scout", &ada);
    app.accounts.add_accounts(&[&ada, &bob, &scout]).await;
    app.accounts.live_introspection().await;
    People { ada, bob, scout }
}

fn code(message: &Value) -> String {
    message["message-id"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

fn default_voice(uuid: &str) -> String {
    VoiceCatalog::default().default_for(uuid).key.clone()
}

fn voice_id(key: &str) -> String {
    VoiceCatalog::default()
        .get(key)
        .map(|voice| voice.voice_id.clone())
        .unwrap_or_default()
}

async fn render_queue(app: &TestApp) -> Result<usize, BoxError> {
    Ok(SpeechWorker::new(app.state.clone()).process_once().await?)
}

async fn sync(app: &TestApp, token: &str, query: &str) -> Result<Value, BoxError> {
    let page = app.get(&format!("/api/v1/sync?{query}"), token).await?;
    expect_status(&page, StatusCode::OK)?;
    Ok(page.1)
}

fn events_for(page: &Value, message: &str) -> usize {
    page["events"]
        .as_array()
        .map(|events| {
            events
                .iter()
                .filter(|event| event["message_id"] == message && event["type"] == "message")
                .count()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn spoken_messages_are_pinned_rendered_and_announced() -> Result<(), BoxError> {
    let deepgram = FakeDeepgram::start().await;
    let app = TestApp::start_with_speech(&deepgram, |_| {}).await?;
    let p = people(&app).await;
    let scout = app.accounts.token(&p.scout);
    let ada = app.accounts.token(&p.ada);
    let start = sync(&app, &ada, "reset=true").await?;
    let cursor = start["cursor"].as_str().unwrap_or_default().to_owned();

    let sent = app
        .post(
            "/api/v1/messages",
            &scout,
            json!({"recipient_id": "c:ada",
                "speech": {"script": "[excited] It works! [laughs]"},
                "voice_transcript": "something else", "metadata": {"source": "test"}}),
        )
        .await?;
    expect_status(&sent, StatusCode::ACCEPTED)?;
    let message = code(&sent.1);
    let voice = default_voice("Sc0");
    assert_eq!(
        sent.1["message"], "",
        "a speech-only message has no caption"
    );
    assert_eq!(
        sent.1["voice_transcript"], "It works!",
        "the transcript is the script's words, set by DM"
    );
    assert_eq!(sent.1["metadata"]["source"], "test");
    assert_eq!(
        sent.1["speech"],
        json!({"voice": voice, "voice_name": VoiceCatalog::default().get(&voice).map(|v| v.name.clone()),
            "script": "[excited] It works! [laughs]", "model": "eleven_v4", "status": "pending",
            "duration_milliseconds": null, "peaks": null})
    );
    let first = sync(&app, &ada, &format!("cursor={cursor}")).await?;
    assert_eq!(events_for(&first, &message), 1, "{first}");
    let cursor = first["cursor"].as_str().unwrap_or_default().to_owned();

    assert_eq!(render_queue(&app).await?, 1);
    assert_eq!(render_queue(&app).await?, 0, "nothing is rendered twice");
    let sessions = deepgram.sessions();
    assert_eq!(sessions.len(), 1);
    let session = &sessions[0];
    assert_eq!(
        session.authorization.as_deref(),
        Some(format!("Token {DEEPGRAM_TEST_KEY}").as_str())
    );
    let settings = session.settings.clone().unwrap_or_default();
    assert_eq!(settings["mip_opt_out"], true);
    assert_eq!(settings["audio"]["output"]["encoding"], "linear16");
    assert_eq!(settings["audio"]["output"]["container"], "none");
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
        voice_id(&voice)
    );
    assert_eq!(session.injections, vec!["[excited] It works! [laughs]"]);

    let path = format!("/api/v1/conversations/c:ada::si:scout/messages/{message}");
    let ready = app.get(&path, &ada).await?;
    expect_status(&ready, StatusCode::OK)?;
    assert_eq!(ready.1["speech"]["status"], "ready");
    assert_eq!(ready.1["speech"]["duration_milliseconds"], 200, "two words");
    let peaks = ready.1["speech"]["peaks"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(peaks.len(), 64);
    assert!(
        peaks
            .iter()
            .all(|peak| peak.as_i64().is_some_and(|v| (0..=100).contains(&v)))
    );
    assert!(peaks.iter().any(|peak| peak == 100));
    // Every participant hears about it, so clients refresh the message.
    let announced = sync(&app, &ada, &format!("cursor={cursor}")).await?;
    assert_eq!(events_for(&announced, &message), 1, "{announced}");
    let listed = app.get("/api/v1/conversations", &ada).await?;
    assert_eq!(
        listed.1["items"][0]["last_message"]["speech"]["status"], "ready",
        "{}",
        listed.1
    );
    app.close().await
}

#[tokio::test]
async fn voices_are_pinned_on_messages_and_edits_keep_theirs() -> Result<(), BoxError> {
    let deepgram = FakeDeepgram::start().await;
    let app = TestApp::start_with_speech(&deepgram, |_| {}).await?;
    let p = people(&app).await;
    let scout = app.accounts.token(&p.scout);
    let ada = app.accounts.token(&p.ada);
    let conversation = "/api/v1/conversations/c:ada::si:scout/messages";
    let before = app
        .post(
            conversation,
            &scout,
            json!({"speech": {"script": "Morning."}}),
        )
        .await?;
    expect_status(&before, StatusCode::ACCEPTED)?;
    assert_eq!(before.1["speech"]["voice"], default_voice("Sc0"));
    // The custodian changes the Silicon's voice: new messages use it, old ones do not.
    let changed = app
        .call(
            Method::PUT,
            "/api/v1/voices/si:scout",
            Some(&ada),
            Some(json!({"voice": "hope"})),
            &[],
        )
        .await?;
    expect_status(&changed, StatusCode::OK)?;
    let after = app
        .post(
            conversation,
            &scout,
            json!({"speech": {"script": "Afternoon."}}),
        )
        .await?;
    assert_eq!(after.1["speech"]["voice"], "hope");
    assert_eq!(after.1["speech"]["voice_name"], "Hope");
    let old = app
        .get(&format!("{conversation}/{}", code(&before.1)), &ada)
        .await?;
    assert_eq!(old.1["speech"]["voice"], default_voice("Sc0"));
    // A voice named in the request wins; an edit without one keeps it.
    let explicit = app
        .post(
            conversation,
            &scout,
            json!({"message": "caption", "speech": {"script": "Evening.", "voice": "jamal"}}),
        )
        .await?;
    assert_eq!(explicit.1["speech"]["voice"], "jamal");
    assert_eq!(explicit.1["message"], "caption");
    render_queue(&app).await?;
    let edited = app
        .call(
            Method::PATCH,
            &format!("{conversation}/{}", code(&explicit.1)),
            Some(&scout),
            Some(json!({"message": "caption", "speech": {"script": "[warmly] Good evening."}})),
            &[],
        )
        .await?;
    expect_status(&edited, StatusCode::OK)?;
    assert_eq!(edited.1["speech"]["voice"], "jamal");
    assert_eq!(edited.1["speech"]["script"], "[warmly] Good evening.");
    assert_eq!(edited.1["speech"]["status"], "pending");
    assert_eq!(edited.1["voice_transcript"], "Good evening.");
    assert_eq!(edited.1["history"][0]["speech"]["script"], "Evening.");
    assert_eq!(
        edited.1["history"][0]["speech"]["status"], "ready",
        "the old version was rendered"
    );
    assert_eq!(render_queue(&app).await?, 1);
    let rendered = app
        .get(&format!("{conversation}/{}", code(&explicit.1)), &ada)
        .await?;
    assert_eq!(rendered.1["speech"]["status"], "ready");
    // An edit can drop the speech; a later edit can bring it back.
    let text = app
        .call(
            Method::PATCH,
            &format!("{conversation}/{}", code(&explicit.1)),
            Some(&scout),
            Some(json!({"message": "just text now"})),
            &[],
        )
        .await?;
    expect_status(&text, StatusCode::OK)?;
    assert_eq!(text.1["speech"], Value::Null);
    app.close().await
}

#[tokio::test]
async fn listening_serves_cached_audio_with_ranges_etags_and_renders_misses() -> Result<(), BoxError>
{
    let deepgram = FakeDeepgram::start().await;
    let app = TestApp::start_with_speech(&deepgram, |_| {}).await?;
    let p = people(&app).await;
    let scout = app.accounts.token(&p.scout);
    let ada = app.accounts.token(&p.ada);
    let spoken = app
        .post(
            "/api/v1/conversations/c:ada::si:scout/messages",
            &scout,
            json!({"speech": {"script": "[warmly] Hello there."}}),
        )
        .await?;
    render_queue(&app).await?;
    let speech = format!(
        "/api/v1/conversations/c:ada::si:scout/messages/{}/speech",
        code(&spoken.1)
    );
    let (status, headers, body) = app.get_raw(&speech, &ada, &[]).await?;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert_eq!(headers["content-type"], "audio/wav");
    assert_eq!(headers["x-dm-cache"], "hit", "pre-rendered");
    assert_eq!(headers["x-dm-voice"], default_voice("Sc0").as_str());
    assert_eq!(headers["x-dm-duration-ms"], "200");
    assert_eq!(headers["accept-ranges"], "bytes");
    assert_eq!(headers["cache-control"], "private, no-cache");
    // 200 ms of 24 kHz 16-bit mono PCM behind a 44-byte header.
    assert_eq!(body.len(), 44 + 9_600);
    assert_eq!(headers["content-length"], (44 + 9_600).to_string().as_str());
    assert_eq!(&body[..4], b"RIFF");
    assert_eq!(&body[8..12], b"WAVE");
    let etag = headers["etag"].to_str()?.to_owned();
    assert!(etag.starts_with('"') && etag.ends_with('"'));

    let (status, headers, part) = app
        .get_raw(&speech, &ada, &[("range", "bytes=0-43")])
        .await?;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(headers["content-range"], "bytes 0-43/9644");
    assert_eq!(headers["content-length"], "44");
    assert_eq!(&part[..], &body[..44]);
    let (status, _, tail) = app
        .get_raw(&speech, &ada, &[("range", "bytes=-100")])
        .await?;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(&tail[..], &body[body.len() - 100..]);
    let (status, headers, _) = app
        .get_raw(&speech, &ada, &[("range", "bytes=99999-")])
        .await?;
    assert_eq!(status, StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(headers["content-range"], "bytes */9644");
    let (status, _, empty) = app
        .get_raw(&speech, &ada, &[("if-none-match", etag.as_str())])
        .await?;
    assert_eq!(status, StatusCode::NOT_MODIFIED);
    assert!(empty.is_empty());
    let (status, _, whole) = app
        .get_raw(
            &speech,
            &ada,
            &[("range", "bytes=0-9"), ("if-range", "\"stale\"")],
        )
        .await?;
    assert_eq!(status, StatusCode::OK, "a stale If-Range sends everything");
    assert_eq!(whole.len(), body.len());
    assert_eq!(deepgram.sessions().len(), 1, "cache hits never render");

    // A text message is read in its sender's current voice, rendered on the first listen.
    let text = app
        .post(
            "/api/v1/conversations/c:ada::si:scout/messages",
            &ada,
            json!({"message": "See [docs] at https://example.com/x now"}),
        )
        .await?;
    let listen = format!(
        "/api/v1/conversations/c:ada::si:scout/messages/{}/speech",
        code(&text.1)
    );
    let (status, headers, _) = app.get_raw(&listen, &scout, &[]).await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["x-dm-cache"], "miss");
    assert_eq!(headers["x-dm-voice"], default_voice("a8K").as_str());
    let sessions = deepgram.sessions();
    assert_eq!(
        sessions.last().map(|session| session.injections.clone()),
        Some(vec!["See (docs) at link now".to_owned()]),
        "brackets in text are words, links are read as link"
    );
    let (_, headers, _) = app.get_raw(&listen, &scout, &[]).await?;
    assert_eq!(headers["x-dm-cache"], "hit");
    assert_eq!(deepgram.sessions().len(), 2);
    app.close().await
}

#[tokio::test]
async fn listening_needs_words_a_live_message_and_read_access() -> Result<(), BoxError> {
    let deepgram = FakeDeepgram::start().await;
    let app = TestApp::start_with_speech(&deepgram, |_| {}).await?;
    let p = people(&app).await;
    let scout = app.accounts.token(&p.scout);
    let ada = app.accounts.token(&p.ada);
    let bob = app.accounts.token(&p.bob);
    // DM creates a Silicon's conversation with its custodian when it first sees the Silicon.
    expect_status(&app.get("/api/v1/me", &scout).await?, StatusCode::OK)?;
    let base = "/api/v1/conversations/c:ada::si:scout/messages";
    let gif = app
        .post(
            base,
            &ada,
            json!({"gif": {"provider_id": "g1", "url": "https://media.giphy.com/g1.gif"}}),
        )
        .await?;
    expect_status(&gif, StatusCode::ACCEPTED)?;
    let (status, _, body) = app
        .get_raw(&format!("{base}/{}/speech", code(&gif.1)), &scout, &[])
        .await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(data_of(&body)["error"]["code"], "nothing_to_speak");

    let spoken = app
        .post(base, &scout, json!({"speech": {"script": "Soon gone."}}))
        .await?;
    let deleted = app
        .call(
            Method::DELETE,
            &format!("{base}/{}", code(&spoken.1)),
            Some(&scout),
            None,
            &[],
        )
        .await?;
    expect_status(&deleted, StatusCode::OK)?;
    assert_eq!(deleted.1["speech"], Value::Null, "tombstones hide speech");
    let (status, _, body) = app
        .get_raw(&format!("{base}/{}/speech", code(&spoken.1)), &ada, &[])
        .await?;
    assert_eq!(status, StatusCode::GONE);
    assert_eq!(data_of(&body)["error"]["code"], "message_deleted");
    // The worker never renders a deleted message.
    assert_eq!(render_queue(&app).await?, 1);
    assert!(deepgram.sessions().is_empty());

    // Someone outside the conversation cannot listen; a custodian reading its Silicon's
    // conversation can.
    let with_bob = app
        .post(
            "/api/v1/messages",
            &scout,
            json!({"recipient_id": "c:bob", "speech": {"script": "Hi Bob."}}),
        )
        .await?;
    expect_status(&with_bob, StatusCode::ACCEPTED)?;
    let bob_path = format!(
        "/api/v1/conversations/c:bob::si:scout/messages/{}/speech",
        code(&with_bob.1)
    );
    let (status, _, _) = app
        .get_raw(&format!("{base}/{}/speech", code(&gif.1)), &bob, &[])
        .await?;
    assert_eq!(status, StatusCode::NOT_FOUND, "not bob's conversation");
    // A listener who arrives while the pre-render is under way waits for it: the audio is
    // rendered once, by the worker, and the listener is not charged.
    let worker = {
        let state = app.state.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            SpeechWorker::new(state).process_once().await
        })
    };
    let (status, headers, _) = app.get_raw(&bob_path, &ada, &[]).await?;
    assert_eq!(status, StatusCode::OK, "the custodian reads it");
    assert_eq!(headers["x-dm-cache"], "hit", "the worker's rendering");
    assert_eq!(worker.await??, 1);
    assert_eq!(deepgram.sessions().len(), 1, "rendered once");
    let (status, _, _) = app
        .get_raw(&format!("{bob_path}?for=si:scout"), &ada, &[])
        .await?;
    assert_eq!(status, StatusCode::OK);
    let (status, _, body) = app
        .get_raw(&format!("{bob_path}?for=si:scout"), &bob, &[])
        .await?;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(data_of(&body)["error"]["code"], "not_custodian");

    // A pre-render waiting out a retry delay is not waited for: listening renders it now and
    // that completes the message.
    let later = app
        .post(
            "/api/v1/conversations/c:bob::si:scout/messages",
            &scout,
            json!({"speech": {"script": "Later, Bob."}}),
        )
        .await?;
    sqlx::query(
        "UPDATE speech_renders SET next_attempt_at = clock_timestamp() + interval '1 hour'
         WHERE script = 'Later, Bob.'",
    )
    .execute(app.state.store.pool())
    .await?;
    let later_path = format!(
        "/api/v1/conversations/c:bob::si:scout/messages/{}",
        code(&later.1)
    );
    let (status, headers, _) = app
        .get_raw(&format!("{later_path}/speech"), &bob, &[])
        .await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["x-dm-cache"], "miss", "rendered on demand");
    let message = app.get(&later_path, &bob).await?;
    assert_eq!(message.1["speech"]["status"], "ready");
    assert_eq!(render_queue(&app).await?, 0, "nothing left to render");
    app.close().await
}

#[tokio::test]
async fn rendering_scripts_and_the_per_account_limits() -> Result<(), BoxError> {
    let deepgram = FakeDeepgram::start().await;
    let app = TestApp::start_with_speech(&deepgram, |speech| {
        speech.renders_per_minute = 2;
        speech.daily_characters = 40;
    })
    .await?;
    let p = people(&app).await;
    let ada = app.accounts.token(&p.ada);
    within_one_minute().await;
    let render = |script: &str, voice: Option<&str>| {
        let mut data = json!({"script": script});
        if let Some(voice) = voice {
            data["voice"] = json!(voice);
        }
        json!({"type": "speech.render", "data": data})
    };
    let (status, headers, body) = app
        .post_envelope("/api/v1/speech", &ada, &render("Hi.", Some("mark")))
        .await?;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert_eq!(headers["content-type"], "audio/wav");
    assert_eq!(headers["x-dm-cache"], "miss");
    assert_eq!(headers["x-dm-voice"], "mark");
    assert_eq!(&body[..4], b"RIFF");
    let (_, headers, _) = app
        .post_envelope("/api/v1/speech", &ada, &render("Hi.", Some("mark")))
        .await?;
    assert_eq!(
        headers["x-dm-cache"], "hit",
        "the same audio is never rendered twice"
    );
    // Without a voice: the caller's own.
    let (status, headers, _) = app
        .post_envelope("/api/v1/speech", &ada, &render("Hello.", None))
        .await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["x-dm-voice"], default_voice("a8K").as_str());
    // A third render this minute is refused; cached audio still plays.
    let (status, headers, body) = app
        .post_envelope("/api/v1/speech", &ada, &render("One more.", Some("mark")))
        .await?;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    let error = data_of(&body);
    assert_eq!(error["error"]["code"], "speech_rate_limited");
    assert_eq!(error["error"]["details"]["limit"], 2);
    assert!(headers.contains_key("retry-after"));
    let (status, _, _) = app
        .post_envelope("/api/v1/speech", &ada, &render("Hi.", Some("mark")))
        .await?;
    assert_eq!(status, StatusCode::OK);
    // Bad input is explained.
    let (status, _, body) = app
        .post_envelope("/api/v1/speech", &ada, &render("[excited It works", None))
        .await?;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let error = data_of(&body);
    assert_eq!(error["error"]["code"], "invalid_speech_script");
    assert!(
        error["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("position 1")),
        "{error}"
    );
    let (status, _, body) = app
        .post_envelope("/api/v1/speech", &ada, &render("Hi.", Some("george")))
        .await?;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(data_of(&body)["error"]["code"], "voice_not_found");
    app.close().await?;

    // The daily character budget, and limits on spoken messages (charged to the sender).
    let deepgram = FakeDeepgram::start().await;
    let app = TestApp::start_with_speech(&deepgram, |speech| {
        speech.renders_per_minute = 1;
        speech.daily_characters = 30;
    })
    .await?;
    let p = people(&app).await;
    let scout = app.accounts.token(&p.scout);
    let base = "/api/v1/conversations/c:ada::si:scout/messages";
    within_one_minute().await;
    let first = app
        .post(base, &scout, json!({"speech": {"script": "First."}}))
        .await?;
    expect_status(&first, StatusCode::ACCEPTED)?;
    let second = app
        .post(base, &scout, json!({"speech": {"script": "Second."}}))
        .await?;
    assert_eq!(second.0, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(second.1["error"]["code"], "speech_rate_limited");
    let listed = app.get(base, &scout).await?;
    assert_eq!(
        listed.1["items"].as_array().map(Vec::len),
        Some(1),
        "a refused message is not stored"
    );
    // Editing only the caption keeps the queued rendering: nothing is charged again.
    let caption = app
        .call(
            Method::PATCH,
            &format!("{base}/{}", code(&first.1)),
            Some(&scout),
            Some(json!({"message": "a caption", "speech": {"script": "First."}})),
            &[],
        )
        .await?;
    expect_status(&caption, StatusCode::OK)?;
    let jobs: i64 = sqlx::query_scalar("SELECT count(*) FROM speech_renders")
        .fetch_one(app.state.store.pool())
        .await?;
    assert_eq!(jobs, 1, "one rendering for one version");
    sqlx::query("DELETE FROM speech_usage WHERE meter = 'renders'")
        .execute(app.state.store.pool())
        .await?;
    let long = app
        .post(
            base,
            &scout,
            json!({"speech": {"script": "This one is far too long for the budget."}}),
        )
        .await?;
    assert_eq!(long.0, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(long.1["error"]["code"], "speech_budget_exceeded");
    assert_eq!(long.1["error"]["details"]["limit"], 30);
    // The same script again is already cached: free.
    render_queue(&app).await?;
    let again = app
        .post(base, &scout, json!({"speech": {"script": "First."}}))
        .await?;
    expect_status(&again, StatusCode::ACCEPTED)?;
    assert_eq!(again.1["speech"]["status"], "ready", "already rendered");
    app.close().await
}

#[tokio::test]
async fn provider_failures_are_explained_and_pre_renders_retry_then_fail() -> Result<(), BoxError> {
    let deepgram = FakeDeepgram::start().await;
    let app = TestApp::start_with_speech(&deepgram, |_| {}).await?;
    let p = people(&app).await;
    let ada = app.accounts.token(&p.ada);
    let scout = app.accounts.token(&p.scout);
    let render = |script: &str| json!({"type": "speech.render", "data": {"script": script}});
    for (mode, status, error) in [
        (
            AgentMode::FailToSpeak,
            StatusCode::UNPROCESSABLE_ENTITY,
            "speech_rejected",
        ),
        (
            AgentMode::InvalidSettings,
            StatusCode::UNPROCESSABLE_ENTITY,
            "speech_rejected",
        ),
        (
            AgentMode::CloseEarly,
            StatusCode::SERVICE_UNAVAILABLE,
            "speech_unavailable",
        ),
        (
            AgentMode::ProviderError,
            StatusCode::SERVICE_UNAVAILABLE,
            "speech_unavailable",
        ),
    ] {
        deepgram.set_agent_mode(mode);
        let (got, headers, body) = app
            .post_envelope("/api/v1/speech", &ada, &render(&format!("Mode {mode:?}.")))
            .await?;
        assert_eq!(got, status, "{mode:?}");
        assert_eq!(data_of(&body)["error"]["code"], error, "{mode:?}");
        assert_eq!(
            headers.contains_key("retry-after"),
            status == StatusCode::SERVICE_UNAVAILABLE,
            "{mode:?}"
        );
    }

    // A pre-render that keeps failing retries with backoff, then fails for good.
    deepgram.set_agent_mode(AgentMode::Speak);
    deepgram.fail_next(5);
    let spoken = app
        .post(
            "/api/v1/conversations/c:ada::si:scout/messages",
            &scout,
            json!({"speech": {"script": "Try again."}}),
        )
        .await?;
    let path = format!(
        "/api/v1/conversations/c:ada::si:scout/messages/{}",
        code(&spoken.1)
    );
    let start = sync(&app, &ada, "reset=true").await?;
    for attempt in 1..=5 {
        assert_eq!(render_queue(&app).await?, 1, "attempt {attempt}");
        let state: (String, i32, bool) = sqlx::query_as(
            "SELECT status, attempts, next_attempt_at > clock_timestamp() FROM speech_renders",
        )
        .fetch_one(app.state.store.pool())
        .await?;
        if attempt < 5 {
            assert_eq!(state, ("pending".to_owned(), attempt, true), "backs off");
            assert_eq!(render_queue(&app).await?, 0, "not before its time");
            sqlx::query("UPDATE speech_renders SET next_attempt_at = clock_timestamp()")
                .execute(app.state.store.pool())
                .await?;
        } else {
            assert_eq!(state.0, "failed");
            assert_eq!(state.1, 5);
        }
    }
    let failed = app.get(&path, &ada).await?;
    assert_eq!(failed.1["speech"]["status"], "failed");
    let announced = sync(
        &app,
        &ada,
        &format!("cursor={}", start["cursor"].as_str().unwrap_or_default()),
    )
    .await?;
    assert_eq!(events_for(&announced, &code(&spoken.1)), 1, "{announced}");
    // Listening still tries, and its success makes the message ready.
    let (status, _, _) = app.get_raw(&format!("{path}/speech"), &ada, &[]).await?;
    assert_eq!(status, StatusCode::OK);
    let ready = app.get(&path, &ada).await?;
    assert_eq!(ready.1["speech"]["status"], "ready");
    // A script the provider refuses fails at once.
    deepgram.set_agent_mode(AgentMode::FailToSpeak);
    app.post(
        "/api/v1/conversations/c:ada::si:scout/messages",
        &scout,
        json!({"speech": {"script": "Refused."}}),
    )
    .await?;
    assert_eq!(render_queue(&app).await?, 1);
    let refused: (String, String) =
        sqlx::query_as("SELECT status, last_error FROM speech_renders WHERE script = 'Refused.'")
            .fetch_one(app.state.store.pool())
            .await?;
    assert_eq!(refused, ("failed".to_owned(), "speech_rejected".to_owned()));
    app.close().await
}

#[tokio::test]
async fn scripts_are_validated_and_one_recording_per_message() -> Result<(), BoxError> {
    let deepgram = FakeDeepgram::start().await;
    let app = TestApp::start_with_speech(&deepgram, |_| {}).await?;
    let p = people(&app).await;
    let scout = app.accounts.token(&p.scout);
    let base = "/api/v1/conversations/c:ada::si:scout/messages";
    for (script, needle) in [
        ("It works]", "position 9"),
        ("[] hi", "empty cue"),
        ("hi [/whispers]", "[whispers]"),
        ("   ", "empty"),
    ] {
        let refused = app
            .post(base, &scout, json!({"speech": {"script": script}}))
            .await?;
        assert_eq!(refused.0, StatusCode::UNPROCESSABLE_ENTITY, "{script}");
        assert_eq!(
            refused.1["error"]["code"], "invalid_speech_script",
            "{script}"
        );
        assert!(
            refused.1["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains(needle)),
            "{script}: {}",
            refused.1
        );
        assert!(refused.1["error"]["hint"].is_string());
    }
    let too_long = app
        .post(
            base,
            &scout,
            json!({"speech": {"script": "a".repeat(5_001)}}),
        )
        .await?;
    assert_eq!(too_long.0, StatusCode::UNPROCESSABLE_ENTITY);
    let both = app
        .post(
            base,
            &scout,
            json!({"speech": {"script": "Hi."}, "voice": {"permanent_url": "https://m.example/v.ogg",
                "duration_milliseconds": 1000}}),
        )
        .await?;
    assert_eq!(both.0, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        both.1["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("not both")),
        "{}",
        both.1
    );
    let unknown = app
        .post(
            base,
            &scout,
            json!({"speech": {"script": "Hi.", "voice": "george"}}),
        )
        .await?;
    assert_eq!(unknown.1["error"]["code"], "voice_not_found");
    // A bundle's display message may speak too.
    let mut codes = Vec::new();
    for text in ["one", "two"] {
        let sent = app.post(base, &scout, json!({"message": text})).await?;
        codes.push(code(&sent.1));
    }
    let bundle = app
        .post(
            "/api/v1/conversations/c:ada::si:scout/bundles",
            &scout,
            json!({"message_ids": codes, "display_message": {"speech": {"script": "One and two."}}}),
        )
        .await?;
    expect_status(&bundle, StatusCode::CREATED)?;
    assert_eq!(bundle.1["display_message"]["speech"]["status"], "pending");
    assert_eq!(render_queue(&app).await?, 1);
    app.close().await
}

#[tokio::test]
async fn without_a_speech_provider_speech_is_refused_clearly() -> Result<(), BoxError> {
    let app = TestApp::start().await?;
    let p = people(&app).await;
    let scout = app.accounts.token(&p.scout);
    let ada = app.accounts.token(&p.ada);
    let spoken = app
        .post(
            "/api/v1/conversations/c:ada::si:scout/messages",
            &scout,
            json!({"speech": {"script": "Hello."}}),
        )
        .await?;
    assert_eq!(spoken.0, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(spoken.1["error"]["code"], "speech_not_configured");
    assert!(spoken.1["error"]["hint"].is_string());
    let (status, _, body) = app
        .post_envelope(
            "/api/v1/speech",
            &ada,
            &json!({"type": "speech.render", "data": {"script": "Hello."}}),
        )
        .await?;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(data_of(&body)["error"]["code"], "speech_not_configured");
    let text = app
        .post(
            "/api/v1/conversations/c:ada::si:scout/messages",
            &ada,
            json!({"message": "Plain text still works."}),
        )
        .await?;
    expect_status(&text, StatusCode::ACCEPTED)?;
    let (status, _, body) = app
        .get_raw(
            &format!(
                "/api/v1/conversations/c:ada::si:scout/messages/{}/speech",
                code(&text.1)
            ),
            &scout,
            &[],
        )
        .await?;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(data_of(&body)["error"]["code"], "speech_not_configured");
    assert_eq!(render_queue(&app).await?, 0);
    app.close().await
}

/// The cache never answers one account with another's audio: a message's rendering is kept
/// for its conversation, a preview for the account that asked. Before, any signed-in account
/// could ask `POST /api/v1/speech` for a guessed script and learn from a fast `hit` that a
/// message in a conversation it cannot read said exactly that.
#[tokio::test]
async fn the_cache_tells_nobody_what_another_conversation_said() -> Result<(), BoxError> {
    let deepgram = FakeDeepgram::start().await;
    let app = TestApp::start_with_speech(&deepgram, |_| {}).await?;
    let p = people(&app).await;
    let ada = app.accounts.token(&p.ada);
    let bob = app.accounts.token(&p.bob);
    let scout = app.accounts.token(&p.scout);
    let secret = "[sighs] Your one-time code is 4821, do not share it.";
    let sent = app
        .post(
            "/api/v1/messages",
            &scout,
            json!({"recipient_id": "c:ada", "speech": {"script": secret, "voice": "hope"}}),
        )
        .await?;
    expect_status(&sent, StatusCode::ACCEPTED)?;
    assert_eq!(render_queue(&app).await?, 1);
    let guess = json!({"type": "speech.render", "data": {"script": secret, "voice": "hope"}});
    let (status, headers, _) = app.post_envelope("/api/v1/speech", &bob, &guess).await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers["x-dm-cache"], "miss",
        "Bob is not in that conversation, and finds nothing of it"
    );
    let (_, headers, _) = app.post_envelope("/api/v1/speech", &bob, &guess).await?;
    assert_eq!(headers["x-dm-cache"], "hit", "his own rendering is his");
    let (_, headers, _) = app.post_envelope("/api/v1/speech", &ada, &guess).await?;
    assert_eq!(
        headers["x-dm-cache"], "miss",
        "a preview is the caller's own, even for someone in the conversation"
    );
    // Listening to the message is the conversation's audio, rendered once.
    let (status, headers, _) = app
        .get_raw(
            &format!(
                "/api/v1/conversations/c:ada::si:scout/messages/{}/speech",
                code(&sent.1)
            ),
            &ada,
            &[],
        )
        .await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["x-dm-cache"], "hit");
    // A text message someone listened to is no oracle either.
    let text = app
        .post(
            "/api/v1/messages",
            &scout,
            json!({"recipient_id": "c:ada", "message": "The offer is 120000, keep it quiet."}),
        )
        .await?;
    let (status, _, _) = app
        .get_raw(
            &format!(
                "/api/v1/conversations/c:ada::si:scout/messages/{}/speech",
                code(&text.1)
            ),
            &ada,
            &[],
        )
        .await?;
    assert_eq!(status, StatusCode::OK);
    let voice = default_voice("Sc0");
    let (_, headers, _) = app
        .post_envelope(
            "/api/v1/speech",
            &bob,
            &json!({"type": "speech.render", "data": {"script": "The offer is 120000, keep it quiet.", "voice": voice}}),
        )
        .await?;
    assert_eq!(headers["x-dm-cache"], "miss");
    app.close().await
}

/// A spoken message being rendered elsewhere (by the worker of another process) is waited
/// for, not rendered a second time, and the listener is never charged for it: the sender
/// paid when it sent the message.
#[tokio::test]
async fn a_render_under_way_in_another_process_is_waited_for() -> Result<(), BoxError> {
    let deepgram = FakeDeepgram::start().await;
    let app = TestApp::start_with_speech(&deepgram, |_| {}).await?;
    let p = people(&app).await;
    let ada = app.accounts.token(&p.ada);
    let scout = app.accounts.token(&p.scout);
    let sent = app
        .post(
            "/api/v1/messages",
            &scout,
            json!({"recipient_id": "c:ada", "speech": {"script": "Rendered elsewhere, once."}}),
        )
        .await?;
    expect_status(&sent, StatusCode::ACCEPTED)?;
    // Another process's worker holds the rendering's lease.
    sqlx::query(
        "UPDATE speech_renders SET lease_id = gen_random_uuid(),
            lease_expires_at = clock_timestamp() + interval '1 minute'",
    )
    .execute(app.state.store.pool())
    .await?;
    let path = format!(
        "/api/v1/conversations/c:ada::si:scout/messages/{}/speech",
        code(&sent.1)
    );
    let listener = {
        let request = axum::http::Request::builder()
            .uri(&path)
            .header("authorization", format!("Bearer {ada}"))
            .body(axum::body::Body::empty())?;
        let router = app.router.clone();
        tokio::spawn(async move { tower::ServiceExt::oneshot(router, request).await })
    };
    tokio::time::sleep(std::time::Duration::from_millis(1_500)).await;
    assert!(
        deepgram.sessions().is_empty(),
        "the listener did not render it"
    );
    // That worker finishes: its lease ends and the rendering lands.
    sqlx::query("UPDATE speech_renders SET lease_id = NULL, lease_expires_at = NULL")
        .execute(app.state.store.pool())
        .await?;
    assert_eq!(render_queue(&app).await?, 1);
    let response = listener.await??;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-dm-cache"], "hit");
    assert_eq!(deepgram.sessions().len(), 1, "rendered once");
    let charged: i64 = sqlx::query_scalar(
        "SELECT COALESCE(sum(amount), 0)::bigint FROM speech_usage WHERE account_uuid = $1",
    )
    .bind(&p.ada.uuid)
    .fetch_one(app.state.store.pool())
    .await?;
    assert_eq!(charged, 0, "the listener pays nothing");
    app.close().await
}

/// A render the provider fails gives the day's characters back: the budget counts what was
/// rendered. (The attempt still counts towards the per-minute limit, which paces retries.)
#[tokio::test]
async fn renders_the_provider_fails_cost_no_characters() -> Result<(), BoxError> {
    let deepgram = FakeDeepgram::start().await;
    let app = TestApp::start_with_speech(&deepgram, |_| {}).await?;
    let p = people(&app).await;
    let ada = app.accounts.token(&p.ada);
    within_one_minute().await;
    deepgram.set_agent_mode(AgentMode::ProviderError);
    let script = "This one will not render.";
    let (status, _, body) = app
        .post_envelope(
            "/api/v1/speech",
            &ada,
            &json!({"type": "speech.render", "data": {"script": script, "voice": "mark"}}),
        )
        .await?;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "{}",
        data_of(&body)
    );
    let meters: Vec<(String, i64)> = sqlx::query_as(
        "SELECT meter, amount FROM speech_usage WHERE account_uuid = $1 ORDER BY meter",
    )
    .bind(&p.ada.uuid)
    .fetch_all(app.state.store.pool())
    .await?;
    assert_eq!(
        meters,
        [("characters".to_owned(), 0), ("renders".to_owned(), 1)],
        "{meters:?}"
    );
    deepgram.set_agent_mode(AgentMode::Speak);
    let (status, _, _) = app
        .post_envelope(
            "/api/v1/speech",
            &ada,
            &json!({"type": "speech.render", "data": {"script": script, "voice": "mark"}}),
        )
        .await?;
    assert_eq!(status, StatusCode::OK);
    let characters: i64 = sqlx::query_scalar(
        "SELECT amount FROM speech_usage WHERE account_uuid = $1 AND meter = 'characters'",
    )
    .bind(&p.ada.uuid)
    .fetch_one(app.state.store.pool())
    .await?;
    assert_eq!(characters, i64::try_from(script.chars().count())?);
    app.close().await
}

/// Audio is streamed from the cache a window at a time (a listener never holds a whole long
/// WAV in memory), byte for byte the rendering, whole or by range.
#[tokio::test]
async fn long_audio_streams_whole_and_by_range() -> Result<(), BoxError> {
    let deepgram = FakeDeepgram::start().await;
    let app = TestApp::start_with_speech(&deepgram, |_| {}).await?;
    let p = people(&app).await;
    let ada = app.accounts.token(&p.ada);
    let words: Vec<String> = (0..400).map(|index| format!("word{index}")).collect();
    let script = format!("{}.", words.join(" "));
    let (status, headers, whole) = app
        .post_envelope(
            "/api/v1/speech",
            &ada,
            &json!({"type": "speech.render", "data": {"script": script, "voice": "mark"}}),
        )
        .await?;
    assert_eq!(status, StatusCode::OK);
    assert!(
        whole.len() > 600 * 1024,
        "longer than a few windows: {}",
        whole.len()
    );
    assert_eq!(headers["content-length"], whole.len().to_string().as_str());
    assert_eq!(&whole[..4], b"RIFF");
    let (_, again_headers, again) = app
        .post_envelope(
            "/api/v1/speech",
            &ada,
            &json!({"type": "speech.render", "data": {"script": script, "voice": "mark"}}),
        )
        .await?;
    assert_eq!(again_headers["x-dm-cache"], "hit");
    assert_eq!(again, whole, "the cached bytes are the rendered bytes");
    // A message's audio by range, across a window boundary.
    let scout = app.accounts.token(&p.scout);
    let sent = app
        .post(
            "/api/v1/messages",
            &scout,
            json!({"recipient_id": "c:ada", "speech": {"script": script}}),
        )
        .await?;
    assert_eq!(render_queue(&app).await?, 1);
    let path = format!(
        "/api/v1/conversations/c:ada::si:scout/messages/{}/speech",
        code(&sent.1)
    );
    let (_, _, full) = app.get_raw(&path, &ada, &[]).await?;
    let (status, headers, part) = app
        .get_raw(&path, &ada, &[("range", "bytes=262100-262200")])
        .await?;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(headers["content-length"], "101");
    assert_eq!(&part[..], &full[262_100..=262_200]);
    app.close().await
}
