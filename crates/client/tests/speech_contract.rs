//! Voices, speech, transcription and bulk receipts against a local fixture, and the message,
//! conversation and discovery shapes of services before and after DM 0.17.
use std::sync::{Arc, Mutex};

use axum::{
    Json, Router,
    body::to_bytes,
    extract::{Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::any,
};
use serde_json::{Value, json};
use silicon_dm_client::{
    Client, Conversation, Me, Message, MessageCreate, ReceiptStatus, SignInDiscovery, SpeechInput,
    relay::Operation,
};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Clone)]
struct Seen {
    method: String,
    path: String,
    query: Option<String>,
    headers: axum::http::HeaderMap,
    raw: Vec<u8>,
}
type Requests = Arc<Mutex<Vec<Seen>>>;

fn wav(milliseconds: usize) -> Vec<u8> {
    let samples = 24 * milliseconds;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + samples as u32 * 2).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&24_000_u32.to_le_bytes());
    bytes.extend_from_slice(&48_000_u32.to_le_bytes());
    bytes.extend_from_slice(&2_u16.to_le_bytes());
    bytes.extend_from_slice(&16_u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&(samples as u32 * 2).to_le_bytes());
    bytes.resize(44 + samples * 2, 0);
    bytes
}

fn envelope(kind: &str, data: Value) -> Response {
    Json(json!({"type": kind, "data": data})).into_response()
}

async fn handler(State(requests): State<Requests>, request: Request) -> Response {
    let method = request.method().to_string();
    let path = request.uri().path().to_owned();
    let query = request.uri().query().map(str::to_owned);
    let headers = request.headers().clone();
    let raw = to_bytes(request.into_body(), 1024 * 1024)
        .await
        .unwrap_or_default()
        .to_vec();
    if let Ok(mut seen) = requests.lock() {
        seen.push(Seen {
            method: method.clone(),
            path: path.clone(),
            query,
            headers,
            raw,
        });
    }
    let voice = json!({"account":{"type":"silicon","id":"si:scout","uuid":"5c0"},
        "voice":{"key":"hope","name":"Hope","description":"Clear and cheerful."},
        "chosen":true,"chosen_by":{"type":"carbon","id":"c:ada","uuid":"Ada"},
        "updated_at":"2026-10-17T10:00:00Z","can_change":true});
    match (method.as_str(), path.as_str()) {
        ("GET", "/api/v1/voices") => envelope(
            "voices",
            json!({"model":"eleven_v4","enabled":true,"items":[
                {"key":"mark","name":"Mark","description":"Relaxed.","voice_id":"UgBBYS2sOqTuMpoF3BR0"},
                {"key":"hope","name":"Hope","description":"Clear and cheerful.","voice_id":"tnSpp4vdxKPjI9w0GnoV"}],
                "default_rule":"first 8 bytes of SHA-256(uuid) mod the number of voices"}),
        ),
        (_, "/api/v1/voices/si:scout") => envelope("voice", voice),
        ("POST", "/api/v1/speech") => (
            [
                ("content-type", "audio/wav"),
                ("x-dm-voice", "hope"),
                ("x-dm-duration-ms", "1500"),
                ("x-dm-cache", "miss"),
                ("etag", "\"abc\""),
            ],
            wav(1500),
        )
            .into_response(),
        ("GET", "/api/v1/conversations/c:ada::si:scout/messages/00a/speech") => {
            // No duration header: the client reads it from the WAV.
            ([("content-type", "audio/wav")], wav(250)).into_response()
        }
        ("GET", "/api/v1/conversations/c:ada::si:scout/messages/00b/speech") => {
            envelope("message", json!({"surprise":"json"}))
        }
        ("POST", "/api/v1/speech/transcriptions") => envelope(
            "transcription",
            json!({"transcript":"see you at noon","language":"en","duration_milliseconds":1200,"confidence":0.97}),
        ),
        ("POST", "/api/v1/conversations/c:ada/receipts") => envelope(
            "receipts",
            json!({"updated":4,"unread_count":1,"last_read_message_id":"01f"}),
        ),
        _ => (
            StatusCode::NOT_FOUND,
            Json(json!({"type":"error","data":{"error":{"code":"not_found","message":"fixture"}}})),
        )
            .into_response(),
    }
}

