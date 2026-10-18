//! Unread counts on conversation summaries and reading in bulk: counts follow receipts in
//! direct conversations and groups, deleted and bundled messages never count, bulk receipts
//! are idempotent and announce status changes like single receipts, and a custodian's view
//! is read only.
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
    app::{TestApp, data_of, expect_status},
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

/// The summary of one conversation in the caller's list.
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

async fn bulk(
    app: &TestApp,
    token: &str,
    conversation: &str,
    envelope: Value,
) -> Result<(StatusCode, Value), BoxError> {
    let (status, _, body) = app
        .post_envelope(
            &format!("/api/v1/conversations/{conversation}/receipts"),
            token,
            &envelope,
        )
        .await?;
    Ok((status, data_of(&body)))
}

#[tokio::test]
async fn unread_counts_follow_receipts_in_direct_conversations_and_groups() -> Result<(), BoxError>
{
    let app = TestApp::start().await?;
    let p = people(&app).await;
    let ada = app.accounts.token(&p.ada);
    let bob = app.accounts.token(&p.bob);
    let cy = app.accounts.token(&p.cy);
    let mut codes = Vec::new();
    for text in ["one", "two", "three", "four"] {
        let sent = app
            .post(
                "/api/v1/messages",
                &bob,
                json!({"recipient_id": "c:ada", "message": text}),
            )
            .await?;
        expect_status(&sent, StatusCode::ACCEPTED)?;
        codes.push(code(&sent.1));
    }
    let pair = "c:ada::c:bob";
    let fresh = summary(&app, &ada, "", pair).await?;
    assert_eq!(fresh["unread_count"], 4, "{fresh}");
    assert_eq!(fresh["last_read_message_id"], Value::Null);
    let sender = summary(&app, &bob, "", pair).await?;
    assert_eq!(
        sender["unread_count"], 0,
        "your own messages are never unread"
    );

    // One read receipt for the second message.
    let read = app
        .post(
            &format!(
                "/api/v1/conversations/{pair}/messages/{}/receipts",
                codes[1]
            ),
            &ada,
            json!({"status": "read", "device_id": "ada-phone"}),
        )
        .await?;
    expect_status(&read, StatusCode::OK)?;
    let after_one = summary(&app, &ada, "", pair).await?;
    assert_eq!(after_one["unread_count"], 3);
    assert_eq!(after_one["last_read_message_id"], codes[1].as_str());
    // A delivered receipt does not make a message read.
    app.post(
        &format!(
            "/api/v1/conversations/{pair}/messages/{}/receipts",
            codes[0]
        ),
        &ada,
        json!({"status": "delivered", "device_id": "ada-phone"}),
    )
    .await?;
    assert_eq!(summary(&app, &ada, "", pair).await?["unread_count"], 3);
    // Deleted messages never count.
    let deleted = app
        .call(
            Method::DELETE,
            &format!("/api/v1/conversations/{pair}/messages/{}", codes[3]),
            Some(&bob),
            None,
            &[],
        )
        .await?;
    expect_status(&deleted, StatusCode::OK)?;
    assert_eq!(summary(&app, &ada, "", pair).await?["unread_count"], 2);

    // Reading everything up to the third message.
    let (status, outcome) = bulk(
        &app,
        &ada,
        pair,
        json!({"type": "receipts.read", "data": {"up_to": codes[2]}}),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{outcome}");
    assert_eq!(
        outcome["updated"], 2,
        "the first and third; the second was read"
    );
    assert_eq!(outcome["unread_count"], 0);
    assert_eq!(outcome["last_read_message_id"], codes[2].as_str());
    // The single conversation read says the same.
    let single = app
        .get(&format!("/api/v1/conversations/{pair}"), &ada)
        .await?;
    expect_status(&single, StatusCode::OK)?;
    assert_eq!(single.1["unread_count"], 0);
    assert_eq!(single.1["last_read_message_id"], codes[2].as_str());
    assert_eq!(
        single.1["last_message"]["message-id"],
        codes[3].as_str(),
        "the tombstone is still the last message"
    );
    assert!(single.1["participants"].is_array());

    // Groups: every member has their own count.
    let group = app
        .post(
            "/api/v1/groups",
            &ada,
            json!({"name": "Unread", "member_ids": ["c:bob", "c:cy"]}),
        )
        .await?;
    let id = group.1["id"].as_str().unwrap_or_default().to_owned();
    for token in [&bob, &cy] {
        app.post(&format!("/api/v1/groups/{id}/accept"), token, json!(null))
            .await?;
    }
    let mut group_codes = Vec::new();
    for text in ["hello all", "agenda"] {
        let sent = app
            .post(
                &format!("/api/v1/conversations/{id}/messages"),
                &ada,
                json!({"message": text}),
            )
            .await?;
        expect_status(&sent, StatusCode::ACCEPTED)?;
        group_codes.push(code(&sent.1));
    }
    assert_eq!(summary(&app, &bob, "", &id).await?["unread_count"], 2);
    assert_eq!(summary(&app, &cy, "", &id).await?["unread_count"], 2);
    assert_eq!(summary(&app, &ada, "", &id).await?["unread_count"], 0);
    let (status, outcome) = bulk(
        &app,
        &bob,
        &id,
        json!({"type": "receipts.read", "data": {"up_to": group_codes[1]}}),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{outcome}");
    assert_eq!(outcome["unread_count"], 0);
    assert_eq!(summary(&app, &bob, "", &id).await?["unread_count"], 0);
    assert_eq!(
        summary(&app, &cy, "", &id).await?["unread_count"],
        2,
        "cy has not read"
    );
    let group_summary = summary(&app, &ada, "", &id).await?;
    assert_eq!(
        group_summary["last_message_status"], "sent",
        "a group message is delivered or read only when every recipient has it"
    );
    app.close().await
}

#[tokio::test]
async fn bulk_receipts_are_idempotent_and_announce_status_like_single_ones() -> Result<(), BoxError>
{
    let app = TestApp::start().await?;
    let p = people(&app).await;
    let ada = app.accounts.token(&p.ada);
    let bob = app.accounts.token(&p.bob);
    let pair = "c:ada::c:bob";
    let mut codes = Vec::new();
    for text in ["a", "b", "c"] {
        let sent = app
            .post(
                "/api/v1/messages",
                &bob,
                json!({"recipient_id": "c:ada", "message": text}),
            )
            .await?;
        codes.push(code(&sent.1));
    }
    let start = app.get("/api/v1/sync?reset=true", &bob).await?;
    let cursor = start.1["cursor"].as_str().unwrap_or_default().to_owned();

    let (status, delivered) = bulk(
        &app,
        &ada,
        pair,
        json!({"type": "receipts.delivered", "data": {"up_to": codes[1], "device_id": "ada-laptop"}}),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{delivered}");
    assert_eq!(delivered["updated"], 2);
    assert_eq!(delivered["unread_count"], 3, "delivered is not read");
    assert_eq!(delivered["last_read_message_id"], Value::Null);
    let message = app
        .get(
            &format!("/api/v1/conversations/{pair}/messages/{}", codes[0]),
            &bob,
        )
        .await?;
    assert!(message.1["delivered_at"].is_string());
    assert_eq!(message.1["read_at"], Value::Null);

    let (_, read) = bulk(
        &app,
        &ada,
        pair,
        json!({"type": "receipts.read", "data": {"up_to": codes[2]}}),
    )
    .await?;
    assert_eq!(read["updated"], 3);
    assert_eq!(read["unread_count"], 0);
    let (_, again) = bulk(
        &app,
        &ada,
        pair,
        json!({"type": "receipts.read", "data": {"up_to": codes[2]}}),
    )
    .await?;
    assert_eq!(again["updated"], 0, "nothing changes twice");
    let (_, delivered_again) = bulk(
        &app,
        &ada,
        pair,
        json!({"type": "receipts", "data": {"up_to": codes[2], "status": "delivered"}}),
    )
    .await?;
    assert_eq!(
        delivered_again["updated"], 0,
        "read already implies delivered"
    );

    // The sender heard about each change, as with single receipts: delivered for the first
    // two, read for all three.
    let page = app
        .get(&format!("/api/v1/sync?cursor={cursor}"), &bob)
        .await?;
    let statuses: Vec<&str> = page.1["events"]
        .as_array()
        .map(|events| {
            events
                .iter()
                .filter(|event| event["type"] == "message_status")
                .filter_map(|event| event["message_id"].as_str())
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(statuses.len(), 5, "{}", page.1);
    for (index, message) in codes.iter().enumerate() {
        let expected = if index < 2 { 2 } else { 1 };
        assert_eq!(
            statuses
                .iter()
                .filter(|seen| **seen == message.as_str())
                .count(),
            expected,
            "{message}"
        );
        let read = app
            .get(
                &format!("/api/v1/conversations/{pair}/messages/{message}"),
                &bob,
            )
            .await?;
        assert!(read.1["read_at"].is_string(), "{}", read.1);
    }
    let listed = summary(&app, &bob, "", pair).await?;
    assert_eq!(listed["last_message_status"], "read");

    // Mistakes are explained.
    for (envelope, needle) in [
        (
            json!({"type": "receipts", "data": {"up_to": codes[0]}}),
            "data.status",
        ),
        (
            json!({"type": "receipts.read", "data": {"up_to": codes[0], "status": "delivered"}}),
            "contradicts",
        ),
        (
            json!({"type": "receipt", "data": {"up_to": codes[0]}}),
            "receipts.read",
        ),
    ] {
        let (status, error) = bulk(&app, &ada, pair, envelope).await?;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            error["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains(needle)),
            "{error}"
        );
    }
    let (status, _) = bulk(
        &app,
        &ada,
        pair,
        json!({"type": "receipts.read", "data": {"up_to": "zzz"}}),
    )
    .await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = bulk(
        &app,
        &ada,
        pair,
        json!({"type": "receipts.read", "data": {"up_to": codes[0], "extra": 1}}),
    )
    .await?;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    app.close().await
}

#[tokio::test]
async fn custodian_views_show_the_silicons_unread_and_are_read_only() -> Result<(), BoxError> {
    let app = TestApp::start().await?;
    let p = people(&app).await;
    let ada = app.accounts.token(&p.ada);
    let bob = app.accounts.token(&p.bob);
    let scout = app.accounts.token(&p.scout);
    let opened = app
        .post(
            "/api/v1/messages",
            &scout,
            json!({"recipient_id": "c:bob", "message": "hi bob"}),
        )
        .await?;
    expect_status(&opened, StatusCode::ACCEPTED)?;
    let pair = "c:bob::si:scout";
    let mut replies = Vec::new();
    for text in ["hi scout", "how are you"] {
        let reply = app
            .post(
                &format!("/api/v1/conversations/{pair}/messages"),
                &bob,
                json!({"message": text}),
            )
            .await?;
        replies.push(code(&reply.1));
    }
    let seen_by_custodian = summary(&app, &ada, "?for=si:scout", pair).await?;
    assert_eq!(
        seen_by_custodian["unread_count"], 2,
        "the Silicon's unread, not the custodian's"
    );
    let single = app
        .get(&format!("/api/v1/conversations/{pair}?for=si:scout"), &ada)
        .await?;
    expect_status(&single, StatusCode::OK)?;
    assert_eq!(single.1["unread_count"], 2);
    let refused = app
        .get(&format!("/api/v1/conversations/{pair}?for=si:scout"), &bob)
        .await?;
    assert_eq!(refused.1["error"]["code"], "not_custodian");

    let (status, error) = bulk(
        &app,
        &ada,
        pair,
        json!({"type": "receipts.read", "data": {"up_to": replies[1]}}),
    )
    .await?;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(error["error"]["code"], "read_only_view");
    assert!(error["error"]["hint"].is_string());
    let (status, _, body) = app
        .post_envelope(
            &format!("/api/v1/conversations/{pair}/receipts?for=si:scout"),
            &scout,
            &json!({"type": "receipts.read", "data": {"up_to": replies[1]}}),
        )
        .await?;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(data_of(&body)["error"]["code"], "read_only_view");

    // The Silicon marks its own messages read.
    let (status, outcome) = bulk(
        &app,
        &scout,
        pair,
        json!({"type": "receipts.read", "data": {"up_to": replies[1]}}),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{outcome}");
    assert_eq!(outcome["updated"], 2);
    assert_eq!(
        summary(&app, &ada, "?for=si:scout", pair).await?["unread_count"],
        0
    );
    app.close().await
}

#[tokio::test]
async fn bundled_messages_count_once_through_their_display_message() -> Result<(), BoxError> {
    let app = TestApp::start().await?;
    let p = people(&app).await;
    let ada = app.accounts.token(&p.ada);
    let scout = app.accounts.token(&p.scout);
    let mut codes = Vec::new();
    for text in ["part one", "part two"] {
        let sent = app
            .post(
                "/api/v1/messages",
                &scout,
                json!({"recipient_id": "c:ada", "message": text}),
            )
            .await?;
        codes.push(code(&sent.1));
    }
    let pair = "c:ada::si:scout";
    assert_eq!(summary(&app, &ada, "", pair).await?["unread_count"], 2);
    let bundle = app
        .post(
            &format!("/api/v1/conversations/{pair}/bundles"),
            &scout,
            json!({"message_ids": codes, "display_message": {"message": "both parts"}}),
        )
        .await?;
    expect_status(&bundle, StatusCode::CREATED)?;
    assert_eq!(
        summary(&app, &ada, "", pair).await?["unread_count"],
        1,
        "the members are shown through the display message"
    );
    let display = code(&bundle.1["display_message"]);
    let (status, outcome) = bulk(
        &app,
        &ada,
        pair,
        json!({"type": "receipts.read", "data": {"up_to": display}}),
    )
    .await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(outcome["updated"], 3, "members are marked read too");
    assert_eq!(outcome["unread_count"], 0);
    app.close().await
}

#[tokio::test]
async fn an_account_that_joins_a_group_starts_with_nothing_unread() -> Result<(), BoxError> {
    let app = TestApp::start().await?;
    let p = people(&app).await;
    let ada = app.accounts.token(&p.ada);
    let bob = app.accounts.token(&p.bob);
    let cy = app.accounts.token(&p.cy);
    let scout = app.accounts.token(&p.scout);
    let group = app
        .post(
            "/api/v1/groups",
            &ada,
            json!({"name": "Joiners", "member_ids": ["c:cy"]}),
        )
        .await?;
    expect_status(&group, StatusCode::CREATED)?;
    let id = group.1["id"].as_str().unwrap_or_default().to_owned();
    expect_status(
        &app.post(&format!("/api/v1/groups/{id}/accept"), &cy, json!(null))
            .await?,
        StatusCode::OK,
    )?;
    let mut codes = Vec::new();
    for text in ["before one", "before two", "before three"] {
        let sent = app
            .post(
                &format!("/api/v1/conversations/{id}/messages"),
                &ada,
                json!({"message": text}),
            )
            .await?;
        expect_status(&sent, StatusCode::ACCEPTED)?;
        codes.push(code(&sent.1));
    }
    // Cy reads them all: Ada's ticks say read, because Cy was the one who received them.
    let (status, _) = bulk(
        &app,
        &cy,
        &id,
        json!({"type": "receipts.read", "data": {"up_to": codes[2]}}),
    )
    .await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        summary(&app, &ada, "", &id).await?["last_message_status"],
        "read"
    );

    // Bob is invited and accepts; Ada's custodian adds Scout, which joins at once.
    let invited = app
        .post(
            &format!("/api/v1/groups/{id}/members"),
            &ada,
            json!({"member_ids": ["c:bob", "si:scout"]}),
        )
        .await?;
    expect_status(&invited, StatusCode::OK)?;
    expect_status(
        &app.post(&format!("/api/v1/groups/{id}/accept"), &bob, json!(null))
            .await?,
        StatusCode::OK,
    )?;
    for (token, who) in [(&bob, "bob"), (&scout, "scout")] {
        let joined = summary(&app, token, "", &id).await?;
        assert_eq!(
            joined["unread_count"], 0,
            "{who}: what was said before it joined is history, not unread: {joined}"
        );
    }
    assert_eq!(
        summary(&app, &ada, "", &id).await?["last_message_status"],
        "read",
        "joining never changes the ticks of messages sent before"
    );
    // Bob still reads the whole history.
    let history = app
        .get(&format!("/api/v1/conversations/{id}/messages"), &bob)
        .await?;
    assert_eq!(history.1["items"].as_array().map(Vec::len), Some(3));

    // What is said after joining counts, and waits for the new member's receipts.
    let after = app
        .post(
            &format!("/api/v1/conversations/{id}/messages"),
            &ada,
            json!({"message": "after"}),
        )
        .await?;
    expect_status(&after, StatusCode::ACCEPTED)?;
    assert_eq!(summary(&app, &bob, "", &id).await?["unread_count"], 1);
    assert_eq!(summary(&app, &cy, "", &id).await?["unread_count"], 1);
    // A member invited again keeps its unread messages.
    expect_status(
        &app.post(
            &format!("/api/v1/groups/{id}/members"),
            &ada,
            json!({"member_ids": ["si:scout"]}),
        )
        .await?,
        StatusCode::OK,
    )?;
    assert_eq!(summary(&app, &scout, "", &id).await?["unread_count"], 1);
    for token in [&bob, &cy, &scout] {
        let (status, _) = bulk(
            &app,
            token,
            &id,
            json!({"type": "receipts.read", "data": {"up_to": code(&after.1)}}),
        )
        .await?;
        assert_eq!(status, StatusCode::OK);
    }
    assert_eq!(
        summary(&app, &ada, "", &id).await?["last_message_status"],
        "read",
        "read by everyone it was delivered to"
    );
    app.close().await
}

#[tokio::test]
async fn bulk_receipts_start_above_what_was_already_marked() -> Result<(), BoxError> {
    let app = TestApp::start().await?;
    let p = people(&app).await;
    let ada = app.accounts.token(&p.ada);
    let bob = app.accounts.token(&p.bob);
    let pair = "c:ada::c:bob";
    let mut codes = Vec::new();
    for index in 0..6 {
        let sent = app
            .post(
                "/api/v1/messages",
                &bob,
                json!({"recipient_id": "c:ada", "message": format!("m{index}")}),
            )
            .await?;
        codes.push(code(&sent.1));
    }
    // Delivered up to the fourth, then up to the sixth: the second call marks only the two
    // it has not marked.
    let (_, first) = bulk(
        &app,
        &ada,
        pair,
        json!({"type": "receipts.delivered", "data": {"up_to": codes[3]}}),
    )
    .await?;
    assert_eq!(first["updated"], 4);
    let (_, second) = bulk(
        &app,
        &ada,
        pair,
        json!({"type": "receipts.delivered", "data": {"up_to": codes[5]}}),
    )
    .await?;
    assert_eq!(second["updated"], 2);
    // Reading half, then all.
    let (_, half) = bulk(
        &app,
        &ada,
        pair,
        json!({"type": "receipts.read", "data": {"up_to": codes[2]}}),
    )
    .await?;
    assert_eq!(half["updated"], 3);
    assert_eq!(half["unread_count"], 3);
    let (_, rest) = bulk(
        &app,
        &ada,
        pair,
        json!({"type": "receipts.read", "data": {"up_to": codes[5]}}),
    )
    .await?;
    assert_eq!(rest["updated"], 3);
    assert_eq!(rest["unread_count"], 0);
    let positions: (i64, i64) = sqlx::query_as(
        "SELECT read_through_sequence, delivered_through_sequence FROM conversation_read_states
         WHERE actor_id = $1",
    )
    .bind(&p.ada.uuid)
    .fetch_one(app.state.store.pool())
    .await?;
    assert_eq!(positions, (6, 6), "both positions sit on the sixth message");
    // A new message is the only thing a later call looks at.
    let late = app
        .post(
            "/api/v1/messages",
            &bob,
            json!({"recipient_id": "c:ada", "message": "late"}),
        )
        .await?;
    let (_, late_read) = bulk(
        &app,
        &ada,
        pair,
        json!({"type": "receipts.read", "data": {"up_to": code(&late.1)}}),
    )
    .await?;
    assert_eq!(late_read["updated"], 1);
    let message = app
        .get(
            &format!("/api/v1/conversations/{pair}/messages/{}", code(&late.1)),
            &bob,
        )
        .await?;
    assert!(message.1["read_at"].is_string(), "{}", message.1);
    app.close().await
}

#[tokio::test]
async fn unread_counts_stop_at_a_thousand() -> Result<(), BoxError> {
    let app = TestApp::start().await?;
    let p = people(&app).await;
    let ada = app.accounts.token(&p.ada);
    let scout = app.accounts.token(&p.scout);
    let pair = "c:ada::si:scout";
    let cap = silicon_dm::infrastructure::postgres::UNREAD_COUNT_CAP;
    let mut last = String::new();
    for index in 0..=cap {
        let sent = app
            .post(
                "/api/v1/messages",
                &ada,
                json!({"recipient_id": "si:scout", "message": format!("note {index}")}),
            )
            .await?;
        expect_status(&sent, StatusCode::ACCEPTED)?;
        last = code(&sent.1);
    }
    // A Silicon that never marks anything read: its count says "a thousand or more".
    let listed = summary(&app, &scout, "", pair).await?;
    assert_eq!(listed["unread_count"], cap, "{listed}");
    let (status, outcome) = bulk(
        &app,
        &scout,
        pair,
        json!({"type": "receipts.read", "data": {"up_to": last}}),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{outcome}");
    assert_eq!(outcome["updated"], cap + 1);
    assert_eq!(outcome["unread_count"], 0);
    app.close().await
}
