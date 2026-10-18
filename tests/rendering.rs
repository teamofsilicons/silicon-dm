//! Rendered messages: `attachments` stays a list of URLs, and `attachment_details`, `voice`,
//! `gif`, `speech` and `metadata` describe the content, in messages, history snapshots and
//! conversation summaries. Clients from before these fields still read every message.
#![allow(
    clippy::too_many_lines,
    reason = "each test is one scenario, read top to bottom"
)]
mod support;

use axum::http::{Method, StatusCode};
use serde_json::{Value, json};
use support::{
    BoxError,
    accounts::TestAccount,
    app::{TestApp, expect_status},
};

fn code(message: &Value) -> String {
    message["message-id"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

#[tokio::test]
async fn messages_render_details_voice_gif_and_metadata_without_breaking_older_clients()
-> Result<(), BoxError> {
    let app = TestApp::start().await?;
    let ada = TestAccount::carbon("a8K", "c:ada");
    let bob = TestAccount::carbon("Bb2", "c:bob");
    app.accounts.add_accounts(&[&ada, &bob]).await;
    app.accounts.live_introspection().await;
    let bob_token = app.accounts.token(&bob);
    let ada_token = app.accounts.token(&ada);
    let sent = app
        .post(
            "/api/v1/messages",
            &bob_token,
            json!({
                "recipient_id": "c:ada",
                "message": "the files",
                "attachments": [
                    {"permanent_url": "https://files.example/report.pdf", "name": "report.pdf",
                     "content_type": "application/pdf", "size": 52_100},
                    "https://files.example/plain-link"
                ],
                "voice": {"permanent_url": "https://files.example/note.webm", "content_type": "audio/webm",
                    "size": 9_600, "duration_milliseconds": 4_200, "name": "note.webm"},
                "voice_transcript": "listen to this",
                "gif": {"provider_id": "g42", "url": "https://media.giphy.com/g42.gif",
                    "preview_url": "https://media.giphy.com/g42-small.gif", "title": "Yay"},
                "metadata": {"thread": "release", "nested": {"type": "carbon", "id": "a8K"}},
            }),
        )
        .await?;
    expect_status(&sent, StatusCode::ACCEPTED)?;
    let message = &sent.1;
    assert_eq!(
        message["attachments"],
        json!([
            "https://files.example/report.pdf",
            "https://files.example/plain-link",
            "https://files.example/note.webm",
            "https://media.giphy.com/g42.gif"
        ]),
        "URLs stay as they were for older clients"
    );
    assert_eq!(
        message["attachment_details"],
        json!([
            {"kind": "attachment", "url": "https://files.example/report.pdf", "name": "report.pdf",
             "content_type": "application/pdf", "size": 52_100},
            {"kind": "attachment", "url": "https://files.example/plain-link"},
            {"kind": "voice", "url": "https://files.example/note.webm", "name": "note.webm",
             "content_type": "audio/webm", "size": 9_600},
            {"kind": "gif", "url": "https://media.giphy.com/g42.gif", "name": "Yay"}
        ]),
        "{message}"
    );
    assert_eq!(
        message["voice"],
        json!({"url": "https://files.example/note.webm",
            "permanent_url": "https://files.example/note.webm", "name": "note.webm",
            "content_type": "audio/webm", "size": 9_600, "duration_milliseconds": 4_200})
    );
    assert_eq!(
        message["gif"],
        json!({"provider_id": "g42", "url": "https://media.giphy.com/g42.gif",
            "preview_url": "https://media.giphy.com/g42-small.gif", "title": "Yay"})
    );
    assert_eq!(message["speech"], Value::Null);
    assert_eq!(
        message["metadata"],
        json!({"thread": "release", "nested": {"type": "carbon", "id": "a8K"}}),
        "metadata is preserved exactly and never rewritten"
    );
    assert_eq!(message["voice_transcript"], "listen to this");

    // A client from before these fields (0.16) still reads the message.
    let older: released::Message = serde_json::from_value(message.clone())?;
    assert_eq!(older.content.text.as_deref(), Some("the files"));
    assert_eq!(older.content.attachments.len(), 4);
    assert_eq!(
        older
            .content
            .voice
            .as_ref()
            .and_then(|voice| voice.duration_milliseconds),
        Some(4_200)
    );
    assert_eq!(
        older
            .content
            .voice
            .as_ref()
            .map(|voice| voice.attachment.permanent_url.as_str()),
        Some("https://files.example/note.webm")
    );
    assert_eq!(
        older
            .content
            .gif
            .as_ref()
            .map(|gif| gif.provider_id.as_str()),
        Some("g42")
    );
    assert_eq!(older.content.metadata["thread"], "release");

    // The conversation list and the message read render the same fields.
    let listed = app.get("/api/v1/conversations", &ada_token).await?;
    let last = &listed.1["items"][0]["last_message"];
    assert_eq!(last["attachment_details"], message["attachment_details"]);
    assert_eq!(last["metadata"], message["metadata"]);
    let path = format!(
        "/api/v1/conversations/c:ada::c:bob/messages/{}",
        code(message)
    );
    // An edit keeps the old content (with its details) in history.
    let edited = app
        .call(
            Method::PATCH,
            &path,
            Some(&bob_token),
            Some(
                json!({"message": "just text", "metadata": {"thread": "release", "edited": true}}),
            ),
            &[],
        )
        .await?;
    expect_status(&edited, StatusCode::OK)?;
    assert_eq!(edited.1["attachments"], json!([]));
    assert_eq!(edited.1["attachment_details"], json!([]));
    assert_eq!(edited.1["voice"], Value::Null);
    assert_eq!(edited.1["gif"], Value::Null);
    assert_eq!(edited.1["metadata"]["edited"], true);
    let snapshot = &edited.1["history"][0];
    assert_eq!(snapshot["message"], "the files");
    assert_eq!(
        snapshot["attachment_details"],
        message["attachment_details"]
    );
    assert_eq!(snapshot["voice"]["duration_milliseconds"], 4_200);
    assert_eq!(snapshot["gif"]["title"], "Yay");
    assert_eq!(snapshot["metadata"]["thread"], "release");
    // A reply quotes the words and links only.
    let reply = app
        .post(
            "/api/v1/conversations/c:ada::c:bob/messages",
            &ada_token,
            json!({"message": "got it", "reply": {"message-id": code(message)}}),
        )
        .await?;
    expect_status(&reply, StatusCode::ACCEPTED)?;
    assert_eq!(
        reply.1["reply"]["content"],
        json!({"message": "just text", "attachments": [], "voice_transcript": null})
    );
    // Tombstones hide everything.
    let deleted = app
        .call(Method::DELETE, &path, Some(&bob_token), None, &[])
        .await?;
    expect_status(&deleted, StatusCode::OK)?;
    for field in ["voice", "gif", "speech", "message", "voice_transcript"] {
        assert_eq!(deleted.1[field], Value::Null, "{field}");
    }
    assert_eq!(deleted.1["attachments"], json!([]));
    assert_eq!(deleted.1["attachment_details"], json!([]));
    assert_eq!(deleted.1["metadata"], json!({}));
    app.close().await
}

/// The message model of `silicon-dm-client` 0.16.1 (the released client), reduced to what
/// decoding a message needs, so this test keeps checking compatibility with clients already
/// installed whatever the workspace client becomes.
mod released {
    use serde::Deserialize;
    use serde_json::{Map, Value};

    #[derive(Debug)]
    pub struct Attachment {
        pub permanent_url: url::Url,
    }

    impl<'de> Deserialize<'de> for Attachment {
        fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            #[derive(Deserialize)]
            struct Details {
                permanent_url: url::Url,
                #[serde(default)]
                #[allow(dead_code, reason = "decoded like the released client does")]
                name: Option<String>,
                #[serde(default)]
                #[allow(dead_code, reason = "decoded like the released client does")]
                content_type: Option<String>,
                #[serde(default)]
                #[allow(dead_code, reason = "decoded like the released client does")]
                size: Option<u64>,
            }
            #[derive(Deserialize)]
            #[serde(untagged)]
            enum Input {
                Link(url::Url),
                Details(Details),
            }
            Ok(match Input::deserialize(deserializer)? {
                Input::Link(permanent_url) => Self { permanent_url },
                Input::Details(details) => Self {
                    permanent_url: details.permanent_url,
                },
            })
        }
    }

    #[derive(Debug, Deserialize)]
    pub struct VoiceAttachment {
        #[serde(flatten)]
        pub attachment: Attachment,
        pub duration_milliseconds: Option<u64>,
    }

    #[derive(Debug, Deserialize)]
    pub struct Gif {
        pub provider_id: String,
        #[allow(dead_code, reason = "decoded like the released client does")]
        pub url: url::Url,
    }

    #[derive(Debug, Deserialize)]
    pub struct Content {
        #[serde(default, rename = "message", alias = "text")]
        pub text: Option<String>,
        #[serde(default)]
        pub attachments: Vec<Attachment>,
        #[serde(default)]
        pub voice: Option<VoiceAttachment>,
        #[serde(default)]
        #[allow(dead_code, reason = "decoded like the released client does")]
        pub voice_transcript: Option<String>,
        #[serde(default)]
        pub gif: Option<Gif>,
        #[serde(default)]
        pub metadata: Map<String, Value>,
    }

    #[derive(Debug, Deserialize)]
    pub struct Message {
        #[serde(rename = "message-id")]
        #[allow(dead_code, reason = "decoded like the released client does")]
        pub id: String,
        #[allow(dead_code, reason = "decoded like the released client does")]
        pub conversation_id: String,
        #[serde(flatten)]
        pub content: Content,
        #[allow(dead_code, reason = "decoded like the released client does")]
        pub created_at: String,
    }
}