async fn fixture() -> Result<(String, Requests, tokio::task::JoinHandle<()>)> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let requests = Requests::default();
    let router = Router::new()
        .route("/{*path}", any(handler))
        .with_state(requests.clone());
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    Ok((base, requests, task))
}

#[tokio::test]
async fn speech_routes_send_explicit_types_raw_audio_and_read_audio_headers() -> Result {
    let (base, requests, task) = fixture().await?;
    let client = Client::new(&base)?.with_token("access-jwt");

    let catalog = client.voices().await?;
    assert_eq!(catalog.model, "eleven_v4");
    assert_eq!(catalog.enabled, Some(true));
    assert_eq!(catalog.keys(), vec!["mark", "hope"]);
    assert_eq!(
        catalog.get("Hope").map(|voice| voice.name.as_str()),
        Some("Hope")
    );
    let chosen = client.set_voice("si:scout", "hope", "voice-key-1").await?;
    assert!(chosen.chosen && chosen.can_change);
    assert_eq!(chosen.voice.key, "hope");
    assert_eq!(chosen.chosen_by.map(|by| by.id).as_deref(), Some("c:ada"));
    client.voice("si:scout").await?;
    client.reset_voice("si:scout").await?;

    let rendered = client
        .render_speech("[warmly] Hello there.", Some("hope"))
        .await?;
    assert_eq!(
        (rendered.voice.as_deref(), rendered.cache.as_deref()),
        (Some("hope"), Some("miss"))
    );
    assert_eq!(rendered.duration_milliseconds, Some(1500));
    assert_eq!(rendered.etag.as_deref(), Some("\"abc\""));
    assert_eq!(&rendered.wav[..4], b"RIFF");
    let listened = client
        .message_speech("c:ada::si:scout", "00a", Some("si:scout"))
        .await?;
    assert_eq!(
        listened.duration_milliseconds,
        Some(250),
        "from the WAV header"
    );
    let unexpected = client
        .message_speech("c:ada::si:scout", "00b", None)
        .await
        .err()
        .ok_or("JSON is not audio")?;
    assert_eq!(unexpected.code(), "unexpected_response");

    let heard = client
        .transcribe(b"OggS raw audio".to_vec(), "audio/ogg", Some("en"))
        .await?;
    assert_eq!(heard.transcript, "see you at noon");
    assert_eq!(heard.confidence, Some(0.97));
    let read = client.read_up_to("c:ada", "01f").await?;
    assert_eq!(
        (
            read.updated,
            read.unread_count,
            read.last_read_message_id.as_deref()
        ),
        (4, Some(1), Some("01f"))
    );
    client
        .receipts_up_to("c:ada", "01f", ReceiptStatus::Delivered)
        .await?;
    assert!(client.read_up_to("c:ada", "not a code!").await.is_err());

    let seen = requests.lock().map(|r| r.clone()).unwrap_or_default();
    assert!(
        seen[0].headers.get("authorization").is_none(),
        "the voice list needs no credential"
    );
    assert_eq!(
        (seen[1].method.as_str(), seen[1].path.as_str()),
        ("PUT", "/api/v1/voices/si:scout")
    );
    assert_eq!(seen[1].headers["idempotency-key"], "voice-key-1");
    let body = |index: usize| serde_json::from_slice::<Value>(&seen[index].raw).unwrap_or_default();
    assert_eq!(body(1), json!({"type":"voice.set","data":{"voice":"hope"}}));
    assert_eq!(seen[3].method, "DELETE");
    assert_eq!(
        body(4),
        json!({"type":"speech.render","data":{"script":"[warmly] Hello there.","voice":"hope"}})
    );
    assert_eq!(seen[5].query.as_deref(), Some("for=si%3Ascout"));
    let transcription = &seen[7];
    assert_eq!(transcription.raw, b"OggS raw audio", "audio is sent as is");
    assert_eq!(transcription.headers["content-type"], "audio/ogg");
    assert_eq!(transcription.query.as_deref(), Some("language=en"));
    assert_eq!(
        body(8),
        json!({"type":"receipts.read","data":{"up_to":"01f"}})
    );
    assert_eq!(body(9)["type"], "receipts.delivered");
    for request in &seen[1..] {
        assert_eq!(request.headers["x-dm-contract-version"], "4");
    }
    task.abort();
    Ok(())
}

