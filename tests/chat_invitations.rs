//! Account binding, signup handoff, atomic acceptance, and durable email retries.
#![allow(
    clippy::too_many_lines,
    reason = "each test reads as one complete invitation scenario"
)]
mod support;
use axum::http::{Method, StatusCode};
use serde_json::{Value, json};
use support::{
    BoxError, TestDatabase,
    accounts::TestAccount,
    app::{TestApp, expect_status},
};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, method, path},
};

async fn invite(
    app: &TestApp,
    token: &str,
    to: &str,
    partner: Option<&str>,
    key: &str,
) -> Result<Value, BoxError> {
    let result = app
        .call(
            Method::POST,
            "/api/v1/chat-invitations",
            Some(token),
            Some(json!({"to":to,"with":partner})),
            &[("idempotency-key", key.into())],
        )
        .await?;
    expect_status(&result, StatusCode::CREATED)?;
    Ok(result.1)
}

async fn answer(
    app: &TestApp,
    token: &str,
    id: &str,
    decision: &str,
    key: &str,
) -> Result<(StatusCode, Value), BoxError> {
    app.call(
        Method::POST,
        &format!("/api/v1/chat-invitations/{id}/{decision}"),
        Some(token),
        Some(json!({})),
        &[("idempotency-key", key.into())],
    )
    .await
}

async fn email_proof(
    app: &TestApp,
    account: &TestAccount,
    token: &str,
    email: &str,
    verified: bool,
    priority: u8,
) {
    Mock::given(method("GET")).and(path("/v1/userinfo")).and(header("authorization", format!("Bearer {token}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "uuid":account.uuid,"kind":account.kind,"id":account.id,"display_name":account.display_name,
            "pfp_url":"","email":email,"email_verified":verified
        }))).with_priority(priority).mount(&app.accounts.server).await;
}

#[tokio::test]
async fn ids_are_private_pending_and_concurrent_acceptance_reuses_one_chat() -> Result<(), BoxError>
{
    let app = TestApp::start().await?;
    let ada = TestAccount::carbon("ada", "c:ada");
    let bo = TestAccount::carbon("bob", "c:bo");
    let eve = TestAccount::carbon("eve", "c:eve");
    for account in [&ada, &bo, &eve] {
        app.accounts.add_account(account).await;
    }
    let a = app.accounts.token(&ada);
    let b = app.accounts.token(&bo);
    let e = app.accounts.token(&eve);
    let invitation = invite(&app, &a, "c:bo", None, "invite-bo-1").await?;
    let id = invitation["id"].as_str().ok_or("id")?;
    assert_eq!(invitation["status"], "pending");
    assert_eq!(invitation["mail_status"], "none");
    assert_eq!(invitation["inviter"]["id"], "c:ada");
    assert_eq!(invitation["recipient"]["id"], "c:bo");
    assert!(invitation["conversation_id"].is_null());
    let again = invite(&app, &a, "c:bo", None, "invite-bo-1").await?;
    assert_eq!(again["id"], id);
    assert_eq!(
        invite(&app, &a, "c:bo", None, "invite-bo-2").await?["id"],
        id
    );
    let changed = app
        .call(
            Method::POST,
            "/api/v1/chat-invitations",
            Some(&a),
            Some(json!({"to":"c:eve"})),
            &[("idempotency-key", "invite-bo-1".into())],
        )
        .await?;
    expect_status(&changed, StatusCode::CONFLICT)?;
    let url = format!("/api/v1/chat-invitations/{id}");
    expect_status(
        &app.call(Method::GET, &url, None, None, &[]).await?,
        StatusCode::UNAUTHORIZED,
    )?;
    expect_status(
        &app.call(Method::GET, &url, Some(&e), None, &[]).await?,
        StatusCode::FORBIDDEN,
    )?;
    expect_status(
        &answer(&app, &e, id, "accept", "wrong-account").await?,
        StatusCode::FORBIDDEN,
    )?;
    let listed = app
        .call(Method::GET, "/api/v1/chat-invitations", Some(&b), None, &[])
        .await?;
    expect_status(&listed, StatusCode::OK)?;
    assert_eq!(listed.1["items"][0]["id"], id);
    let (one, two) = tokio::join!(
        answer(&app, &b, id, "accept", "answer-one"),
        answer(&app, &b, id, "accept", "answer-two")
    );
    let one = one?;
    let two = two?;
    expect_status(&one, StatusCode::OK)?;
    expect_status(&two, StatusCode::OK)?;
    assert_eq!(one.1["conversation_id"], two.1["conversation_id"]);
    assert!(
        one.1["conversation_id"]
            .as_str()
            .ok_or("conversation")?
            .contains("c:ada")
    );
    expect_status(
        &answer(&app, &b, id, "decline", "answer-one").await?,
        StatusCode::CONFLICT,
    )?;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM conversations WHERE NOT is_group")
        .fetch_one(app.state.store.pool())
        .await?;
    assert_eq!(count, 1);
    app.close().await
}

