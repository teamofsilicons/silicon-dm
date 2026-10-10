//! Ting delivery after the move to Silicon Accounts. Off by default: handoffs are kept
//! and marked discarded while clients sync. When configured, DM sends each handoff once
//! with its own App verification proof and never fabricates a receipt; requests prepared
//! before the move, and recipients that never got an account, are discarded; recipients
//! are enrolled with a proof of their own agreement whose refresh token is kept encrypted.
#![allow(
    clippy::too_many_lines,
    reason = "each test is one scenario, read top to bottom"
)]
mod support;

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use axum::http::StatusCode;
use serde_json::{Value, json};
use silicon_dm::{
    infrastructure::{
        postgres::TingDeliveryClaim,
        ting::{TingAcceptance, TingFailure, TingPublisher},
    },
    worker::TingDeliveryWorker,
};
use support::{
    BoxError, TestDatabase,
    accounts::TestAccount,
    app::{TestApp, expect_status},
};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_partial_json, header, method, path},
};

/// Records what would have gone to Ting, answering as `outcome` says.
struct Recording {
    sent: Mutex<Vec<(String, Value)>>,
    reject_with: Option<&'static str>,
}

impl Recording {
    fn accepting() -> Arc<Self> {
        Arc::new(Self {
            sent: Mutex::new(Vec::new()),
            reject_with: None,
        })
    }

    fn rejecting(code: &'static str) -> Arc<Self> {
        Arc::new(Self {
            sent: Mutex::new(Vec::new()),
            reject_with: Some(code),
        })
    }

    fn sent(&self) -> Vec<(String, Value)> {
        self.sent
            .lock()
            .map(|sent| sent.clone())
            .unwrap_or_default()
    }
}

#[async_trait]
impl TingPublisher for Recording {
    async fn publish(&self, claim: &TingDeliveryClaim) -> Result<TingAcceptance, TingFailure> {
        let body = serde_json::from_str(&claim.request_body).unwrap_or(Value::Null);
        if let Ok(mut sent) = self.sent.lock() {
            sent.push((claim.target.id.to_string(), body));
        }
        match self.reject_with {
            Some(code) => Err(TingFailure::Rejected(code.to_owned())),
            None => Ok(TingAcceptance {
                id: format!("ting-{}", claim.delivery_id),
                created_at: time::OffsetDateTime::now_utc(),
                silent: false,
            }),
        }
    }
}

async fn count(app: &TestApp, sql: &str) -> Result<i64, BoxError> {
    Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_owned()))
        .fetch_one(app.state.store.pool())
        .await?)
}

async fn people(app: &TestApp) -> (TestAccount, TestAccount, TestAccount) {
    let ada = TestAccount::carbon("a8K", "c:ada");
    let bob = TestAccount::carbon("Bb2", "c:bob");
    let cos = TestAccount::silicon("K1E", "si:cos", &ada);
    app.accounts.add_accounts(&[&ada, &bob, &cos]).await;
    (ada, bob, cos)
}

#[tokio::test]
async fn without_ting_handoffs_are_kept_and_marked_discarded() -> Result<(), BoxError> {
    let app = TestApp::start().await?;
    let (ada, bob, _) = people(&app).await;
    let sent = app
        .post(
            "/api/v1/messages",
            &app.accounts.token(&ada),
            json!({"recipient_id": "c:bob", "message": "hi"}),
        )
        .await?;
    expect_status(&sent, StatusCode::ACCEPTED)?;
    let pending = count(
        &app,
        "SELECT count(*) FROM ting_handoffs WHERE accepted_at IS NULL AND discarded_at IS NULL",
    )
    .await?;
    assert!(pending > 0);
    let discarded = TingDeliveryWorker::new(app.state.clone(), None)
        .process_once()
        .await?;
    assert_eq!(i64::try_from(discarded)?, pending);
    assert_eq!(
        count(
            &app,
            "SELECT count(*) FROM ting_handoffs WHERE discard_reason = 'ting_not_configured'"
        )
        .await?,
        pending
    );
    assert_eq!(
        count(&app, "SELECT count(*) FROM ting_handoffs").await?,
        pending,
        "nothing is deleted"
    );
    let messages = app
        .get(
            "/api/v1/conversations/c:ada::c:bob/messages",
            &app.accounts.token(&bob),
        )
        .await?;
    assert!(
        messages.1["items"][0]["delivered_at"].is_null(),
        "no receipt is invented"
    );
    let delivery = app
        .get("/api/v1/delivery", &app.accounts.token(&bob))
        .await?;
    assert_eq!(delivery.1["transport"], "sync", "{}", delivery.1);
    app.close().await
}

