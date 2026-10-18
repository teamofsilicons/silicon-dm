//! Conversation events in the sync feed: when a conversation appears for, changes for or
//! leaves an account, that account's stream says so, but only to clients that ask
//! (`include=conversations`); everyone else's feed is exactly as before and its cursor moves
//! past them. Each event names the conversation (as GET /api/v1/conversations names it) and
//! what happened.
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
    cy: TestAccount,
    scout: TestAccount,
}

async fn people(app: &TestApp) -> People {
    let ada = TestAccount::carbon("a8K", "c:ada");
    let bob = TestAccount::carbon("Bb2", "c:bob");
    let cy = TestAccount::carbon("Cy3", "c:cyd");
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

/// One account's feed: a cursor and what the account has been told since.
struct Feed {
    token: String,
    cursor: String,
}

impl Feed {
    async fn start(app: &TestApp, token: String) -> Result<Self, BoxError> {
        let page = app.get("/api/v1/sync?reset=true", &token).await?;
        expect_status(&page, StatusCode::OK)?;
        let cursor = page.1["cursor"].as_str().unwrap_or_default().to_owned();
        Ok(Self { token, cursor })
    }

    /// The conversation events since the last call, as (conversation, change); message
    /// events are left out.
    async fn changes(&mut self, app: &TestApp) -> Result<Vec<(String, String)>, BoxError> {
        let page = app
            .get(
                &format!("/api/v1/sync?include=conversations&cursor={}", self.cursor),
                &self.token,
            )
            .await?;
        expect_status(&page, StatusCode::OK)?;
        page.1["cursor"]
            .as_str()
            .unwrap_or_default()
            .clone_into(&mut self.cursor);
        let events = page.1["events"].as_array().cloned().unwrap_or_default();
        for event in &events {
            if event["type"] == "conversation" {
                assert!(event.get("message_id").is_none(), "{event}");
                assert!(event["event_id"].is_string() && event["sequence"].is_i64());
            } else {
                assert!(event.get("change").is_none(), "{event}");
            }
        }
        Ok(events
            .iter()
            .filter(|event| event["type"] == "conversation")
            .map(|event| {
                (
                    event["conversation_id"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                    event["change"].as_str().unwrap_or_default().to_owned(),
                )
            })
            .collect())
    }
}

fn pairs(conversation: &str, changes: &[&str]) -> Vec<(String, String)> {
    changes
        .iter()
        .map(|change| (conversation.to_owned(), (*change).to_owned()))
        .collect()
}

/// The changes of `feed` since its last read, for `group` only (asserting nothing else came).
async fn heard(app: &TestApp, feed: &mut Feed, group: &str) -> Result<Vec<String>, BoxError> {
    let changes = feed.changes(app).await?;
    assert!(
        changes
            .iter()
            .all(|(conversation, _)| conversation == group),
        "{changes:?}"
    );
    Ok(changes.into_iter().map(|(_, change)| change).collect())
}

async fn version(app: &TestApp, token: &str, group: &str) -> Result<i64, BoxError> {
    let read = app.get(&format!("/api/v1/groups/{group}"), token).await?;
    expect_status(&read, StatusCode::OK)?;
    read.1["group"]["version"]
        .as_i64()
        .ok_or_else(|| format!("a group version: {}", read.1).into())
}

#[tokio::test]
async fn every_group_change_reaches_the_accounts_it_concerns() -> Result<(), BoxError> {
    let app = TestApp::start().await?;
    let p = people(&app).await;
    let ada_token = app.accounts.token(&p.ada);
    let bob_token = app.accounts.token(&p.bob);
    let cy_token = app.accounts.token(&p.cy);
    let scout_token = app.accounts.token(&p.scout);
    // DM first meets Scout here and makes its conversation with Ada, its custodian (a
    // `created` for both): start every feed after that.
    expect_status(&app.get("/api/v1/me", &scout_token).await?, StatusCode::OK)?;
    let mut ada = Feed::start(&app, ada_token.clone()).await?;
    let mut bob = Feed::start(&app, bob_token.clone()).await?;
    let mut cy = Feed::start(&app, cy_token.clone()).await?;
    let mut scout = Feed::start(&app, scout_token).await?;
    let none: Vec<String> = Vec::new();

    // Created: the creator and the Silicon its custodian adds are members at once; Bob is
    // invited; Cy hears nothing.
    let created = app
        .post(
            "/api/v1/groups",
            &ada_token,
            json!({"name": "Events", "member_ids": ["c:bob", "si:scout"]}),
        )
        .await?;
    expect_status(&created, StatusCode::CREATED)?;
    assert_eq!(created.1["draft_version"], Value::Null);
    let group = created.1["id"].as_str().unwrap_or_default().to_owned();
    assert_eq!(group, "g:events");
    assert_eq!(heard(&app, &mut ada, &group).await?, ["created"]);
    assert_eq!(heard(&app, &mut scout, &group).await?, ["created"]);
    assert_eq!(heard(&app, &mut bob, &group).await?, ["invited"]);
    assert_eq!(heard(&app, &mut cy, &group).await?, none);

    // An invitation answered: the invitee's own devices hear it, the members see a member join.
    let accepted = app
        .post(
            &format!("/api/v1/groups/{group}/accept"),
            &bob_token,
            json!(null),
        )
        .await?;
    expect_status(&accepted, StatusCode::OK)?;
    assert_eq!(accepted.1["draft_version"], Value::Null);
    assert_eq!(
        heard(&app, &mut bob, &group).await?,
        ["invitation_accepted"]
    );
    assert_eq!(heard(&app, &mut ada, &group).await?, ["members_changed"]);
    assert_eq!(heard(&app, &mut scout, &group).await?, ["members_changed"]);

    let invited = app
        .post(
            &format!("/api/v1/groups/{group}/members"),
            &ada_token,
            json!({"member_ids": ["c:cyd"]}),
        )
        .await?;
    expect_status(&invited, StatusCode::OK)?;
    assert_eq!(heard(&app, &mut cy, &group).await?, ["invited"]);
    for feed in [&mut ada, &mut bob, &mut scout] {
        assert_eq!(heard(&app, feed, &group).await?, ["members_changed"]);
    }
    let declined = app
        .post(
            &format!("/api/v1/groups/{group}/decline"),
            &cy_token,
            json!(null),
        )
        .await?;
    assert_eq!(declined.0, StatusCode::NO_CONTENT);
    assert_eq!(heard(&app, &mut cy, &group).await?, ["invitation_declined"]);
    for feed in [&mut ada, &mut bob, &mut scout] {
        assert_eq!(heard(&app, feed, &group).await?, ["members_changed"]);
    }
    // Inviting someone who is already invited changes nothing, so nobody hears anything.
    for _ in 0..2 {
        let again = app
            .post(
                &format!("/api/v1/groups/{group}/members"),
                &ada_token,
                json!({"member_ids": ["c:cyd"]}),
            )
            .await?;
        expect_status(&again, StatusCode::OK)?;
    }
    assert_eq!(heard(&app, &mut cy, &group).await?, ["invited"]);
    for feed in [&mut ada, &mut bob, &mut scout] {
        assert_eq!(heard(&app, feed, &group).await?, ["members_changed"]);
    }

    // Renamed and described: one event for each, for members and the pending invitee (an
    // invitation shows the name). Saving the same settings again says nothing.
    let renamed = app
        .call(
            Method::PATCH,
            &format!("/api/v1/groups/{group}"),
            Some(&ada_token),
            Some(json!({"name": "Event planning", "description": "Who brings what"})),
            &[(
                "if-match",
                version(&app, &ada_token, &group).await?.to_string(),
            )],
        )
        .await?;
    expect_status(&renamed, StatusCode::OK)?;
    for feed in [&mut ada, &mut bob, &mut scout, &mut cy] {
        assert_eq!(
            heard(&app, feed, &group).await?,
            ["renamed", "description_changed"]
        );
    }
    let same = app
        .call(
            Method::PATCH,
            &format!("/api/v1/groups/{group}"),
            Some(&ada_token),
            Some(json!({"name": "Event planning", "description": "Who brings what"})),
            &[(
                "if-match",
                version(&app, &ada_token, &group).await?.to_string(),
            )],
        )
        .await?;
    expect_status(&same, StatusCode::OK)?;
    let described = app
        .call(
            Method::PATCH,
            &format!("/api/v1/groups/{group}"),
            Some(&ada_token),
            Some(json!({"name": "Event planning", "description": "Bring snacks"})),
            &[(
                "if-match",
                version(&app, &ada_token, &group).await?.to_string(),
            )],
        )
        .await?;
    expect_status(&described, StatusCode::OK)?;
    for feed in [&mut ada, &mut bob, &mut scout, &mut cy] {
        assert_eq!(heard(&app, feed, &group).await?, ["description_changed"]);
    }

    // Owners: members and the pending invitee (an invitation names the owners) hear of it.
    let promoted = app
        .post(
            &format!("/api/v1/groups/{group}/owners"),
            &ada_token,
            json!({"member_ids": ["c:bob"]}),
        )
        .await?;
    expect_status(&promoted, StatusCode::OK)?;
    for feed in [&mut ada, &mut bob, &mut scout, &mut cy] {
        assert_eq!(heard(&app, feed, &group).await?, ["owners_changed"]);
    }

    // An invitation withdrawn.
    let cancelled = app
        .call(
            Method::DELETE,
            &format!("/api/v1/groups/{group}/members"),
            Some(&ada_token),
            Some(json!({"member_ids": ["c:cyd"]})),
            &[],
        )
        .await?;
    expect_status(&cancelled, StatusCode::OK)?;
    assert_eq!(
        heard(&app, &mut cy, &group).await?,
        ["invitation_cancelled"]
    );
    for feed in [&mut ada, &mut bob, &mut scout] {
        assert_eq!(heard(&app, feed, &group).await?, ["members_changed"]);
    }

    // An owner leaves: its own devices hear `left`, the rest a member and an owner gone.
    let left = app
        .post(
            &format!("/api/v1/groups/{group}/leave"),
            &bob_token,
            json!(null),
        )
        .await?;
    assert_eq!(left.0, StatusCode::NO_CONTENT);
    assert_eq!(heard(&app, &mut bob, &group).await?, ["left"]);
    for feed in [&mut ada, &mut scout] {
        assert_eq!(
            heard(&app, feed, &group).await?,
            ["members_changed", "owners_changed"]
        );
    }
    let gone = app
        .get(&format!("/api/v1/conversations/{group}"), &bob_token)
        .await?;
    assert_eq!(
        gone.0,
        StatusCode::NOT_FOUND,
        "a left group is gone, as `left` says"
    );

    // A custodian takes its Silicon out; a retry of the same request tells nobody twice.
    let key = vec![("idempotency-key", "remove-scout-1".to_owned())];
    for _ in 0..2 {
        let removed = app
            .call(
                Method::DELETE,
                &format!("/api/v1/groups/{group}/members"),
                Some(&ada_token),
                Some(json!({"member_ids": ["si:scout"]})),
                &key,
            )
            .await?;
        expect_status(&removed, StatusCode::OK)?;
    }
    assert_eq!(heard(&app, &mut scout, &group).await?, ["removed"]);
    assert_eq!(heard(&app, &mut ada, &group).await?, ["members_changed"]);
    assert_eq!(
        heard(&app, &mut bob, &group).await?,
        none,
        "Bob left before"
    );
    assert_eq!(heard(&app, &mut cy, &group).await?, none);
    app.close().await
}

#[tokio::test]
async fn a_deleted_account_changes_the_conversations_it_was_in() -> Result<(), BoxError> {
    let app = TestApp::start().await?;
    let p = people(&app).await;
    let ada_token = app.accounts.token(&p.ada);
    let bob_token = app.accounts.token(&p.bob);
    let created = app
        .post(
            "/api/v1/groups",
            &bob_token,
            json!({"name": "Bobs", "member_ids": ["c:ada"]}),
        )
        .await?;
    expect_status(&created, StatusCode::CREATED)?;
    app.post("/api/v1/groups/g:bobs/accept", &ada_token, json!(null))
        .await?;
    app.post(
        "/api/v1/messages",
        &bob_token,
        json!({"recipient_id": "c:ada", "message": "hi"}),
    )
    .await?;
    // A group Bob left before: his deletion changes nothing there.
    let old = app
        .post(
            "/api/v1/groups",
            &bob_token,
            json!({"name": "Old days", "member_ids": ["c:ada"]}),
        )
        .await?;
    expect_status(&old, StatusCode::CREATED)?;
    app.post("/api/v1/groups/g:old-days/accept", &ada_token, json!(null))
        .await?;
    expect_status(
        &app.post(
            "/api/v1/groups/g:old-days/owners",
            &bob_token,
            json!({"member_ids": ["c:ada"]}),
        )
        .await?,
        StatusCode::OK,
    )?;
    let left = app
        .post("/api/v1/groups/g:old-days/leave", &bob_token, json!(null))
        .await?;
    assert_eq!(left.0, StatusCode::NO_CONTENT, "{}", left.1);
    let mut ada = Feed::start(&app, ada_token.clone()).await?;
    app.state
        .store
        .delete_account(p.bob.uuid.as_str(), time::OffsetDateTime::now_utc())
        .await?;
    let mut changes = ada.changes(&app).await?;
    changes.sort();
    assert_eq!(
        changes,
        [
            ("Bb2::c:ada".to_owned(), "members_changed".to_owned()),
            ("g:bobs".to_owned(), "members_changed".to_owned()),
            ("g:bobs".to_owned(), "owners_changed".to_owned()),
        ],
        "the deleted owner's group passed to Ada, and their conversation shows them deleted (by uuid)"
    );
    app.close().await
}

#[tokio::test]
async fn new_conversations_appear_for_everyone_in_them() -> Result<(), BoxError> {
    let app = TestApp::start().await?;
    let p = people(&app).await;
    let ada_token = app.accounts.token(&p.ada);
    let bob_token = app.accounts.token(&p.bob);
    let mut ada = Feed::start(&app, ada_token.clone()).await?;
    let mut bob = Feed::start(&app, bob_token.clone()).await?;
    let mut cy = Feed::start(&app, app.accounts.token(&p.cy)).await?;

    // A first message starts a direct conversation: both sides hear of it, then of the message.
    let sent = app
        .post(
            "/api/v1/messages",
            &bob_token,
            json!({"recipient_id": "c:cyd", "message": "hello"}),
        )
        .await?;
    expect_status(&sent, StatusCode::ACCEPTED)?;
    assert_eq!(
        bob.changes(&app).await?,
        pairs("c:bob::c:cyd", &["created"])
    );
    assert_eq!(cy.changes(&app).await?, pairs("c:bob::c:cyd", &["created"]));
    // Starting it again finds the same conversation: nothing new to say.
    let again = app
        .post(
            "/api/v1/conversations",
            &bob_token,
            json!({"participant_ids": ["c:cyd"]}),
        )
        .await?;
    expect_status(&again, StatusCode::CREATED)?;
    assert!(bob.changes(&app).await?.is_empty());

    // A conversation of several is named by its UUID.
    let several = app
        .post(
            "/api/v1/conversations",
            &ada_token,
            json!({"participant_ids": ["c:bob", "c:cyd"]}),
        )
        .await?;
    expect_status(&several, StatusCode::CREATED)?;
    let id = several.1["id"].as_str().unwrap_or_default().to_owned();
    for feed in [&mut ada, &mut bob, &mut cy] {
        assert_eq!(feed.changes(&app).await?, pairs(&id, &["created"]));
    }

    // DM makes a Silicon's conversation with its custodian by itself, the first time it sees
    // the Silicon: the custodian's list gains it.
    let mut fresh_ada = Feed::start(&app, ada_token.clone()).await?;
    let nova = TestAccount::silicon("Nv4", "si:nova", &p.ada);
    app.accounts.add_account(&nova).await;
    let me = app.get("/api/v1/me", &app.accounts.token(&nova)).await?;
    expect_status(&me, StatusCode::OK)?;
    assert_eq!(
        fresh_ada.changes(&app).await?,
        pairs("c:ada::si:nova", &["created"])
    );
    app.close().await
}

#[tokio::test]
async fn only_clients_that_ask_get_conversation_events() -> Result<(), BoxError> {
    let app = TestApp::start().await?;
    let p = people(&app).await;
    let ada = app.accounts.token(&p.ada);
    let bob = app.accounts.token(&p.bob);
    let start = app.get("/api/v1/sync?reset=true", &bob).await?;
    let cursor = start.1["cursor"].as_str().unwrap_or_default().to_owned();
    let created = app
        .post(
            "/api/v1/groups",
            &ada,
            json!({"name": "Quiet", "member_ids": ["c:bob"]}),
        )
        .await?;
    expect_status(&created, StatusCode::CREATED)?;
    app.post("/api/v1/groups/g:quiet/accept", &bob, json!(null))
        .await?;
    let sent = app
        .post(
            "/api/v1/conversations/g:quiet/messages",
            &ada,
            json!({"message": "welcome"}),
        )
        .await?;
    expect_status(&sent, StatusCode::ACCEPTED)?;

    // An older client: only the message, and its cursor moves past the conversation events.
    let plain = app
        .get(&format!("/api/v1/sync?cursor={cursor}"), &bob)
        .await?;
    expect_status(&plain, StatusCode::OK)?;
    let events = plain.1["events"].as_array().cloned().unwrap_or_default();
    assert_eq!(events.len(), 1, "{}", plain.1);
    assert_eq!(events[0]["type"], "message");
    assert_eq!(events[0]["message_id"], "000");
    let after = plain.1["cursor"].as_str().unwrap_or_default().to_owned();
    let later = app
        .get(
            &format!("/api/v1/sync?include=conversations&cursor={after}"),
            &bob,
        )
        .await?;
    assert_eq!(later.1["events"], json!([]), "nothing is told twice");

    // A client that asks: the same stream, in order, with the conversation events in it.
    let full = app
        .get(
            &format!("/api/v1/sync?include=conversations&cursor={cursor}"),
            &bob,
        )
        .await?;
    let kinds: Vec<String> = full.1["events"]
        .as_array()
        .map(|events| {
            events
                .iter()
                .map(|event| {
                    format!(
                        "{}:{}",
                        event["type"].as_str().unwrap_or_default(),
                        event["change"]
                            .as_str()
                            .or_else(|| event["message_id"].as_str())
                            .unwrap_or_default()
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        kinds,
        [
            "conversation:invited",
            "conversation:invitation_accepted",
            "message:000"
        ],
        "{}",
        full.1
    );
    let sequences: Vec<i64> = full.1["events"]
        .as_array()
        .map(|events| {
            events
                .iter()
                .filter_map(|event| event["sequence"].as_i64())
                .collect()
        })
        .unwrap_or_default();
    assert!(
        sequences.windows(2).all(|pair| pair[0] < pair[1]),
        "{sequences:?}"
    );
    assert!(
        full.1["cursor"]
            .as_str()
            .is_some_and(|cursor| !cursor.is_empty()),
        "a cursor to continue from"
    );

    // An unknown kind is refused, naming the ones there are.
    let unknown = app.get("/api/v1/sync?include=presence", &bob).await?;
    assert_eq!(unknown.0, StatusCode::UNPROCESSABLE_ENTITY, "{}", unknown.1);
    assert_eq!(unknown.1["error"]["code"], "validation_error");
    assert_eq!(
        unknown.1["error"]["details"]["valid"],
        json!(["conversations"])
    );

    // A long-poll that asks wakes for a conversation event.
    let cancellation = tokio_util::sync::CancellationToken::new();
    let listener = tokio::spawn(
        app.state
            .sync
            .clone()
            .run(app.state.store.clone(), cancellation.clone()),
    );
    let head = app
        .get("/api/v1/sync?reset=true&include=conversations", &bob)
        .await?;
    let cursor = head.1["cursor"].as_str().unwrap_or_default().to_owned();
    let current = version(&app, &ada, "g:quiet").await?;
    let renamer = {
        let router = app.router.clone();
        let ada = ada.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            let request = axum::http::Request::builder()
                .method(Method::PATCH)
                .uri("/api/v1/groups/g:quiet")
                .header("authorization", format!("Bearer {ada}"))
                .header("content-type", "application/json")
                .header("idempotency-key", "rename-quiet-1")
                .header("if-match", current.to_string())
                .body(axum::body::Body::from(
                    json!({"type": "update_group", "data": {"name": "Loud"}}).to_string(),
                ))?;
            tower::ServiceExt::oneshot(router, request).await?;
            Ok::<(), BoxError>(())
        })
    };
    let started = Instant::now();
    let woken = app
        .get(
            &format!("/api/v1/sync?include=conversations&wait=10&cursor={cursor}"),
            &bob,
        )
        .await?;
    renamer.await??;
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(woken.1["events"][0]["change"], "renamed", "{}", woken.1);
    cancellation.cancel();
    listener.await?;

    // Discovery announces it.
    let contracts = app.get("/api/v1/contracts", &bob).await?;
    let feature = &contracts.1["features"]["conversation_events"];
    assert_eq!(feature["sync_query"], "include=conversations");
    assert_eq!(feature["changes"].as_array().map(Vec::len), Some(12));
    app.close().await
}

/// Reads every event of a feed from `cursor` to the head, conversation events included.
async fn drain(app: &TestApp, token: &str, cursor: &str) -> Result<Vec<Value>, BoxError> {
    let mut cursor = cursor.to_owned();
    let mut events = Vec::new();
    loop {
        let page = app
            .get(
                &format!("/api/v1/sync?include=conversations&limit=100&cursor={cursor}"),
                token,
            )
            .await?;
        expect_status(&page, StatusCode::OK)?;
        events.extend(page.1["events"].as_array().cloned().unwrap_or_default());
        page.1["cursor"]
            .as_str()
            .unwrap_or_default()
            .clone_into(&mut cursor);
        if page.1["has_more"] != true {
            return Ok(events);
        }
    }
}

#[tokio::test]
async fn racing_sends_receipts_and_group_changes_never_deadlock_or_lose_an_event()
-> Result<(), BoxError> {
    let app = TestApp::start().await?;
    let p = people(&app).await;
    let ada = app.accounts.token(&p.ada);
    let bob = app.accounts.token(&p.bob);
    let cy = app.accounts.token(&p.cy);
    let scout = app.accounts.token(&p.scout);
    let created = app
        .post(
            "/api/v1/groups",
            &ada,
            json!({"name": "Race", "member_ids": ["c:bob", "c:cyd", "si:scout"]}),
        )
        .await?;
    expect_status(&created, StatusCode::CREATED)?;
    for token in [&bob, &cy] {
        expect_status(
            &app.post("/api/v1/groups/g:race/accept", token, json!(null))
                .await?,
            StatusCode::OK,
        )?;
    }
    let mut starts = Vec::new();
    for token in [&ada, &bob, &cy, &scout] {
        let head = app
            .get("/api/v1/sync?reset=true&include=conversations", token)
            .await?;
        starts.push(head.1["cursor"].as_str().unwrap_or_default().to_owned());
    }
    let rounds = 12;
    // Every writer below takes the members' stream locks at once: message fan-out, the
    // senders' receipt announcements and the owners' conversation events.
    let senders = async {
        for round in 0..rounds {
            for (token, who) in [(&bob, "bob"), (&cy, "cy"), (&scout, "scout")] {
                let sent = app
                    .post(
                        "/api/v1/conversations/g:race/messages",
                        token,
                        json!({"message": format!("{who} {round}")}),
                    )
                    .await?;
                expect_status(&sent, StatusCode::ACCEPTED)?;
            }
        }
        Ok::<(), BoxError>(())
    };
    let reader = async {
        for _ in 0..rounds * 2 {
            let latest = app
                .get("/api/v1/conversations/g:race/messages?limit=1", &ada)
                .await?;
            let Some(code) = latest.1["items"][0]["message-id"].as_str() else {
                tokio::task::yield_now().await;
                continue;
            };
            let (status, _, body) = app
                .post_envelope(
                    "/api/v1/conversations/g:race/receipts",
                    &ada,
                    &json!({"type": "receipts.read", "data": {"up_to": code}}),
                )
                .await?;
            assert!(
                status == StatusCode::OK,
                "{status}: {}",
                String::from_utf8_lossy(&body)
            );
        }
        Ok::<(), BoxError>(())
    };
    let owners = async {
        for round in 0..rounds {
            let (method, path) = (
                if round % 2 == 0 {
                    Method::POST
                } else {
                    Method::DELETE
                },
                "/api/v1/groups/g:race/owners",
            );
            let changed = app
                .call(
                    method,
                    path,
                    Some(&ada),
                    Some(json!({"member_ids": ["c:bob"]})),
                    &[],
                )
                .await?;
            expect_status(&changed, StatusCode::OK)?;
        }
        Ok::<(), BoxError>(())
    };
    let (sent, read, owned) = tokio::join!(senders, reader, owners);
    sent?;
    read?;
    owned?;

    // Every feed is whole (no gap: no reset asked for) and told every owner change.
    for (token, start) in [&ada, &bob, &cy, &scout].into_iter().zip(&starts) {
        let events = drain(&app, token, start).await?;
        let sequences: Vec<i64> = events
            .iter()
            .filter_map(|event| event["sequence"].as_i64())
            .collect();
        assert!(
            sequences.windows(2).all(|pair| pair[0] < pair[1]),
            "{sequences:?}"
        );
        let owner_changes = events
            .iter()
            .filter(|event| event["change"] == "owners_changed")
            .count();
        assert_eq!(owner_changes, usize::try_from(rounds)?, "{events:?}");
        let messages = events
            .iter()
            .filter(|event| event["type"] == "message")
            .count();
        assert_eq!(messages, usize::try_from(rounds * 3)?, "{events:?}");
    }
    app.close().await
}

#[tokio::test]
async fn fan_out_never_waits_on_another_writers_directory_refresh() -> Result<(), BoxError> {
    // Two writers whose targets overlap, as a send (which refreshes its sender's directory row
    // before it fans out) and a bulk receipt (which fans out to several senders at once) are.
    // The receipt must not wait on the sender's refreshed row while it holds another target's
    // stream: the send then waits for that stream, and PostgreSQL fails one of them.
    let app = TestApp::start().await?;
    let p = people(&app).await;
    let ada = app.accounts.token(&p.ada);
    let created = app
        .post(
            "/api/v1/groups",
            &ada,
            json!({"name": "Locks", "member_ids": ["si:scout"]}),
        )
        .await?;
    expect_status(&created, StatusCode::CREATED)?;
    // Bob and Cy have sent before, so their directory rows exist (every send refreshes them).
    for who in [&p.bob, &p.cy] {
        let sent = app
            .post(
                "/api/v1/messages",
                &app.accounts.token(who),
                json!({"recipient_id": "c:ada", "message": "hello"}),
            )
            .await?;
        expect_status(&sent, StatusCode::ACCEPTED)?;
    }
    let conversation: uuid::Uuid =
        sqlx::query_scalar("SELECT conversation_id FROM groups WHERE public_id = 'g:locks'")
            .fetch_one(app.state.store.pool())
            .await?;
    let (bob, cy) = (p.bob.uuid.clone(), p.cy.uuid.clone());
    let event = "INSERT INTO conversation_events (id, target_kind, target_id, conversation_id, change, sequence)
                 VALUES ($1, 'carbon', $2, $3, 'members_changed', 1)";
    let pool = app.state.store.pool().clone();
    let sender = {
        let (pool, bob, cy) = (pool.clone(), bob.clone(), cy.clone());
        tokio::spawn(async move {
            let mut tx = pool.begin().await?;
            // What refresh_directory_in does for the sender, Cy.
            let refreshed = sqlx::query(
                "UPDATE actor_snapshots SET refreshed_at = clock_timestamp()
                 WHERE organization_id = '@accounts' AND actor_kind = 'carbon' AND actor_id = $1",
            )
            .bind(&cy)
            .execute(&mut *tx)
            .await?
            .rows_affected();
            assert_eq!(refreshed, 1, "Cy has a directory row to refresh");
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            sqlx::query(event)
                .bind(uuid::Uuid::now_v7())
                .bind(&bob)
                .bind(conversation)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            Ok::<(), sqlx::Error>(())
        })
    };
    let fan_out = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let mut tx = pool.begin().await?;
        for target in [&bob, &cy] {
            sqlx::query(event)
                .bind(uuid::Uuid::now_v7())
                .bind(target)
                .bind(conversation)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok::<(), sqlx::Error>(())
    });
    let started = std::time::Instant::now();
    let (sent, fanned) = (sender.await?, fan_out.await?);
    assert!(sent.is_ok() && fanned.is_ok(), "{sent:?} {fanned:?}");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(1),
        "nobody waited for a deadlock to be broken: {:?}",
        started.elapsed()
    );
    app.close().await
}