#[tokio::test]
async fn email_invitee_can_be_created_later_but_must_prove_the_address() -> Result<(), BoxError> {
    let app = TestApp::start().await?;
    let ada = TestAccount::carbon("ada", "c:ada");
    app.accounts.add_account(&ada).await;
    let invitation = invite(
        &app,
        &app.accounts.token(&ada),
        "New+dm@Example.test",
        None,
        "invite-new-email",
    )
    .await?;
    let id = invitation["id"].as_str().ok_or("id")?;
    assert_eq!(invitation["email"], "new+dm@example.test");
    assert_eq!(invitation["mail_status"], "simulated");
    // This Carbon did not exist at invitation time. Accounts supplies the identity only after signup.
    let new = TestAccount::carbon("new", "c:new");
    app.accounts.add_account(&new).await;
    let token = app
        .accounts
        .token_with(&new, &json!({"scope":"profile email"}));
    email_proof(&app, &new, &token, "new+dm@example.test", false, 9).await;
    expect_status(
        &answer(&app, &token, id, "accept", "email-answer").await?,
        StatusCode::FORBIDDEN,
    )?;
    email_proof(&app, &new, &token, "wrong@example.test", true, 8).await;
    expect_status(
        &answer(&app, &token, id, "accept", "email-answer").await?,
        StatusCode::FORBIDDEN,
    )?;
    email_proof(&app, &new, &token, "new+dm@example.test", true, 7).await;
    let listed = app
        .call(
            Method::GET,
            "/api/v1/chat-invitations",
            Some(&token),
            None,
            &[],
        )
        .await?;
    assert_eq!(listed.1["items"][0]["id"], id);
    let accepted = answer(&app, &token, id, "accept", "email-answer").await?;
    expect_status(&accepted, StatusCode::OK)?;
    assert_eq!(accepted.1["status"], "accepted");
    assert!(accepted.1["conversation_id"].is_string());
    // Reopening after changing primary email still returns this account's conversation.
    email_proof(&app, &new, &token, "changed@example.test", true, 6).await;
    assert_eq!(
        answer(&app, &token, id, "accept", "email-reopen").await?.1["conversation_id"],
        accepted.1["conversation_id"]
    );
    let other = TestAccount::carbon("oth", "c:other");
    app.accounts.add_account(&other).await;
    let other_token = app
        .accounts
        .token_with(&other, &json!({"scope":"profile email"}));
    email_proof(&app, &other, &other_token, "new+dm@example.test", true, 1).await;
    expect_status(
        &answer(&app, &other_token, id, "accept", "email-new-owner").await?,
        StatusCode::FORBIDDEN,
    )?;
    app.close().await
}

#[tokio::test]
async fn only_current_custodian_can_invite_to_a_silicon_and_silicons_can_receive()
-> Result<(), BoxError> {
    let app = TestApp::start().await?;
    let ada = TestAccount::carbon("ada", "c:ada");
    let bo = TestAccount::carbon("bob", "c:bo");
    let scout = TestAccount::silicon("sct", "si:scout", &ada);
    for account in [&ada, &bo, &scout] {
        app.accounts.add_account(account).await;
    }
    let a = app.accounts.token(&ada);
    let b = app.accounts.token(&bo);
    let s = app.accounts.token(&scout);
    let denied = app
        .call(
            Method::POST,
            "/api/v1/chat-invitations",
            Some(&b),
            Some(json!({"to":"c:ada","with":"si:scout"})),
            &[("idempotency-key", "wrong-custodian".into())],
        )
        .await?;
    expect_status(&denied, StatusCode::FORBIDDEN)?;
    let invitation = invite(&app, &a, "c:bo", Some("si:scout"), "custodian-invite").await?;
    let id = invitation["id"].as_str().ok_or("id")?;
    let accepted = answer(&app, &b, id, "accept", "custodian-accept").await?;
    expect_status(&accepted, StatusCode::OK)?;
    assert_eq!(accepted.1["partner"]["id"], "si:scout");
    let invitation = invite(&app, &b, "si:scout", None, "silicon-invite").await?;
    let id = invitation["id"].as_str().ok_or("id")?;
    expect_status(
        &answer(&app, &a, id, "accept", "custodian-cannot-accept").await?,
        StatusCode::FORBIDDEN,
    )?;
    assert_eq!(
        answer(&app, &s, id, "accept", "silicon-accept").await?.1["conversation_id"],
        accepted.1["conversation_id"]
    );
    let pending = invite(&app, &a, "c:bo", Some("si:scout"), "before-transfer").await?;
    let id = pending["id"].as_str().ok_or("id")?;
    let moved = TestAccount::silicon("sct", "si:scout", &bo);
    app.accounts.serve_only(&[&ada, &bo, &moved]).await;
    // A pending invitation cannot outlive the sender's authority over its partner.
    expect_status(
        &answer(&app, &b, id, "accept", "after-transfer").await?,
        StatusCode::FORBIDDEN,
    )?;
    expect_status(
        &answer(&app, &b, id, "decline", "decline-transferred").await?,
        StatusCode::OK,
    )?;
    app.close().await
}

