//! The delivery outbox keeps a window, not everything: the worker raises an account's sync
//! floor past old events and deletes them (and Ting handoffs finished that long ago), and
//! nothing that matters needed them. Receipts on old messages still turn the ticks (the
//! recipients are kept with the message), edits of old messages are still edits, a scan
//! without a cursor starts at the floor, and a cursor below it is told to reset.
#![allow(
    clippy::too_many_lines,
    reason = "each test is one scenario, read top to bottom"
)]
mod support;

use axum::http::{Method, StatusCode};
use serde_json::{Value, json};
use silicon_dm::worker::TingDeliveryWorker;
use support::{
    BoxError,
    accounts::TestAccount,
    app::{TestApp, data_of, expect_status},
};

fn code(message: &Value) -> String {
    message["message-id"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

async fn count(app: &TestApp, sql: &str, account: &str) -> Result<i64, BoxError> {
    Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_owned()))
        .bind(account)
        .fetch_one(app.state.store.pool())
        .await?)
}

#[tokio::test]
async fn old_events_go_and_what_needed_them_keeps_working() -> Result<(), BoxError> {
    let app = TestApp::start().await?;
    let ada = TestAccount::carbon("a8K", "c:ada");
    let bob = TestAccount::carbon("Bb2", "c:bob");
    app.accounts.add_accounts(&[&ada, &bob]).await;
    let ada_token = app.accounts.token(&ada);
    let bob_token = app.accounts.token(&bob);
    let pair = "c:ada::c:bob";
    let mut codes = Vec::new();
    for text in ["one", "two", "three"] {
        let sent = app
            .post(
                "/api/v1/messages",
                &bob_token,
                json!({"recipient_id": "c:ada", "message": text}),
            )
            .await?;
        expect_status(&sent, StatusCode::ACCEPTED)?;
        codes.push(code(&sent.1));
    }
    let edited = app
        .call(
            Method::PATCH,
            &format!("/api/v1/conversations/{pair}/messages/{}", codes[1]),
            Some(&bob_token),
            Some(json!({"message": "two, edited"})),
            &[],
        )
        .await?;
    expect_status(&edited, StatusCode::OK)?;
    let old_cursor = app.get("/api/v1/sync", &ada_token).await?;
    expect_status(&old_cursor, StatusCode::OK)?;
    assert!(
        old_cursor.1["events"]
            .as_array()
            .is_some_and(|events| events.len() >= 4),
        "three messages and an edit: {}",
        old_cursor.1
    );
    let early_cursor = app.get("/api/v1/sync?limit=1", &ada_token).await?;
    let early_cursor = early_cursor.1["cursor"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    // Ting is not configured here: the worker marks the handoffs discarded (finished).
    TingDeliveryWorker::new(app.state.clone(), None)
        .process_once()
        .await?;

    // Everything so far is older than the window (a cutoff a minute from now).
    let cutoff = time::OffsetDateTime::now_utc() + time::Duration::minutes(1);
    let pruned = app.state.store.prune_sync_history(cutoff).await?;
    assert!(pruned.deliveries >= 8, "{pruned:?}");
    assert!(pruned.ting_handoffs >= 8, "{pruned:?}");
    for account in [&ada.uuid, &bob.uuid] {
        assert_eq!(
            count(
                &app,
                "SELECT count(*) FROM actor_deliveries WHERE target_id = $1",
                account
            )
            .await?,
            0
        );
        let (floor, head): (i64, i64) = sqlx::query_as(
            "SELECT sync_floor, next_sequence - 1 FROM actor_delivery_streams
             WHERE organization_id = '@accounts' AND actor_id = $1",
        )
        .bind(account)
        .fetch_one(app.state.store.pool())
        .await?;
        assert_eq!(floor, head, "the floor sits on the newest event it removed");
    }
    assert_eq!(
        count(
            &app,
            "SELECT count(*) FROM ting_handoffs WHERE target_id = $1",
            &ada.uuid
        )
        .await?,
        0
    );
    // Nothing is deleted below a floor twice, and history itself is untouched.
    assert_eq!(app.state.store.prune_sync_history(cutoff).await?.total(), 0);
    let history = app
        .get(
            &format!("/api/v1/conversations/{pair}/messages"),
            &ada_token,
        )
        .await?;
    assert_eq!(history.1["items"].as_array().map(Vec::len), Some(3));

    // A scan without a cursor starts at the floor; a cursor below it is told to reset.
    let fresh = app.get("/api/v1/sync", &ada_token).await?;
    expect_status(&fresh, StatusCode::OK)?;
    assert_eq!(fresh.1["events"], json!([]));
    let stale = app
        .get(&format!("/api/v1/sync?cursor={early_cursor}"), &ada_token)
        .await?;
    assert_eq!(stale.0, StatusCode::CONFLICT, "{}", stale.1);
    assert_eq!(stale.1["error"]["code"], "sync_reset_required");
    let cursor = fresh.1["cursor"].as_str().unwrap_or_default().to_owned();
    let bob_cursor = app.get("/api/v1/sync?reset=true", &bob_token).await?;
    let bob_cursor = bob_cursor.1["cursor"]
        .as_str()
        .unwrap_or_default()
        .to_owned();

    // Reading old messages still turns the sender's ticks: the recipients are kept with the
    // message, not in the outbox.
    let read = app
        .post(
            &format!(
                "/api/v1/conversations/{pair}/messages/{}/receipts",
                codes[0]
            ),
            &ada_token,
            json!({"status": "read", "device_id": "ada-phone"}),
        )
        .await?;
    expect_status(&read, StatusCode::OK)?;
    assert!(read.1["read_at"].is_string(), "{}", read.1);
    let (status, _, body) = app
        .post_envelope(
            &format!("/api/v1/conversations/{pair}/receipts"),
            &ada_token,
            &json!({"type": "receipts.read", "data": {"up_to": codes[2]}}),
        )
        .await?;
    assert_eq!(status, StatusCode::OK, "{}", data_of(&body));
    for message in &codes {
        let seen = app
            .get(
                &format!("/api/v1/conversations/{pair}/messages/{message}"),
                &bob_token,
            )
            .await?;
        assert!(seen.1["read_at"].is_string(), "{message}: {}", seen.1);
    }
    let statuses = app
        .get(&format!("/api/v1/sync?cursor={bob_cursor}"), &bob_token)
        .await?;
    assert_eq!(
        statuses.1["events"].as_array().map(|events| events
            .iter()
            .filter(|event| event["type"] == "message_status")
            .count()),
        Some(3),
        "the sender hears about each: {}",
        statuses.1
    );

    // An old message edited again is an edit (never a "new" message to Ting), and readers
    // hear about it.
    let again = app
        .call(
            Method::PATCH,
            &format!("/api/v1/conversations/{pair}/messages/{}", codes[1]),
            Some(&bob_token),
            Some(json!({"message": "two, edited again"})),
            &[],
        )
        .await?;
    expect_status(&again, StatusCode::OK)?;
    let events: Vec<String> = sqlx::query_scalar(
        "SELECT event FROM ting_handoffs WHERE target_id = $1 ORDER BY delivery_sequence",
    )
    .bind(&ada.uuid)
    .fetch_all(app.state.store.pool())
    .await?;
    assert_eq!(events, ["message.updated"]);
    let revisions: Vec<i64> = sqlx::query_scalar(
        "SELECT DISTINCT delivery_revision FROM actor_deliveries d
         JOIN messages m ON m.id = d.message_id
         WHERE m.sequence = 2 AND d.delivery_kind = 'message'",
    )
    .fetch_all(app.state.store.pool())
    .await?;
    assert!(
        revisions.iter().all(|revision| *revision >= 2),
        "{revisions:?}"
    );
    let page = app
        .get(&format!("/api/v1/sync?cursor={cursor}"), &ada_token)
        .await?;
    assert!(
        page.1["events"].as_array().is_some_and(|events| events
            .iter()
            .any(|event| event["type"] == "message" && event["message_id"] == codes[1].as_str())),
        "{}",
        page.1
    );

    // A handoff still waiting to be sent is never deleted, however old.
    let pending = app
        .post(
            "/api/v1/messages",
            &bob_token,
            json!({"recipient_id": "c:ada", "message": "four"}),
        )
        .await?;
    expect_status(&pending, StatusCode::ACCEPTED)?;
    let later = time::OffsetDateTime::now_utc() + time::Duration::minutes(1);
    app.state.store.prune_sync_history(later).await?;
    assert!(
        count(
            &app,
            "SELECT count(*) FROM ting_handoffs WHERE target_id = $1 AND accepted_at IS NULL AND discarded_at IS NULL",
            &ada.uuid
        )
        .await?
            > 0,
        "pending handoffs stay until they are sent or discarded"
    );
    // A delivery is only ever deleted at or below its account's floor.
    let newest = app
        .post(
            "/api/v1/messages",
            &bob_token,
            json!({"recipient_id": "c:ada", "message": "five"}),
        )
        .await?;
    expect_status(&newest, StatusCode::ACCEPTED)?;
    let refused = sqlx::query(
        "DELETE FROM actor_deliveries WHERE target_id = $1
         AND sequence > (SELECT sync_floor FROM actor_delivery_streams
                         WHERE organization_id = '@accounts' AND actor_id = $1)",
    )
    .bind(&ada.uuid)
    .execute(app.state.store.pool())
    .await;
    assert!(refused.is_err(), "the database refuses it");
    app.close().await
}

/// Who a message reached is recorded by the database from the send's own first-revision outbox
/// rows (migration 0049), not by the service's code: a service that does not know
/// `message_recipients` (0.16, if the code is rolled back after the 0.17 migrations) still sends
/// messages whose ticks move.
#[tokio::test]
async fn who_a_message_reached_is_recorded_by_the_database() -> Result<(), BoxError> {
    let app = TestApp::start().await?;
    let ada = TestAccount::carbon("a8K", "c:ada");
    let bob = TestAccount::carbon("Bb2", "c:bob");
    let cy = TestAccount::carbon("Cy3", "c:cy");
    app.accounts.add_accounts(&[&ada, &bob, &cy]).await;
    let ada_token = app.accounts.token(&ada);
    let bob_token = app.accounts.token(&bob);
    let cy_token = app.accounts.token(&cy);
    let group = app
        .post(
            "/api/v1/groups",
            &ada_token,
            json!({"name": "Recipients", "member_ids": ["c:bob", "c:cy"]}),
        )
        .await?;
    expect_status(&group, StatusCode::CREATED)?;
    let id = group.1["id"].as_str().unwrap_or_default().to_owned();
    for token in [&bob_token, &cy_token] {
        expect_status(
            &app.post(&format!("/api/v1/groups/{id}/accept"), token, json!(null))
                .await?,
            StatusCode::OK,
        )?;
    }
    let sent = app
        .post(
            &format!("/api/v1/conversations/{id}/messages"),
            &ada_token,
            json!({"message": "who got this"}),
        )
        .await?;
    expect_status(&sent, StatusCode::ACCEPTED)?;
    let recipients: Vec<String> = sqlx::query_scalar(
        "SELECT r.recipient_id FROM message_recipients r
         JOIN messages m ON m.id = r.message_id
         WHERE m.sender_id = $1 ORDER BY r.recipient_id",
    )
    .bind(&ada.uuid)
    .fetch_all(app.state.store.pool())
    .await?;
    let mut expected = vec![bob.uuid.clone(), cy.uuid.clone()];
    expected.sort();
    assert_eq!(
        recipients, expected,
        "everyone it reached, the sender aside"
    );
    // Read by everyone it reached, and only then: Ada's tick waits for Cy after Bob.
    let message = code(&sent.1);
    for (token, everyone) in [(&bob_token, false), (&cy_token, true)] {
        let (status, _, body) = app
            .post_envelope(
                &format!("/api/v1/conversations/{id}/receipts"),
                token,
                &json!({"type": "receipts.read", "data": {"up_to": message}}),
            )
            .await?;
        assert_eq!(status, StatusCode::OK, "{}", data_of(&body));
        let seen = app
            .get(
                &format!("/api/v1/conversations/{id}/messages/{message}"),
                &ada_token,
            )
            .await?;
        assert_eq!(seen.1["read_at"].is_string(), everyone, "{}", seen.1);
    }
    app.close().await
}