#[tokio::test]
async fn with_ting_each_handoff_is_sent_once_and_never_becomes_a_receipt() -> Result<(), BoxError> {
    let app = TestApp::start().await?;
    let (ada, bob, _) = people(&app).await;
    let ada_token = app.accounts.token(&ada);
    expect_status(
        &app.post(
            "/api/v1/messages",
            &ada_token,
            json!({"recipient_id": "c:bob", "message": "hi"}),
        )
        .await?,
        StatusCode::ACCEPTED,
    )?;
    expect_status(
        &app.post(
            "/api/v1/messages",
            &ada_token,
            json!({"recipient_id": "deliberate@si:cos", "message": "think"}),
        )
        .await?,
        StatusCode::ACCEPTED,
    )?;
    let recording = Recording::accepting();
    let worker = TingDeliveryWorker::new(app.state.clone(), Some(recording.clone()));
    let processed = worker.process_once().await?;
    assert!(
        processed >= 4,
        "both messages reach both sides: {processed}"
    );
    let sent = recording.sent();
    let to_bob = sent
        .iter()
        .find(|(target, _)| target == "Bb2")
        .map(|(_, body)| body.clone())
        .unwrap_or_default();
    assert_eq!(to_bob["type"], "dm.sync.changed");
    assert_eq!(to_bob["for"], json!({"uuid": "Bb2", "id": "c:bob"}));
    assert_eq!(to_bob["key"], to_bob["data"]["delivery_id"]);
    assert_eq!(to_bob["data"]["schema_version"], 2);
    assert_eq!(to_bob["data"]["conversation_id"], "c:ada::c:bob");
    assert_eq!(to_bob["data"]["message_id"], "000");
    assert_eq!(to_bob["data"]["event"], "message.created");
    assert!(to_bob.get("org_id").is_none());
    let to_cos = sent
        .iter()
        .find(|(target, _)| target == "K1E")
        .map(|(_, body)| body.clone())
        .unwrap_or_default();
    assert_eq!(to_cos["metadata"], json!({"isi": "deliberate"}), "{to_cos}");
    assert_eq!(
        count(
            &app,
            "SELECT count(*) FROM ting_handoffs WHERE accepted_at IS NULL"
        )
        .await?,
        0
    );
    assert_eq!(
        worker.process_once().await?,
        0,
        "accepted handoffs are not sent again"
    );
    let messages = app
        .get(
            "/api/v1/conversations/c:ada::c:bob/messages",
            &app.accounts.token(&bob),
        )
        .await?;
    assert!(
        messages.1["items"][0]["delivered_at"].is_null(),
        "Ting acceptance is not a DM receipt"
    );

    // A refused send stays pending for a later attempt, with the same bytes.
    expect_status(
        &app.post(
            "/api/v1/messages",
            &ada_token,
            json!({"recipient_id": "c:bob", "message": "again"}),
        )
        .await?,
        StatusCode::ACCEPTED,
    )?;
    let failing = Recording::rejecting("ting_unavailable");
    TingDeliveryWorker::new(app.state.clone(), Some(failing.clone()))
        .process_once()
        .await?;
    assert_eq!(
        count(&app, "SELECT count(*) FROM ting_handoffs WHERE accepted_at IS NULL AND discarded_at IS NULL AND attempt_count = 1 AND next_attempt_at > clock_timestamp()").await?,
        i64::try_from(failing.sent().len())?
    );
    app.close().await
}

