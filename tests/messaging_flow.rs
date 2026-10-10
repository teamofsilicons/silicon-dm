//! The messaging contract over HTTP with Silicon Accounts identities: idempotent sends,
//! drafts cleared by the matching send, monotonic receipts, ISI routing that cannot be
//! forged or rerouted, Silicon bundles, edit history and tombstones, and the sync feed
//! (with its long-poll) that replaces Ting while Ting is not configured.
#![allow(
    clippy::too_many_lines,
    reason = "each test is one scenario, read top to bottom"
)]
mod support;

use std::time::{Duration, Instant};

use axum::http::{Method, StatusCode};
use serde_json::{Value, json};
use support::{
    BoxError,
    accounts::TestAccount,
    app::{TestApp, expect_status},
};

struct People {
    ada: TestAccount,
    bob: TestAccount,
    writer: TestAccount,
    cos: TestAccount,
}

async fn people(app: &TestApp) -> People {
    let ada = TestAccount::carbon("a8K", "c:ada");
    let bob = TestAccount::carbon("Bb2", "c:bob");
    let writer = TestAccount::silicon("Wr1", "si:writer", &ada);
    let cos = TestAccount::silicon("K1E", "si:cos", &ada);
    app.accounts
        .add_accounts(&[&ada, &bob, &writer, &cos])
        .await;
    app.accounts.live_introspection().await;
    People {
        ada,
        bob,
        writer,
        cos,
    }
}

