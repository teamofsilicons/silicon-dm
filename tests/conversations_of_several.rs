//! Conversations of several: three or more accounts in one conversation that is not a group.
//! It is addressed by its UUID everywhere (the API, the sync feed, the Ting handoff), and
//! everything a two-party conversation does works in it: sending, routing, receipts one by one
//! and in bulk, unread counts, drafts and `draft_version`, speech, bundles, and going on after
//! one of its accounts is deleted.
#![allow(
    clippy::too_many_lines,
    reason = "each test is one scenario, read top to bottom"
)]
mod support;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::http::{Method, StatusCode};
use serde_json::{Value, json};
use silicon_dm::{
    infrastructure::{
        postgres::TingDeliveryClaim,
        ting::{TingAcceptance, TingFailure, TingPublisher},
    },
    worker::{SpeechWorker, TingDeliveryWorker},
};
use support::{
    BoxError,
    accounts::TestAccount,
    app::{TestApp, data_of, expect_status},
    deepgram::FakeDeepgram,
};

struct People {
    ada: TestAccount,
    bob: TestAccount,
    cy: TestAccount,
    scout: TestAccount,
}

async fn people(app: &TestApp) -> People {
    let ada = TestAccount::carbon("a8K", "c:ada");
    let bob = TestAccount::carbon("Bb2", "c:bob");
    let cy = TestAccount::carbon("Cy3", "c:cy");
    let scout = TestAccount::silicon("Sc0", "si:scout", &ada);
    app.accounts.add_accounts(&[&ada, &bob, &cy, &scout]).await;
    app.accounts.live_introspection().await;
    People {
        ada,
        bob,
        cy,
        scout,
    }
}