#[tokio::test]
async fn requests_prepared_before_the_move_and_recipients_without_accounts_are_discarded()
-> Result<(), BoxError> {
    let app = TestApp::start_on(TestDatabase::upgraded_from_0038().await?, "").await?;
    // One legacy handoff was already prepared by the IAM-era worker.
    sqlx::query(
        "UPDATE ting_handoffs SET request_body = '{\"type\":\"dm.sync.changed\",\"org_id\":\"tos\",\"for\":\"si:cos\",\"key\":\"k\",\"data\":{}}'
         WHERE delivery_id = '01a122d0-eaa2-7083-954d-760c61e79cd9'",
    )
    .execute(app.state.store.pool())
    .await?;
    let recording = Recording::accepting();
    let worker = TingDeliveryWorker::new(app.state.clone(), Some(recording.clone()));
    while worker.process_once().await? > 0 {}
    assert!(
        recording.sent().is_empty(),
        "nothing legacy is sent: {:?}",
        recording.sent()
    );
    assert_eq!(
        count(&app, "SELECT count(*) FROM ting_handoffs WHERE discard_reason = 'prepared_before_accounts_cutover'").await?,
        1
    );
    assert!(
        count(
            &app,
            "SELECT count(*) FROM ting_handoffs WHERE discard_reason = 'recipient_without_account'"
        )
        .await?
            > 0
    );
    app.close().await
}

async fn wait_for(app: &TestApp, sql: &str, expected: i64) -> Result<(), BoxError> {
    for _ in 0..100 {
        if count(app, sql).await? == expected {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Err(format!("timed out waiting for `{sql}` = {expected}").into())
}

#[tokio::test]
async fn recipients_are_enrolled_with_their_own_proof_and_reenrolled_from_the_stored_one()
-> Result<(), BoxError> {
    let ting = MockServer::start().await;
    for (proof, subscription) in [("sap_user", "sub_1"), ("sap_user_2", "sub_2")] {
        Mock::given(method("POST"))
            .and(path("/v1/subscriptions"))
            .and(header("authorization", format!("Proof {proof}").as_str()))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": subscription})))
            .mount(&ting)
            .await;
    }
    let app = TestApp::start_with_ting(ting.uri().parse()?).await?;
    let (ada, _, _) = people(&app).await;
    app.accounts.issue_proofs().await;
    let expires = (time::OffsetDateTime::now_utc() + time::Duration::minutes(10))
        .format(&time::format_description::well_known::Rfc3339)?;
    Mock::given(method("POST"))
        .and(path("/v1/proofs/refresh"))
        .and(body_partial_json(json!({"proof_refresh_token": "sapr_refresh"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "proof_id": "prf_2", "kind": "user_verification", "proof_token": "sap_user_2",
            "expires_at": expires, "proof_refresh_token": "sapr_refresh_2", "refresh_expires_at": expires,
        })))
        .mount(&app.accounts.server)
        .await;

    // Ada's first request enrols her in the background.
    expect_status(
        &app.get("/api/v1/me", &app.accounts.token(&ada)).await?,
        StatusCode::OK,
    )?;
    wait_for(&app, "SELECT count(*) FROM ting_automatic_enrollments WHERE actor_id = 'a8K' AND subscription_id = 'sub_1'", 1).await?;
    let subscriptions = ting.received_requests().await.unwrap_or_default();
    let body: Value = serde_json::from_slice(&subscriptions[0].body)?;
    assert_eq!(
        body,
        json!({"app_id": "dm", "for": {"uuid": "a8K", "id": "c:ada"}})
    );
    let (nonce, sealed): (Vec<u8>, Vec<u8>) =
        sqlx::query_as("SELECT nonce, ciphertext FROM ting_proof_grants WHERE actor_id = 'a8K'")
            .fetch_one(app.state.store.pool())
            .await?;
    assert_eq!(nonce.len(), 12);
    assert!(
        !sealed.windows(12).any(|window| window == b"sapr_refresh"),
        "stored encrypted"
    );

    // Ting forgot her: the worker re-enrols from the stored proof, without a live session.
    expect_status(
        &app.post(
            "/api/v1/messages",
            &app.accounts.token(&ada),
            json!({"recipient_id": "c:bob", "message": "hi"}),
        )
        .await?,
        StatusCode::ACCEPTED,
    )?;
    TingDeliveryWorker::new(
        app.state.clone(),
        Some(Recording::rejecting("recipient_not_registered")),
    )
    .process_once()
    .await?;
    wait_for(
        &app,
        "SELECT count(*) FROM ting_proof_grants WHERE actor_id = 'a8K' AND proof_id = 'prf_2'",
        1,
    )
    .await?;
    for _ in 0..100 {
        if ting.received_requests().await.unwrap_or_default().len() >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let requests = ting.received_requests().await.unwrap_or_default();
    assert!(
        requests.iter().any(|request| request
            .headers
            .get("authorization")
            .is_some_and(|value| value == "Proof sap_user_2")),
        "re-subscribed with the refreshed proof"
    );
    app.close().await
}
