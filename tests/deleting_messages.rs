//! Who may delete a message: its sender, and the custodian of the Silicon that sent it,
//! whether or not the custodian is in the conversation (`?for=si:x` from the custodian's view
//! of its Silicon limits it to that Silicon's messages). Everyone else is told who sent the
//! message and who may delete it; nobody but the sender edits; an invitee is told to accept.
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

async fn send(
    app: &TestApp,
    token: &str,
    conversation: &str,
    text: &str,
) -> Result<String, BoxError> {
    let sent = app
        .post(
            &format!("/api/v1/conversations/{conversation}/messages"),
            token,
            json!({"message": text}),
        )
        .await?;
    expect_status(&sent, StatusCode::ACCEPTED)?;
    Ok(code(&sent.1))
}

async fn delete(
    app: &TestApp,
    token: &str,
    conversation: &str,
    message: &str,
    query: &str,
) -> Result<(StatusCode, Value), BoxError> {
    app.call(
        Method::DELETE,
        &format!("/api/v1/conversations/{conversation}/messages/{message}{query}"),
        Some(token),
        None,
        &[],
    )
    .await
}

#[tokio::test]
async fn a_custodian_deletes_its_silicons_messages_and_nobody_elses() -> Result<(), BoxError> {
    let app = TestApp::start().await?;
    let ada_account = TestAccount::carbon("a8K", "c:ada");
    let bob_account = TestAccount::carbon("Bb2", "c:bob");
    let cyd_account = TestAccount::carbon("Cy3", "c:cyd");
    let scout_account = TestAccount::silicon("Sc0", "si:scout", &ada_account);
    app.accounts
        .add_accounts(&[&ada_account, &bob_account, &cyd_account, &scout_account])
        .await;
    app.accounts.live_introspection().await;
    let ada = app.accounts.token(&ada_account);
    let bob = app.accounts.token(&bob_account);
    let cyd = app.accounts.token(&cyd_account);
    let scout = app.accounts.token(&scout_account);

    // A group Ada (Scout's custodian) is in, with Scout and Bob.
    let created = app
        .post(
            "/api/v1/groups",
            &ada,
            json!({"name": "Deletes", "member_ids": ["c:bob", "si:scout", "c:cyd"]}),
        )
        .await?;
    expect_status(&created, StatusCode::CREATED)?;
    let group = created.1["id"].as_str().unwrap_or_default().to_owned();
    app.post(&format!("/api/v1/groups/{group}/accept"), &bob, json!(null))
        .await?;
    let from_scout = send(&app, &scout, &group, "scout says").await?;
    let from_bob = send(&app, &bob, &group, "bob says").await?;

    // Bob may not delete Scout's message: he is told who may.
    let (status, refused) = delete(&app, &bob, &group, &from_scout, "").await?;
    assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
    assert_eq!(refused["error"]["code"], "not_message_sender");
    let words = refused["error"]["message"].as_str().unwrap_or_default();
    assert!(
        words.contains("si:scout") && words.contains("c:ada"),
        "names the sender and its custodian: {words}"
    );
    assert_eq!(refused["error"]["details"]["sender"], "si:scout");
    assert_eq!(refused["error"]["details"]["sender_custodian"], "c:ada");
    assert!(
        refused["error"]["hint"]
            .as_str()
            .is_some_and(|hint| hint.contains("ask si:scout or c:ada")),
        "the hint names both who may: {refused}"
    );
    // ... nor edit it.
    let (status, edit) = app
        .call(
            Method::PATCH,
            &format!("/api/v1/conversations/{group}/messages/{from_scout}"),
            Some(&bob),
            Some(json!({"message": "bob was here"})),
            &[],
        )
        .await?;
    assert_eq!(status, StatusCode::FORBIDDEN, "{edit}");
    assert_eq!(edit["error"]["code"], "not_message_sender");

    // Ada, in the group and Scout's custodian, may delete Scout's message without `for` ...
    let (status, tombstone) = delete(&app, &ada, &group, &from_scout, "").await?;
    assert_eq!(status, StatusCode::OK, "{tombstone}");
    assert!(tombstone["deleted_at"].is_string());
    assert_eq!(tombstone["message"], Value::Null);
    // ... and with it, from her view of Scout.
    let second = send(&app, &scout, &group, "scout again").await?;
    let (status, tombstone) = delete(&app, &ada, &group, &second, "?for=si:scout").await?;
    assert_eq!(status, StatusCode::OK, "{tombstone}");
    // Deleting it again is a conflict, as for a sender, in words; a retry of the first
    // delete (its Idempotency-Key) answers the tombstone instead.
    let (status, again) = delete(&app, &ada, &group, &second, "?for=si:scout").await?;
    assert_eq!(status, StatusCode::CONFLICT, "{again}");
    assert_eq!(again["error"]["code"], "conflict");
    assert!(
        again["error"]["message"]
            .as_str()
            .is_some_and(|words| words.contains("already deleted")),
        "{again}"
    );
    assert!(again["error"]["hint"].is_string(), "{again}");
    let key = [("idempotency-key", "custodian-delete-retry".to_owned())];
    let third_try = send(&app, &scout, &group, "scout, to be deleted twice").await?;
    for attempt in 0..2 {
        let (status, retried) = app
            .call(
                Method::DELETE,
                &format!("/api/v1/conversations/{group}/messages/{third_try}?for=si:scout"),
                Some(&ada),
                None,
                &key,
            )
            .await?;
        assert_eq!(status, StatusCode::OK, "attempt {attempt}: {retried}");
        assert!(retried["deleted_at"].is_string());
    }
    // With `for`, only that Silicon's messages: Bob's is refused with words naming both.
    let (status, not_scouts) = delete(&app, &ada, &group, &from_bob, "?for=si:scout").await?;
    assert_eq!(status, StatusCode::FORBIDDEN, "{not_scouts}");
    assert_eq!(not_scouts["error"]["code"], "not_message_sender");
    assert!(
        not_scouts["error"]["message"]
            .as_str()
            .is_some_and(|words| words.contains("c:bob") && words.contains("not si:scout")),
        "{not_scouts}"
    );
    // Without `for`, Bob's message is still not Ada's to delete.
    let (status, _) = delete(&app, &ada, &group, &from_bob, "").await?;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // Someone who is not Scout's custodian cannot use `for`.
    let (status, not_custodian) = delete(&app, &bob, &group, &from_bob, "?for=si:scout").await?;
    assert_eq!(status, StatusCode::FORBIDDEN, "{not_custodian}");
    assert_eq!(not_custodian["error"]["code"], "not_custodian");
    assert!(
        not_custodian["error"]["hint"].is_string(),
        "{not_custodian}"
    );
    // A custodian never edits its Silicon's words, and is told it may delete them instead.
    let third = send(&app, &scout, &group, "scout, third").await?;
    let (status, edit) = app
        .call(
            Method::PATCH,
            &format!("/api/v1/conversations/{group}/messages/{third}"),
            Some(&ada),
            Some(json!({"message": "ada as scout"})),
            &[],
        )
        .await?;
    assert_eq!(status, StatusCode::FORBIDDEN, "{edit}");
    assert_eq!(edit["error"]["code"], "not_message_sender");
    assert!(
        edit["error"]["message"]
            .as_str()
            .is_some_and(|words| words.contains("delete it, never edit it")),
        "{edit}"
    );
    // The sender deletes its own.
    let (status, own) = delete(&app, &bob, &group, &from_bob, "").await?;
    assert_eq!(status, StatusCode::OK, "{own}");

    // An invitee who has not accepted is told so, wherever it reaches into the group.
    for path in [
        format!("/api/v1/conversations/{group}/messages"),
        format!("/api/v1/conversations/{group}/draft"),
    ] {
        let (status, pending) = app.call(Method::GET, &path, Some(&cyd), None, &[]).await?;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}: {pending}");
        assert_eq!(pending["error"]["code"], "invitation_pending");
        assert!(pending["error"]["hint"].is_string());
    }

    // A conversation Ada is not in: her custodian's view of Scout deletes there too.
    let direct = app
        .post(
            "/api/v1/messages",
            &scout,
            json!({"recipient_id": "c:bob", "message": "just for bob"}),
        )
        .await?;
    expect_status(&direct, StatusCode::ACCEPTED)?;
    let pair = direct.1["conversation_id"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert_eq!(pair, "c:bob::si:scout");
    let reply = send(&app, &bob, &pair, "thanks").await?;
    let (status, tombstone) = delete(&app, &ada, &pair, &code(&direct.1), "?for=si:scout").await?;
    assert_eq!(status, StatusCode::OK, "{tombstone}");
    let (status, refused) = delete(&app, &ada, &pair, &reply, "?for=si:scout").await?;
    assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
    let (status, refused) = delete(&app, &ada, &pair, &reply, "").await?;
    assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
    assert_eq!(refused["error"]["details"]["sender"], "c:bob");
    assert_eq!(refused["error"]["details"]["sender_custodian"], Value::Null);
    app.close().await
}