fn code(message: &Value) -> String {
    message["message-id"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

#[tokio::test]
async fn sends_are_idempotent_and_clear_the_matching_draft() -> Result<(), BoxError> {
    let app = TestApp::start().await?;
    let p = people(&app).await;
    let bob = app.accounts.token(&p.bob);
    let key = vec![("idempotency-key", "voice-message-1".to_owned())];
    let voice = json!({"permanent_url": "https://media.example/voice/1.ogg", "name": "1.ogg",
        "content_type": "audio/ogg", "size": 8192, "duration_milliseconds": 42000});
    // A conversation and a voice draft in it.
    let first = app
        .post(
            "/api/v1/messages",
            &bob,
            json!({"recipient_id": "c:ada", "message": "hi"}),
        )
        .await?;
    let conversation = first.1["conversation_id"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let draft = app
        .call(
            Method::PUT,
            &format!("/api/v1/conversations/{conversation}/draft"),
            Some(&bob),
            Some(json!({"voice": voice, "voice_transcript": "client transcript", "metadata": {}})),
            &[],
        )
        .await?;
    expect_status(&draft, StatusCode::OK)?;
    assert_eq!(draft.1["voice"]["duration_milliseconds"], 42000);
    // Sending the same content clears it; a retry with the key replays the same message.
    let body = json!({"voice": voice, "voice_transcript": "client transcript"});
    let sent = app
        .call(
            Method::POST,
            &format!("/api/v1/conversations/{conversation}/messages"),
            Some(&bob),
            Some(body.clone()),
            &key,
        )
        .await?;
    expect_status(&sent, StatusCode::ACCEPTED)?;
    assert_eq!(sent.1["voice_transcript"], "client transcript");
    let replay = app
        .call(
            Method::POST,
            &format!("/api/v1/conversations/{conversation}/messages"),
            Some(&bob),
            Some(body),
            &key,
        )
        .await?;
    assert_eq!(code(&replay.1), code(&sent.1));
    let conflict = app
        .call(
            Method::POST,
            &format!("/api/v1/conversations/{conversation}/messages"),
            Some(&bob),
            Some(json!({"message": "different"})),
            &key,
        )
        .await?;
    assert_eq!(conflict.0, StatusCode::CONFLICT);
    let cleared = app
        .get(&format!("/api/v1/conversations/{conversation}/draft"), &bob)
        .await?;
    assert_eq!(cleared.0, StatusCode::NOT_FOUND);
    app.close().await
}

#[tokio::test]
async fn receipts_are_monotonic_and_drive_the_aggregate_status() -> Result<(), BoxError> {
    let app = TestApp::start().await?;
    let p = people(&app).await;
    let sent = app
        .post(
            "/api/v1/messages",
            &app.accounts.token(&p.bob),
            json!({"recipient_id": "c:ada", "message": "ping"}),
        )
        .await?;
    let path = format!(
        "/api/v1/conversations/{}/messages/{}/receipts",
        sent.1["conversation_id"].as_str().unwrap_or_default(),
        code(&sent.1)
    );
    let ada = app.accounts.token(&p.ada);
    let receipt = |status: &str| json!({"status": status, "device_id": "ada-phone"});
    let delivered = app.post(&path, &ada, receipt("delivered")).await?;
    expect_status(&delivered, StatusCode::OK)?;
    assert!(
        delivered.1["delivered_at"].is_string() && delivered.1["read_at"].is_null(),
        "{}",
        delivered.1
    );
    let read = app.post(&path, &ada, receipt("read")).await?;
    let read_at = read.1["read_at"].clone();
    assert!(read_at.is_string(), "{}", read.1);
    let downgrade = app.post(&path, &ada, receipt("delivered")).await?;
    assert_eq!(
        downgrade.1["read_at"], read_at,
        "a receipt never moves backwards"
    );
    // The conversation list carries the aggregate status of the last message.
    let listed = app
        .get("/api/v1/conversations", &app.accounts.token(&p.bob))
        .await?;
    assert_eq!(
        listed.1["items"][0]["last_message_status"], "read",
        "{}",
        listed.1
    );
    // The sender cannot receipt its own message.
    let own = app
        .post(&path, &app.accounts.token(&p.bob), receipt("read"))
        .await?;
    assert_eq!(own.0, StatusCode::UNPROCESSABLE_ENTITY);
    app.close().await
}

#[tokio::test]
async fn isi_routing_round_trips_and_cannot_be_forged_or_rerouted() -> Result<(), BoxError> {
    let app = TestApp::start().await?;
    let p = people(&app).await;
    let writer = app.accounts.token(&p.writer);
    // si:writer and si:cos share a custodian, so they may talk.
    let body = json!({"recipient_id": "deliberate@si:cos", "sender_id": "compose@si:writer",
        "message": "ISI round trip"});
    let key = vec![("idempotency-key", "isi-message-key".to_owned())];
    let created = app
        .post(
            "/api/v1/conversations",
            &writer,
            json!({"participant_ids": ["si:cos"]}),
        )
        .await?;
    expect_status(&created, StatusCode::CREATED)?;
    let conversation = created.1["id"].as_str().unwrap_or_default().to_owned();
    let path = format!("/api/v1/conversations/{conversation}/messages");
    let sent = app
        .call(Method::POST, &path, Some(&writer), Some(body.clone()), &key)
        .await?;
    expect_status(&sent, StatusCode::ACCEPTED)?;
    assert_eq!(sent.1["sender"]["id"], "si:writer");
    assert_eq!(sent.1["recipient_id"], "deliberate@si:cos");
    let stored: (Option<String>, Option<String>) = sqlx::query_as("SELECT sender_address, recipient_address FROM messages WHERE text_content = 'ISI round trip'")
        .fetch_one(app.state.store.pool())
        .await?;
    assert_eq!(
        stored,
        (
            Some("compose@Wr1".to_owned()),
            Some("deliberate@K1E".to_owned())
        ),
        "stored by uuid"
    );
    let replay = app
        .call(Method::POST, &path, Some(&writer), Some(body.clone()), &key)
        .await?;
    assert_eq!(code(&replay.1), code(&sent.1));
    let mut rerouted = body.clone();
    rerouted["recipient_id"] = json!("review@si:cos");
    let conflict = app
        .call(Method::POST, &path, Some(&writer), Some(rerouted), &key)
        .await?;
    assert_eq!(conflict.0, StatusCode::CONFLICT);
    let mut forged = body.clone();
    forged["sender_id"] = json!("compose@si:cos");
    let refused = app
        .call(Method::POST, &path, Some(&writer), Some(forged), &[])
        .await?;
    assert_eq!(refused.1["error"]["code"], "sender_mismatch");
    let mut carbon_isi = body.clone();
    carbon_isi["recipient_id"] = json!("deliberate@c:ada");
    let refused = app
        .call(Method::POST, &path, Some(&writer), Some(carbon_isi), &[])
        .await?;
    assert_eq!(refused.0, StatusCode::UNPROCESSABLE_ENTITY);
    // Edits keep routing and may not change it.
    let message = format!("{path}/{}", code(&sent.1));
    let edited = app
        .call(
            Method::PATCH,
            &message,
            Some(&writer),
            Some(json!({"message": "edited"})),
            &[],
        )
        .await?;
    expect_status(&edited, StatusCode::OK)?;
    assert_eq!(edited.1["recipient_id"], "deliberate@si:cos");
    assert_eq!(edited.1["history"].as_array().map(Vec::len), Some(1));
    let reroute = app
        .call(
            Method::PATCH,
            &message,
            Some(&writer),
            Some(json!({"message": "x", "recipient_id": "other@si:cos"})),
            &[],
        )
        .await?;
    assert_eq!(reroute.0, StatusCode::UNPROCESSABLE_ENTITY);
    let same_route = app
        .call(
            Method::PATCH,
            &message,
            Some(&writer),
            Some(json!({"message": "again", "recipient_id": "deliberate@si:cos"})),
            &[],
        )
        .await?;
    expect_status(&same_route, StatusCode::OK)?;
    // Tombstones carry no content and add no history.
    let deleted = app
        .call(Method::DELETE, &message, Some(&writer), None, &[])
        .await?;
    expect_status(&deleted, StatusCode::OK)?;
    assert!(deleted.1["deleted_at"].is_string());
    let after = app
        .call(
            Method::PATCH,
            &message,
            Some(&writer),
            Some(json!({"message": "resurrect"})),
            &[],
        )
        .await?;
    assert_eq!(after.0, StatusCode::CONFLICT);
    app.close().await
}

#[tokio::test]
async fn bundles_are_for_silicons_and_keep_their_originals() -> Result<(), BoxError> {
    let app = TestApp::start().await?;
    let p = people(&app).await;
    let cos = app.accounts.token(&p.cos);
    let mut codes = Vec::new();
    for text in ["part one", "part two"] {
        let sent = app
            .post(
                "/api/v1/messages",
                &cos,
                json!({"recipient_id": "c:ada", "message": text}),
            )
            .await?;
        expect_status(&sent, StatusCode::ACCEPTED)?;
        codes.push(code(&sent.1));
    }
    let conversation = "c:ada::si:cos";
    let bundle = app
        .post(
            &format!("/api/v1/conversations/{conversation}/bundles"),
            &cos,
            json!({"message_ids": codes, "display_message": {"message": "parts one and two"}}),
        )
        .await?;
    expect_status(&bundle, StatusCode::CREATED)?;
    assert_eq!(bundle.1["created_by"]["id"], "si:cos");
    let listed = app
        .get(
            &format!("/api/v1/conversations/{conversation}/messages"),
            &app.accounts.token(&p.ada),
        )
        .await?;
    let texts: Vec<&str> = listed.1["items"]
        .as_array()
        .map(|items| items.iter().filter_map(|m| m["message"].as_str()).collect())
        .unwrap_or_default();
    assert!(
        texts.contains(&"parts one and two") && !texts.contains(&"part one"),
        "{texts:?}"
    );
    let expanded = app
        .get(
            &format!(
                "/api/v1/conversations/{conversation}/bundles/{}",
                bundle.1["id"].as_str().unwrap_or_default()
            ),
            &app.accounts.token(&p.ada),
        )
        .await?;
    expect_status(&expanded, StatusCode::OK)?;
    assert_eq!(
        expanded.1["original_messages"].as_array().map(Vec::len),
        Some(2)
    );
    let carbon = app
        .post(
            &format!("/api/v1/conversations/{conversation}/bundles"),
            &app.accounts.token(&p.ada),
            json!({"message_ids": ["000"], "display_message": {"message": "x"}}),
        )
        .await?;
    assert_eq!(carbon.1["error"]["code"], "silicon_only");
    app.close().await
}

async fn sync(app: &TestApp, token: &str, query: &str) -> Result<Value, BoxError> {
    let page = app.get(&format!("/api/v1/sync?{query}"), token).await?;
    expect_status(&page, StatusCode::OK)?;
    Ok(page.1)
}

#[tokio::test]
async fn the_sync_feed_references_current_events_and_long_polls() -> Result<(), BoxError> {
    let app = TestApp::start().await?;
    let p = people(&app).await;
    let cancellation = tokio_util::sync::CancellationToken::new();
    let listener = tokio::spawn(
        app.state
            .sync
            .clone()
            .run(app.state.store.clone(), cancellation.clone()),
    );
    let ada = app.accounts.token(&p.ada);
    let bob = app.accounts.token(&p.bob);
    let start = sync(&app, &bob, "reset=true").await?;
    let cursor = start["cursor"].as_str().unwrap_or_default().to_owned();
    assert_eq!(start["events"], json!([]));

    let sent = app
        .post(
            "/api/v1/messages",
            &ada,
            json!({"recipient_id": "c:bob", "message": "sync me"}),
        )
        .await?;
    expect_status(&sent, StatusCode::ACCEPTED)?;
    let page = sync(&app, &bob, &format!("cursor={cursor}")).await?;
    assert_eq!(page["events"][0]["conversation_id"], "c:ada::c:bob");
    assert_eq!(page["events"][0]["message_id"], code(&sent.1));
    assert_eq!(page["events"][0]["type"], "message");
    let cursor = page["cursor"].as_str().unwrap_or_default().to_owned();

    // Nothing new: a long-poll waits, and returns as soon as something arrives.
    let waiting = {
        let router = app.router.clone();
        let bob = bob.clone();
        let cursor = cursor.clone();
        tokio::spawn(async move {
            let started = Instant::now();
            let request = axum::http::Request::builder()
                .uri(format!("/api/v1/sync?cursor={cursor}&wait=10"))
                .header("authorization", format!("Bearer {bob}"))
                .body(axum::body::Body::empty())?;
            let response = tower::ServiceExt::oneshot(router, request).await?;
            let body = axum::body::to_bytes(response.into_body(), usize::MAX).await?;
            Ok::<_, BoxError>((started.elapsed(), serde_json::from_slice::<Value>(&body)?))
        })
    };
    tokio::time::sleep(Duration::from_millis(400)).await;
    app.post(
        "/api/v1/messages",
        &ada,
        json!({"recipient_id": "c:bob", "message": "wake up"}),
    )
    .await?;
    let (elapsed, woken) = waiting.await??;
    assert!(
        elapsed < Duration::from_secs(5),
        "long-poll returned after {elapsed:?}"
    );
    assert_eq!(
        woken["data"]["events"].as_array().map(Vec::len),
        Some(1),
        "{woken}"
    );
    // An empty long-poll returns empty after its wait.
    let empty_cursor = woken["data"]["cursor"].as_str().unwrap_or_default();
    let quiet = sync(&app, &bob, &format!("cursor={empty_cursor}&wait=1")).await?;
    assert_eq!(quiet["events"], json!([]));
    let too_long = app.get("/api/v1/sync?wait=31", &bob).await?;
    assert_eq!(too_long.0, StatusCode::UNPROCESSABLE_ENTITY);
    // A cursor from another account, or signed with another key, asks for a reset.
    let foreign = app
        .get(&format!("/api/v1/sync?cursor={empty_cursor}"), &ada)
        .await?;
    assert_eq!(foreign.1["error"]["code"], "sync_reset_required");
    cancellation.cancel();
    listener.await?;
    app.close().await
}

#[tokio::test]
async fn sync_skips_groups_the_account_left() -> Result<(), BoxError> {
    let app = TestApp::start().await?;
    let p = people(&app).await;
    let ada = app.accounts.token(&p.ada);
    let bob = app.accounts.token(&p.bob);
    let group = app
        .post(
            "/api/v1/groups",
            &ada,
            json!({"name": "Team", "member_ids": ["c:bob"]}),
        )
        .await?;
    let id = group.1["id"].as_str().unwrap_or_default().to_owned();
    app.post(&format!("/api/v1/groups/{id}/accept"), &bob, json!(null))
        .await?;
    let start = sync(&app, &bob, "reset=true").await?;
    app.post(
        &format!("/api/v1/conversations/{id}/messages"),
        &ada,
        json!({"message": "while a member"}),
    )
    .await?;
    let member_page = sync(
        &app,
        &bob,
        &format!("cursor={}", start["cursor"].as_str().unwrap_or_default()),
    )
    .await?;
    assert_eq!(member_page["events"].as_array().map(Vec::len), Some(1));
    let left = app
        .post(&format!("/api/v1/groups/{id}/leave"), &bob, json!(null))
        .await?;
    assert_eq!(left.0, StatusCode::NO_CONTENT);
    app.post(
        &format!("/api/v1/conversations/{id}/messages"),
        &ada,
        json!({"message": "after leaving"}),
    )
    .await?;
    // Re-reading from the start: the event from before leaving is no longer authorized
    // either (references follow current access), and the position still advances.
    let again = sync(
        &app,
        &bob,
        &format!("cursor={}", start["cursor"].as_str().unwrap_or_default()),
    )
    .await?;
    assert_eq!(again["events"], json!([]), "{again}");
    app.close().await
}