fn code(message: &Value) -> String {
    message["message-id"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

/// Ada starts a conversation with `others`; answers its id (a UUID).
async fn start(app: &TestApp, ada: &str, others: &[&str]) -> Result<String, BoxError> {
    let created = app
        .post(
            "/api/v1/conversations",
            ada,
            json!({"participant_ids": others}),
        )
        .await?;
    expect_status(&created, StatusCode::CREATED)?;
    let id = created.1["id"].as_str().unwrap_or_default().to_owned();
    assert!(
        uuid::Uuid::parse_str(&id).is_ok(),
        "a conversation of several is addressed by its UUID: {}",
        created.1
    );
    assert_eq!(
        created.1["participants"].as_array().map(Vec::len),
        Some(others.len() + 1)
    );
    assert_eq!(created.1["draft_version"], Value::Null, "{}", created.1);
    Ok(id)
}

async fn send(
    app: &TestApp,
    token: &str,
    conversation: &str,
    body: Value,
) -> Result<Value, BoxError> {
    let sent = app
        .post(
            &format!("/api/v1/conversations/{conversation}/messages"),
            token,
            body,
        )
        .await?;
    expect_status(&sent, StatusCode::ACCEPTED)?;
    Ok(sent.1)
}

async fn summary(app: &TestApp, token: &str, query: &str, id: &str) -> Result<Value, BoxError> {
    let page = app
        .get(&format!("/api/v1/conversations{query}"), token)
        .await?;
    expect_status(&page, StatusCode::OK)?;
    page.1["items"]
        .as_array()
        .and_then(|items| items.iter().find(|item| item["id"] == id).cloned())
        .ok_or_else(|| format!("{id} is not listed: {}", page.1).into())
}

async fn sync(app: &TestApp, token: &str, query: &str) -> Result<Value, BoxError> {
    let page = app.get(&format!("/api/v1/sync?{query}"), token).await?;
    expect_status(&page, StatusCode::OK)?;
    Ok(page.1)
}

/// Records what would have gone to Ting and accepts it.
#[derive(Default)]
struct Recording {
    sent: Mutex<Vec<(String, Value)>>,
}

#[async_trait]
impl TingPublisher for Recording {
    async fn publish(&self, claim: &TingDeliveryClaim) -> Result<TingAcceptance, TingFailure> {
        let body = serde_json::from_str(&claim.request_body).unwrap_or(Value::Null);
        if let Ok(mut sent) = self.sent.lock() {
            sent.push((claim.target.id.to_string(), body));
        }
        Ok(TingAcceptance {
            id: format!("ting-{}", claim.delivery_id),
            created_at: time::OffsetDateTime::now_utc(),
            silent: false,
        })
    }
}

#[tokio::test]
async fn messages_receipts_unread_drafts_and_ting_work_in_a_conversation_of_several()
-> Result<(), BoxError> {
    let app = TestApp::start().await?;
    let p = people(&app).await;
    let ada = app.accounts.token(&p.ada);
    let bob = app.accounts.token(&p.bob);
    let scout = app.accounts.token(&p.scout);
    let bob_start = sync(&app, &bob, "reset=true").await?;
    let bob_cursor = bob_start["cursor"].as_str().unwrap_or_default().to_owned();
    let id = start(&app, &ada, &["c:bob", "si:scout"]).await?;
    // The same set of accounts always has the same conversation.
    let again = app
        .post(
            "/api/v1/conversations",
            &bob,
            json!({"participant_ids": ["si:scout", "c:ada"]}),
        )
        .await?;
    expect_status(&again, StatusCode::CREATED)?;
    assert_eq!(again.1["id"], id.as_str());

    // Sending: plain, and routed to one participant (a Carbon, or a Silicon's intent).
    let hello = send(&app, &ada, &id, json!({"message": "hello all"})).await?;
    assert_eq!(code(&hello), "000");
    assert_eq!(hello["conversation_id"], id.as_str());
    assert_eq!(
        hello["recipient_id"],
        id.as_str(),
        "an unrouted message names the conversation"
    );
    assert_eq!(hello["routed_to"], Value::Null);
    let to_bob = send(
        &app,
        &scout,
        &id,
        json!({"message": "Bob, your build passed", "recipient_id": "c:bob"}),
    )
    .await?;
    assert_eq!(to_bob["routed_to"], "c:bob", "{to_bob}");
    assert_eq!(to_bob["recipient_id"], "c:bob");
    let to_intent = send(
        &app,
        &bob,
        &id,
        json!({"message": "think about it", "recipient_id": "deliberate@si:scout"}),
    )
    .await?;
    assert_eq!(to_intent["routed_to"], "deliberate@si:scout");
    let codes = [code(&hello), code(&to_bob), code(&to_intent)];

    // Receiving: everyone reads the same history, and Bob's sync feed names the conversation
    // by its UUID.
    for token in [&ada, &bob, &scout] {
        let listed = app
            .get(&format!("/api/v1/conversations/{id}/messages"), token)
            .await?;
        expect_status(&listed, StatusCode::OK)?;
        assert_eq!(listed.1["items"].as_array().map(Vec::len), Some(3));
    }
    let feed = sync(&app, &bob, &format!("cursor={bob_cursor}")).await?;
    let messages: Vec<&Value> = feed["events"]
        .as_array()
        .map(|events| {
            events
                .iter()
                .filter(|event| event["type"] == "message")
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(messages.len(), 3, "{feed}");
    assert!(
        messages
            .iter()
            .all(|event| event["conversation_id"] == id.as_str())
    );

    // Unread: each account counts the others' messages.
    assert_eq!(summary(&app, &bob, "", &id).await?["unread_count"], 2);
    assert_eq!(summary(&app, &ada, "", &id).await?["unread_count"], 2);
    assert_eq!(summary(&app, &scout, "", &id).await?["unread_count"], 2);
    // ... and the custodian reads its Silicon's facts with `for`.
    assert_eq!(
        summary(&app, &ada, "?for=si:scout", &id).await?["unread_count"],
        2
    );

    // Receipts, one by one: read by every recipient makes a message read.
    for token in [&bob, &scout] {
        let read = app
            .post(
                &format!("/api/v1/conversations/{id}/messages/000/receipts"),
                token,
                json!({"status": "read", "device_id": "laptop"}),
            )
            .await?;
        expect_status(&read, StatusCode::OK)?;
    }
    let first = app
        .get(&format!("/api/v1/conversations/{id}/messages/000"), &ada)
        .await?;
    assert!(first.1["read_at"].is_string(), "{}", first.1);
    assert_eq!(summary(&app, &bob, "", &id).await?["unread_count"], 1);
    // ... and in bulk.
    let (status, _, body) = app
        .post_envelope(
            &format!("/api/v1/conversations/{id}/receipts"),
            &bob,
            &json!({"type": "receipts.read", "data": {"up_to": codes[2]}}),
        )
        .await?;
    let outcome = data_of(&body);
    assert_eq!(status, StatusCode::OK, "{outcome}");
    assert_eq!(outcome["updated"], 1, "only Scout's message was left");
    assert_eq!(outcome["unread_count"], 0);
    assert_eq!(outcome["last_read_message_id"], codes[1].as_str());

    // Drafts: the summary says which version there is, the draft route agrees, and the send
    // that matches it clears it.
    let saved = app
        .call(
            Method::PUT,
            &format!("/api/v1/conversations/{id}/draft"),
            Some(&bob),
            Some(json!({"message_content": "see you all", "metadata": {}})),
            &[],
        )
        .await?;
    expect_status(&saved, StatusCode::OK)?;
    let version = saved.1["version"].as_i64().ok_or("a draft version")?;
    assert_eq!(
        summary(&app, &bob, "", &id).await?["draft_version"],
        version
    );
    let single = app
        .get(&format!("/api/v1/conversations/{id}"), &bob)
        .await?;
    assert_eq!(single.1["draft_version"], version, "{}", single.1);
    assert_eq!(
        summary(&app, &ada, "", &id).await?["draft_version"],
        Value::Null,
        "drafts are each account's own"
    );
    send(&app, &bob, &id, json!({"message": "see you all"})).await?;
    assert_eq!(
        summary(&app, &bob, "", &id).await?["draft_version"],
        Value::Null,
        "the matching send cleared it"
    );
    let gone = app
        .get(&format!("/api/v1/conversations/{id}/draft"), &bob)
        .await?;
    assert_eq!(gone.0, StatusCode::NOT_FOUND);
    // A Silicon's draft, as its custodian sees it.
    let scout_draft = app
        .call(
            Method::PUT,
            &format!("/api/v1/conversations/{id}/draft"),
            Some(&scout),
            Some(json!({"message_content": "thinking", "metadata": {}})),
            &[],
        )
        .await?;
    expect_status(&scout_draft, StatusCode::OK)?;
    let viewed = app
        .get(&format!("/api/v1/conversations/{id}?for=si:scout"), &ada)
        .await?;
    assert_eq!(viewed.1["draft_version"], scout_draft.1["version"]);

    // The Ting handoffs name the conversation by its UUID, and reach every participant.
    let addresses: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT public_conversation_id FROM ting_handoffs WHERE conversation_id = $1",
    )
    .bind(uuid::Uuid::parse_str(&id)?)
    .fetch_all(app.state.store.pool())
    .await?;
    assert_eq!(addresses, vec![id.clone()]);
    let recording = Arc::new(Recording::default());
    TingDeliveryWorker::new(app.state.clone(), Some(recording.clone()))
        .process_once()
        .await?;
    let sent = recording
        .sent
        .lock()
        .map(|sent| sent.clone())
        .unwrap_or_default();
    let created: Vec<&(String, Value)> = sent
        .iter()
        .filter(|(_, body)| {
            body["data"]["event"] == "message.created" && body["data"]["message_id"] == "000"
        })
        .collect();
    assert_eq!(
        created.len(),
        3,
        "Ada's other devices, Bob and Scout: {sent:?}"
    );
    assert!(
        created
            .iter()
            .all(|(_, body)| body["data"]["conversation_id"] == id.as_str())
    );
    let routed = sent
        .iter()
        .find(|(target, body)| target == "Sc0" && body["data"]["message_id"] == codes[2].as_str())
        .map(|(_, body)| body.clone())
        .unwrap_or_default();
    assert_eq!(routed["metadata"], json!({"isi": "deliberate"}), "{routed}");
    app.close().await
}

#[tokio::test]
async fn speech_and_bundles_work_in_a_conversation_of_several() -> Result<(), BoxError> {
    let deepgram = FakeDeepgram::start().await;
    let app = TestApp::start_with_speech(&deepgram, |_| {}).await?;
    let p = people(&app).await;
    let ada = app.accounts.token(&p.ada);
    let bob = app.accounts.token(&p.bob);
    let scout = app.accounts.token(&p.scout);
    let id = start(&app, &ada, &["c:bob", "si:scout"]).await?;

    let spoken = send(
        &app,
        &scout,
        &id,
        json!({"speech": {"script": "[warmly] Morning, both of you."}}),
    )
    .await?;
    assert_eq!(spoken["speech"]["status"], "pending");
    assert_eq!(spoken["voice_transcript"], "Morning, both of you.");
    assert_eq!(
        SpeechWorker::new(app.state.clone()).process_once().await?,
        1
    );
    let rendered = app
        .get(
            &format!("/api/v1/conversations/{id}/messages/{}", code(&spoken)),
            &bob,
        )
        .await?;
    assert_eq!(rendered.1["speech"]["status"], "ready", "{}", rendered.1);
    let (status, headers, audio) = app
        .get_raw(
            &format!(
                "/api/v1/conversations/{id}/messages/{}/speech",
                code(&spoken)
            ),
            &bob,
            &[],
        )
        .await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["content-type"], "audio/wav");
    assert!(audio.starts_with(b"RIFF"));

    let mut parts = Vec::new();
    for text in ["first part", "second part"] {
        parts.push(code(
            &send(&app, &scout, &id, json!({"message": text})).await?,
        ));
    }
    let bundle = app
        .post(
            &format!("/api/v1/conversations/{id}/bundles"),
            &scout,
            json!({"message_ids": parts, "display_message": {"message": "both parts"}}),
        )
        .await?;
    expect_status(&bundle, StatusCode::CREATED)?;
    let listed = app
        .get(&format!("/api/v1/conversations/{id}/messages"), &ada)
        .await?;
    let texts: Vec<&str> = listed.1["items"]
        .as_array()
        .map(|items| items.iter().filter_map(|m| m["message"].as_str()).collect())
        .unwrap_or_default();
    assert!(
        texts.contains(&"both parts") && !texts.contains(&"first part"),
        "{texts:?}"
    );
    assert_eq!(
        summary(&app, &bob, "", &id).await?["unread_count"],
        2,
        "the spoken message and the bundle's display message"
    );
    app.close().await
}

#[tokio::test]
async fn a_conversation_of_several_goes_on_while_two_of_its_accounts_remain() -> Result<(), BoxError>
{
    let app = TestApp::start().await?;
    let p = people(&app).await;
    let ada = app.accounts.token(&p.ada);
    let bob = app.accounts.token(&p.bob);
    let id = start(&app, &ada, &["c:bob", "c:cy"]).await?;
    send(&app, &ada, &id, json!({"message": "before"})).await?;
    app.state
        .store
        .delete_account(p.cy.uuid.as_str(), time::OffsetDateTime::now_utc())
        .await?;
    // Two remain: the conversation takes messages, and nobody delivers to the deleted account.
    let after = send(&app, &bob, &id, json!({"message": "after"})).await?;
    assert_eq!(code(&after), "001");
    let listed = summary(&app, &ada, "", &id).await?;
    assert!(
        listed["participants"]
            .as_array()
            .is_some_and(|people| people.iter().any(|person| person["deleted"] == true)),
        "{listed}"
    );
    // One remains: there is nobody left to talk to.
    app.state
        .store
        .delete_account(p.bob.uuid.as_str(), time::OffsetDateTime::now_utc())
        .await?;
    let refused = app
        .post(
            &format!("/api/v1/conversations/{id}/messages"),
            &ada,
            json!({"message": "anyone?"}),
        )
        .await?;
    assert_eq!(refused.0, StatusCode::CONFLICT, "{}", refused.1);
    assert_eq!(refused.1["error"]["code"], "recipient_deleted");
    assert!(
        refused.1["error"]["message"]
            .as_str()
            .is_some_and(|message| message.starts_with("Every other account")),
        "{}",
        refused.1
    );
    app.close().await
}

#[tokio::test]
async fn every_account_in_a_conversation_of_several_must_reach_its_silicons() -> Result<(), BoxError>
{
    let app = TestApp::start().await?;
    let p = people(&app).await;
    let ada = app.accounts.token(&p.ada);
    let bob = app.accounts.token(&p.bob);
    let cy = app.accounts.token(&p.cy);
    let scout = app.accounts.token(&p.scout);
    // Scout hears from Bob; Cy is not on its allow-list.
    let allowed = app
        .post(
            "/api/v1/allow-list",
            &scout,
            json!({"account_ids": ["c:bob"]}),
        )
        .await?;
    expect_status(&allowed, StatusCode::OK)?;
    let alone = app
        .post(
            "/api/v1/conversations",
            &cy,
            json!({"participant_ids": ["si:scout"]}),
        )
        .await?;
    assert_eq!(
        alone.1["error"]["code"], "contact_not_allowed",
        "{}",
        alone.1
    );

    // Bob may reach Scout, but he cannot bring Cy into a conversation with it: everyone in a
    // conversation of several hears everyone, and nobody asked Scout.
    let refused = app
        .post(
            "/api/v1/conversations",
            &bob,
            json!({"participant_ids": ["si:scout", "c:cy"]}),
        )
        .await?;
    assert_eq!(refused.0, StatusCode::FORBIDDEN, "{}", refused.1);
    let error = &refused.1["error"];
    assert_eq!(error["code"], "contact_not_allowed");
    assert_eq!(error["details"]["silicon"], "si:scout");
    assert_eq!(error["details"]["requester"], "c:cy");
    assert_eq!(error["details"]["started_by"], "c:bob");
    assert_eq!(error["details"]["custodian"], "c:ada");
    assert_eq!(
        error["details"]["allow"]["by_custodian"],
        "dm allow add c:cy --for si:scout"
    );
    for words in [&error["message"], &error["hint"]] {
        assert!(
            words.as_str().is_some_and(|words| words.contains("c:cy")),
            "{error}"
        );
    }
    let none: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM conversations c WHERE NOT c.is_group
           AND (SELECT count(*) FROM conversation_participants p WHERE p.conversation_id = c.id) = 3",
    )
    .fetch_one(app.state.store.pool())
    .await?;
    assert_eq!(none, 0, "nothing was created");

    // The custodian's choice: Ada may put her Silicon in a conversation with anyone.
    let by_custodian = start(&app, &ada, &["si:scout", "c:cy"]).await?;
    send(&app, &cy, &by_custodian, json!({"message": "hi Scout"})).await?;
    // Bob with Ada (Scout's custodian) is fine too: Ada reaches Scout.
    start(&app, &bob, &["si:scout", "c:ada"]).await?;
    // Once Scout allows Cy, Bob may start it.
    let allowed = app
        .post(
            "/api/v1/allow-list",
            &scout,
            json!({"account_ids": ["c:cy"]}),
        )
        .await?;
    expect_status(&allowed, StatusCode::OK)?;
    let with_bob = app
        .post(
            "/api/v1/conversations",
            &bob,
            json!({"participant_ids": ["si:scout", "c:cy"]}),
        )
        .await?;
    expect_status(&with_bob, StatusCode::CREATED)?;
    // A Silicon may start one with Carbons it reaches: it chose them.
    let by_silicon = app
        .post(
            "/api/v1/conversations",
            &scout,
            json!({"participant_ids": ["c:bob", "c:ada", "c:cy"]}),
        )
        .await?;
    expect_status(&by_silicon, StatusCode::CREATED)?;
    app.close().await
}