fn message_today() -> Value {
    json!({"message-id":"00a","conversation_id":"c:ada::si:scout","recipient_id":"c:ada",
        "sender":{"type":"silicon","id":"si:scout","uuid":"5c0"},"message":"Build 1843",
        "attachments":["https://files.example/note.ogg","https://media.giphy.com/x.gif"],
        "voice_transcript":null,"reply":null,"bundle":null,"history":[],"created_at":"2026-10-17T10:00:00Z",
        "updated_at":null,"deleted_at":null,"delivered_at":null,"read_at":null})
}

#[test]
fn messages_from_services_before_and_after_speech_read_and_render() -> Result {
    // A message as DM 0.16 renders it: no speech, voice, GIF, details or metadata.
    let old: Message = serde_json::from_value(message_today())?;
    assert!(old.speech.is_none() && old.voice.is_none() && old.gif.is_none());
    assert!(old.attachment_details.is_empty() && old.content.speech.is_none());
    let rendered = serde_json::to_value(&old)?;
    assert_eq!(rendered["attachments"].as_array().map(Vec::len), Some(2));
    assert_eq!(
        (&rendered["speech"], &rendered["voice"], &rendered["gif"]),
        (&Value::Null, &Value::Null, &Value::Null)
    );
    assert_eq!(rendered["metadata"], json!({}));

    // The same message from DM 0.17.
    let mut new = message_today();
    new["voice_transcript"] = json!("The build is green.");
    new["speech"] = json!({"voice":"mark","voice_name":"Mark","script":"[warmly] The build is green.",
        "model":"eleven_v4","status":"ready","duration_milliseconds":1840,"peaks":[3,40,100]});
    new["voice"] = json!({"url":"https://files.example/note.ogg","content_type":"audio/ogg","size":4096,"duration_milliseconds":2100});
    new["gif"] = json!({"provider_id":"x","url":"https://media.giphy.com/x.gif","title":"party"});
    new["attachment_details"] = json!([{"url":"https://files.example/note.ogg","name":"note.ogg","content_type":"audio/ogg","size":4096},
        {"url":"https://media.giphy.com/x.gif"}]);
    new["metadata"] = json!({"isi":"planner"});
    let spoken: Message = serde_json::from_value(new.clone())?;
    let speech = spoken.speech.as_ref().ok_or("speech")?;
    assert_eq!(
        (speech.voice.as_str(), speech.status.as_str()),
        ("mark", "ready")
    );
    assert_eq!(speech.peaks.as_deref(), Some(&[3, 40, 100][..]));
    assert_eq!(
        spoken.content.speech,
        Some(SpeechInput {
            script: "[warmly] The build is green.".into(),
            voice: Some("mark".into()),
        }),
        "re-sending the content keeps it spoken"
    );
    assert_eq!(
        spoken.voice.as_ref().and_then(|v| v.duration_milliseconds),
        Some(2100)
    );
    assert_eq!(
        spoken.attachment_details[0].name.as_deref(),
        Some("note.ogg")
    );
    assert_eq!(spoken.content.metadata["isi"], "planner");
    let rendered = serde_json::to_value(&spoken)?;
    for key in [
        "speech",
        "voice",
        "gif",
        "attachment_details",
        "metadata",
        "attachments",
    ] {
        assert_eq!(rendered[key], new[key], "{key}");
    }

    // A tombstone hides the speech like the rest of its content.
    let mut deleted = new.clone();
    deleted["deleted_at"] = json!("2026-10-17T11:00:00Z");
    let deleted = serde_json::to_value(serde_json::from_value::<Message>(deleted)?)?;
    assert_eq!(
        (&deleted["speech"], &deleted["voice"], &deleted["message"]),
        (&Value::Null, &Value::Null, &Value::Null)
    );
    assert_eq!(deleted["attachment_details"], json!([]));

    // A shape this release does not know leaves the field empty instead of failing.
    let mut odd = message_today();
    odd["speech"] = json!("a future shape");
    odd["voice"] = json!({"permanent_url":"https://files.example/note.ogg"});
    let odd: Message = serde_json::from_value(odd)?;
    assert!(odd.speech.is_none() && odd.voice.is_none());
    Ok(())
}

