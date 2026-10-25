//! Delivery is our completed outbound handoff; reading still requires recipient acknowledgment.
#![allow(
    clippy::too_many_lines,
    reason = "one fixture follows partial, concurrent and early-read handoffs"
)]
mod support;

use axum::http::StatusCode;
use serde_json::json;
use silicon_dm::infrastructure::postgres::{TingDeliveryClaim, TingDeliveryContext};
use std::time::Duration;
use support::{
    BoxError,
    accounts::TestAccount,
    app::{TestApp, expect_status},
};

async fn accept(app: &TestApp, claim: &TingDeliveryClaim) -> Result<(), BoxError> {
    app.state
        .store
        .accept_ting_delivery(
            claim.delivery_id,
            claim.lease_id,
            &format!("fixture-{}", claim.delivery_id),
            time::OffsetDateTime::UNIX_EPOCH,
            false,
        )
        .await?;
    Ok(())
}

#[tokio::test]
async fn outbound_delivery_is_atomic_concurrent_and_independent_of_read_receipts()
-> Result<(), BoxError> {
    let app = TestApp::start().await?;
    let ada = TestAccount::carbon("a8K", "c:ada");
    let bob = TestAccount::carbon("Bb2", "c:bob");
    let cy = TestAccount::carbon("Cy3", "c:cy");
    app.accounts.add_accounts(&[&ada, &bob, &cy]).await;
    let sender_token = app.accounts.token(&ada);
    let created = app
        .post(
            "/api/v1/conversations",
            &sender_token,
            json!({"participant_ids":["c:bob","c:cy"]}),
        )
        .await?;
    expect_status(&created, StatusCode::CREATED)?;
    let chat = created.1["id"].as_str().ok_or("chat ID missing")?;
    let context = TingDeliveryContext {
        app_id: "dm".into(),
    };
    for scenario in ["partial", "concurrent", "read-first"] {
        let sent = app
            .post(
                &format!("/api/v1/conversations/{chat}/messages"),
                &sender_token,
                json!({"message":scenario}),
            )
            .await?;
        expect_status(&sent, StatusCode::ACCEPTED)?;
        let code = sent.1["message-id"]
            .as_str()
            .ok_or("message code missing")?;
        let path = format!("/api/v1/conversations/{chat}/messages/{code}");
        let claims = app
            .state
            .store
            .claim_ting_deliveries(scenario, 100, Duration::from_secs(60), &context)
            .await?;
        assert_eq!(claims.len(), 3);
        let sender = claims
            .iter()
            .find(|c| c.target.id.as_str() == ada.uuid)
            .ok_or("sender copy missing")?;
        let left = claims
            .iter()
            .find(|c| c.target.id.as_str() == bob.uuid)
            .ok_or("bob missing")?;
        let right = claims
            .iter()
            .find(|c| c.target.id.as_str() == cy.uuid)
            .ok_or("cy missing")?;
        if scenario == "read-first" {
            for reader in [&bob, &cy] {
                let result = app
                    .post(
                        &format!("{path}/receipts"),
                        &app.accounts.token(reader),
                        json!({"status":"read","device_id":"reader"}),
                    )
                    .await?;
                expect_status(&result, StatusCode::OK)?;
            }
        }
        if scenario == "partial" {
            accept(&app, sender).await?;
            accept(&app, left).await?;
            assert!(app.get(&path, &sender_token).await?.1["delivered_at"].is_null());
            // Partial acceptance evidence survives transport retention.
            sqlx::query("DELETE FROM ting_handoffs WHERE delivery_id=$1")
                .bind(left.delivery_id)
                .execute(app.state.store.pool())
                .await?;
            let mut tx = app.state.store.pool().begin().await?;
            sqlx::query("UPDATE ting_handoffs SET accepted_at=clock_timestamp(),ting_id='rollback',ting_created_at=clock_timestamp(),silent=false,lease_id=NULL,lease_owner=NULL,lease_expires_at=NULL WHERE delivery_id=$1")
                .bind(right.delivery_id).execute(&mut *tx).await?;
            let status: String = sqlx::query_scalar("SELECT status::text FROM messages WHERE conversation_id=$1 ORDER BY sequence DESC LIMIT 1")
                .bind(right.conversation_id).fetch_one(&mut *tx).await?;
            assert_eq!(status, "delivered");
            tx.rollback().await?;
            assert!(app.get(&path, &sender_token).await?.1["delivered_at"].is_null());
            accept(&app, right).await?;
        } else {
            let (a, b) = tokio::join!(accept(&app, left), accept(&app, right));
            a?;
            b?;
        }
        let delivered = app.get(&path, &sender_token).await?;
        expect_status(&delivered, StatusCode::OK)?;
        assert!(delivered.1["delivered_at"].is_string());
        if scenario != "read-first" {
            assert!(delivered.1["read_at"].is_null());
        }
        accept(&app, sender).await?;
        accept(&app, right).await?;
        for (index, reader) in [&bob, &cy].into_iter().enumerate() {
            let result = app
                .post(
                    &format!("{path}/receipts"),
                    &app.accounts.token(reader),
                    json!({"status":"read","device_id":"reader"}),
                )
                .await?;
            expect_status(&result, StatusCode::OK)?;
            if index == 0 && scenario != "read-first" {
                assert!(result.1["read_at"].is_null());
            }
        }
        let read = app
            .post(
                &format!("{path}/receipts"),
                &app.accounts.token(&bob),
                json!({"status":"delivered","device_id":"reader"}),
            )
            .await?;
        expect_status(&read, StatusCode::OK)?;
        assert!(read.1["read_at"].is_string());
        assert_eq!(read.1["delivered_at"], delivered.1["delivered_at"]);
        let statuses = app
            .state
            .store
            .claim_ting_deliveries("statuses", 100, Duration::from_secs(60), &context)
            .await?;
        assert_eq!(statuses.len(), if scenario == "read-first" { 1 } else { 2 });
        for status in statuses {
            let body: serde_json::Value = serde_json::from_str(&status.request_body)?;
            if body["data"]["event"] == "message.delivered" {
                assert_eq!(status.originator, Some(ada.actor()));
            }
            accept(&app, &status).await?;
        }
    }
    app.close().await
}