#[tokio::test]
async fn decline_expiry_limits_and_simulation_do_not_create_chats_or_send_mail()
-> Result<(), BoxError> {
    let app = TestApp::start().await?;
    let ada = TestAccount::carbon("ada", "c:ada");
    let bo = TestAccount::carbon("bob", "c:bo");
    for account in [&ada, &bo] {
        app.accounts.add_account(account).await;
    }
    let a = app.accounts.token(&ada);
    let b = app.accounts.token(&bo);
    let invitation = invite(&app, &a, "c:bo", None, "decline-invite").await?;
    let id = invitation["id"].as_str().ok_or("id")?;
    expect_status(
        &answer(&app, &b, id, "decline", "decline-answer").await?,
        StatusCode::OK,
    )?;
    expect_status(
        &answer(&app, &b, id, "accept", "declined-accept").await?,
        StatusCode::GONE,
    )?;
    let invitation = invite(&app, &a, "c:bo", None, "expired-invite").await?;
    let id = invitation["id"].as_str().ok_or("id")?;
    sqlx::query("UPDATE chat_invitations SET expires_at=now()-interval '1 minute' WHERE id=$1")
        .bind(uuid::Uuid::parse_str(id)?)
        .execute(app.state.store.pool())
        .await?;
    expect_status(
        &answer(&app, &b, id, "accept", "expired-answer").await?,
        StatusCode::GONE,
    )?;
    for n in 0..18 {
        invite(
            &app,
            &a,
            &format!("person{n}@example.test"),
            None,
            &format!("email-invite-{n}"),
        )
        .await?;
    }
    let limited = app
        .call(
            Method::POST,
            "/api/v1/chat-invitations",
            Some(&a),
            Some(json!({"to":"last@example.test"})),
            &[("idempotency-key", "limit-invite".into())],
        )
        .await?;
    expect_status(&limited, StatusCode::TOO_MANY_REQUESTS)?;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM conversations")
        .fetch_one(app.state.store.pool())
        .await?;
    assert_eq!(count, 0);
    app.close().await
}

#[tokio::test]
async fn postmark_failure_stays_queued_and_retry_sends_minimal_untracked_email()
-> Result<(), BoxError> {
    let postmark = MockServer::start().await;
    let endpoint = format!("{}/email", postmark.uri()).parse()?;
    let app = TestApp::start_configured(TestDatabase::start().await?, "", |settings| {
        settings.environment = silicon_dm::config::RuntimeEnvironment::Production;
        settings.reporting = Some(silicon_dm::config::ReportingSettings {
            endpoint,
            token: "test-postmark".into(),
        });
    })
    .await?;
    let ada = TestAccount::carbon("ada", "c:ada");
    app.accounts.add_account(&ada).await;
    let invitation = invite(
        &app,
        &app.accounts.token(&ada),
        "new@example.test",
        None,
        "mail-invitation",
    )
    .await?;
    assert_eq!(invitation["mail_status"], "queued");
    Mock::given(method("POST"))
        .and(path("/email"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&postmark)
        .await;
    silicon_dm::invitation_mail::deliver_one(&app.state).await?;
    let status: (String, i32) =
        sqlx::query_as("SELECT mail_status,mail_attempts FROM chat_invitations")
            .fetch_one(app.state.store.pool())
            .await?;
    assert_eq!(status, ("queued".into(), 1));
    postmark.reset().await;
    Mock::given(method("POST"))
        .and(path("/email"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"ErrorCode":0,"MessageID":"mail-id"})),
        )
        .expect(1)
        .mount(&postmark)
        .await;
    sqlx::query("UPDATE chat_invitations SET next_attempt_at=now()")
        .execute(app.state.store.pool())
        .await?;
    silicon_dm::invitation_mail::deliver_one(&app.state).await?;
    silicon_dm::invitation_mail::deliver_one(&app.state).await?;
    let requests = postmark.received_requests().await.ok_or("requests")?;
    let mail: Value = serde_json::from_slice(&requests[0].body)?;
    assert_eq!(mail["To"], "new@example.test");
    assert_eq!(mail["TrackOpens"], false);
    assert_eq!(mail["TrackLinks"], "None");
    assert!(
        mail["TextBody"]
            .as_str()
            .ok_or("text")?
            .contains(invitation["url"].as_str().ok_or("url")?)
    );
    assert_eq!(mail["Subject"], "c:ada invited you to chat");
    app.close().await
}