#[test]
fn spoken_content_serializes_only_when_present_and_picks_its_relay_operation() -> Result {
    let text = MessageCreate {
        text: Some("hello".into()),
        ..Default::default()
    };
    let wire = serde_json::to_value(&text)?;
    assert!(
        wire.get("speech").is_none(),
        "text messages keep their shape"
    );
    let spoken = MessageCreate {
        text: Some("Build 1843".into()),
        speech: Some(SpeechInput {
            script: "[excited] It shipped!".into(),
            voice: None,
        }),
        ..Default::default()
    };
    assert_eq!(
        serde_json::to_value(&spoken)?["speech"],
        json!({"script":"[excited] It shipped!"})
    );
    let parsed: MessageCreate =
        serde_json::from_value(json!({"speech":{"script":"Hi.","voice":"hope"}}))?;
    assert_eq!(parsed.speech.and_then(|s| s.voice).as_deref(), Some("hope"));

    let operation = |op: Operation| serde_json::to_value(op).map(|v| v["operation"].clone());
    assert_eq!(
        operation(Operation::send_message("c:ada", spoken.clone(), "k1"))?,
        "send_spoken_message"
    );
    assert_eq!(
        operation(Operation::send_message("c:ada", text.clone(), "k2"))?,
        "send_message"
    );
    assert_eq!(
        operation(Operation::edit_message("c:ada", "00a", spoken, "k3"))?,
        "edit_spoken_message"
    );
    assert_eq!(
        operation(Operation::edit_message("c:ada", "00a", text, "k4"))?,
        "edit_message"
    );
    // A relay request queued by this release survives a restart unchanged.
    let queued = json!({"operation":"receipts_up_to","conversation_id":"c:ada","up_to":"01f","status":"read"});
    let parsed: Operation = serde_json::from_value(queued.clone())?;
    assert!(parsed.is_mutation());
    assert_eq!(serde_json::to_value(parsed)?, queued);
    Ok(())
}

#[test]
fn summaries_and_discovery_read_with_or_without_the_new_fields() -> Result {
    let summary = json!({"id":"c:ada::si:scout","participants":[],"last_message":null,
        "created_at":"2026-10-17T10:00:00Z","updated_at":"2026-10-17T10:00:00Z"});
    let old: Conversation = serde_json::from_value(summary.clone())?;
    assert_eq!(
        (old.unread_count, old.last_read_message_id.as_deref()),
        (None, None)
    );
    assert!(serde_json::to_value(&old)?.get("unread_count").is_none());
    let mut new = summary;
    new["unread_count"] = json!(3);
    new["last_read_message_id"] = json!("01c");
    let new: Conversation = serde_json::from_value(new)?;
    assert_eq!(new.unread_count, Some(3));
    assert_eq!(serde_json::to_value(&new)?["last_read_message_id"], "01c");

    let discovery = json!({"app_id":"dm","accounts_url":"https://accounts.teamofsilicons.com",
        "api_base_url":"https://api.dm.teamofsilicons.com/api/v1","contract_version":4});
    let before: SignInDiscovery = serde_json::from_value(discovery.clone())?;
    assert!(before.speech.is_none());
    let mut with_speech = discovery;
    with_speech["speech"] = json!({"enabled":false,"model":"eleven_v4","stt_model":"nova-3","voices_path":"/api/v1/voices"});
    let after: SignInDiscovery = serde_json::from_value(with_speech)?;
    let speech = after.speech.ok_or("speech")?;
    assert!(!speech.enabled);
    assert_eq!(speech.stt_model.as_deref(), Some("nova-3"));
    let me: Me = serde_json::from_value(
        json!({"account":{"type":"carbon","id":"c:ada","uuid":"Ada"},
        "speech":{"enabled":true,"model":"eleven_v4"}}),
    )?;
    assert!(me.speech.is_some_and(|speech| speech.enabled));
    Ok(())
}