#[tokio::test]
async fn upgrade_backfills_only_proven_original_recipient_acceptances() -> Result<(), BoxError> {
    let app = TestApp::start().await?;
    // Rehearse the old schema in this disposable database, then run the real upgrade.
    sqlx::raw_sql(
        "DROP TRIGGER ting_handoffs_message_delivered ON dm.ting_handoffs;
        DROP FUNCTION dm_private.advance_message_on_ting_acceptance();
        DROP INDEX dm.ting_handoffs_original_message;
        ALTER TABLE dm.message_recipients DROP COLUMN outbound_accepted_at;
        DELETE FROM public._sqlx_migrations WHERE version=52;",
    )
    .execute(app.state.store.pool())
    .await?;
    let ada = TestAccount::carbon("a8K", "c:ada");
    let bob = TestAccount::carbon("Bb2", "c:bob");
    app.accounts.add_accounts(&[&ada, &bob]).await;
    let token = app.accounts.token(&ada);
    for message in ["accepted before upgrade", "recipient still pending"] {
        expect_status(
            &app.post(
                "/api/v1/messages",
                &token,
                json!({"recipient_id":"c:bob","message":message}),
            )
            .await?,
            StatusCode::ACCEPTED,
        )?;
    }
    let context = TingDeliveryContext {
        app_id: "dm".into(),
    };
    let claims = app
        .state
        .store
        .claim_ting_deliveries("before-upgrade", 100, Duration::from_secs(60), &context)
        .await?;
    for claim in claims {
        let body: serde_json::Value = serde_json::from_str(&claim.request_body)?;
        if body["data"]["message_id"] == "000" || claim.target.id.as_str() == ada.uuid {
            accept(&app, &claim).await?;
        }
    }
    let first = "/api/v1/conversations/c:ada::c:bob/messages/000";
    let second = "/api/v1/conversations/c:ada::c:bob/messages/001";
    assert!(app.get(first, &token).await?.1["delivered_at"].is_null());
    app.state.store.migrate().await?;
    let delivered = app.get(first, &token).await?;
    assert!(delivered.1["delivered_at"].is_string());
    assert!(delivered.1["read_at"].is_null());
    assert!(app.get(second, &token).await?.1["delivered_at"].is_null());
    let receipts: i64 = sqlx::query_scalar("SELECT count(*) FROM message_receipts")
        .fetch_one(app.state.store.pool())
        .await?;
    assert_eq!(receipts, 0);
    let statuses = app
        .state
        .store
        .claim_ting_deliveries("after-upgrade", 100, Duration::from_secs(60), &context)
        .await?;
    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0].originator, Some(ada.actor()));
    app.close().await
}
