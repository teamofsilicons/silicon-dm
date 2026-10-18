//! Activity about one conversation: what a device is doing (typing, recording…) is said about
//! the conversation it does it in (`renew_presence`'s `conversation_id`) and shown only to
//! accounts in that conversation. A scoped read (`?conversation=`) answers activity about that
//! conversation only; an unscoped read never shows activity about a conversation the caller is
//! not in; a custodian sees what its Silicon would (`?for=`); clearing the activity clears its
//! conversation; and conversations the account is not in are refused with words.
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

struct People {
    ada: TestAccount,
    bob: TestAccount,
    cy: TestAccount,
    dee: TestAccount,
    scout: TestAccount,
}

async fn people(app: &TestApp) -> People {
    let ada = TestAccount::carbon("Pa1", "c:ada");
    let bob = TestAccount::carbon("Pb2", "c:bob");
    let cy = TestAccount::carbon("Pc3", "c:cyra");
    let dee = TestAccount::carbon("Pd4", "c:dee");
    let scout = TestAccount::silicon("Ps5", "si:scout", &dee);
    app.accounts
        .add_accounts(&[&ada, &bob, &cy, &dee, &scout])
        .await;
    app.accounts.live_introspection().await;
    People {
        ada,
        bob,
        cy,
        dee,
        scout,
    }
}

/// Sends one message and returns the conversation's address.
async fn converse(app: &TestApp, token: &str, to: &str) -> Result<String, BoxError> {
    let sent = app
        .post(
            "/api/v1/messages",
            token,
            json!({"recipient_id": to, "message": "hello"}),
        )
        .await?;
    expect_status(&sent, StatusCode::ACCEPTED)?;
    sent.1["conversation_id"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("no conversation in {}", sent.1).into())
}

async fn renew(
    app: &TestApp,
    token: &str,
    device: &str,
    data: Value,
) -> Result<(StatusCode, Value), BoxError> {
    app.call(
        Method::PUT,
        &format!("/api/v1/presence/devices/{device}"),
        Some(token),
        Some(data),
        &[],
    )
    .await
}

/// What `token` sees of `account`'s presence with `query` (`""`, `?conversation=…`, …).
async fn seen(
    app: &TestApp,
    token: &str,
    account: &str,
    query: &str,
) -> Result<(StatusCode, Value), BoxError> {
    app.get(&format!("/api/v1/presence/{account}{query}"), token)
        .await
}

async fn activity(
    app: &TestApp,
    token: &str,
    account: &str,
    query: &str,
) -> Result<Value, BoxError> {
    let presence = seen(app, token, account, query).await?;
    expect_status(&presence, StatusCode::OK)?;
    assert_eq!(presence.1["availability"], "online", "{}", presence.1);
    Ok(presence.1["activity"].clone())
}

#[tokio::test]
async fn activity_is_shown_only_in_the_conversation_it_is_about() -> Result<(), BoxError> {
    let app = TestApp::start().await?;
    let p = people(&app).await;
    let ada = app.accounts.token(&p.ada);
    let bob = app.accounts.token(&p.bob);
    let cy = app.accounts.token(&p.cy);
    let with_bob = converse(&app, &ada, "c:bob").await?;
    let with_cy = converse(&app, &ada, "c:cyra").await?;
    let bob_and_cy = converse(&app, &bob, "c:cyra").await?;

    // The service says it scopes activity, with the field and the query to use.
    let contracts = app.get("/api/v1/contracts", &ada).await?;
    let feature = &contracts.1["features"]["presence_activity"];
    assert_eq!(feature["scope"], "conversation", "{}", contracts.1);
    assert_eq!(feature["renew_field"], "conversation_id");
    assert_eq!(feature["query"], "conversation");

    // Ada types to Bob. The lease names the conversation by its address.
    let typing = renew(
        &app,
        &ada,
        "ada-laptop",
        json!({"activity": "typing", "conversation_id": with_bob}),
    )
    .await?;
    expect_status(&typing, StatusCode::OK)?;
    assert_eq!(
        typing.1["conversation_id"],
        with_bob.as_str(),
        "{}",
        typing.1
    );
    assert!(typing.1["activity_expires_at"].is_string());

    // Bob sees it in their conversation, scoped or not.
    let scoped = format!("?conversation={with_bob}");
    assert_eq!(activity(&app, &bob, "c:ada", &scoped).await?, "typing");
    assert_eq!(activity(&app, &bob, "c:ada", "").await?, "typing");
    // Cy shares another conversation with Ada: Ada is online for Cy, but what she types to
    // Bob is not Cy's to see, scoped to their own conversation or not.
    assert_eq!(activity(&app, &cy, "c:ada", "").await?, Value::Null);
    assert_eq!(
        activity(&app, &cy, "c:ada", &format!("?conversation={with_cy}")).await?,
        Value::Null
    );
    // Asking about a conversation Cy is not in is refused, as everywhere.
    let outsider = seen(&app, &cy, "c:ada", &scoped).await?;
    assert_eq!(outsider.0, StatusCode::NOT_FOUND, "{}", outsider.1);
    assert_eq!(outsider.1["error"]["details"]["field"], "conversation");
    assert!(
        outsider.1["error"]["hint"]
            .as_str()
            .is_some_and(|hint| hint.contains("leave conversation out")),
        "{}",
        outsider.1
    );
    // A conversation the caller is in, but that Ada is not in, shows none of her activity.
    assert_eq!(
        activity(&app, &bob, "c:ada", &format!("?conversation={bob_and_cy}")).await?,
        Value::Null
    );
    // The conversation may be named by its UUID too (here: the lease's own words).
    let uuid: String = sqlx::query_scalar(
        "SELECT activity_conversation_id::text FROM client_presence_leases WHERE device_id = 'ada-laptop'",
    )
    .fetch_one(app.state.store.pool())
    .await?;
    assert_eq!(
        activity(&app, &bob, "c:ada", &format!("?conversation={uuid}")).await?,
        "typing"
    );

    // Now Ada types to Cy on another device: Cy sees that one, Bob still sees his.
    expect_status(
        &renew(
            &app,
            &ada,
            "ada-phone",
            json!({"activity": "recording_voice", "conversation_id": with_cy}),
        )
        .await?,
        StatusCode::OK,
    )?;
    assert_eq!(
        activity(&app, &cy, "c:ada", &format!("?conversation={with_cy}")).await?,
        "recording_voice"
    );
    assert_eq!(activity(&app, &cy, "c:ada", "").await?, "recording_voice");
    assert_eq!(activity(&app, &bob, "c:ada", &scoped).await?, "typing");
    // Unscoped, Bob sees the newest activity he may see: still the one about his conversation.
    assert_eq!(activity(&app, &bob, "c:ada", "").await?, "typing");

    // Clearing the activity clears its conversation.
    let cleared = renew(&app, &ada, "ada-laptop", json!({"activity": null})).await?;
    expect_status(&cleared, StatusCode::OK)?;
    assert_eq!(cleared.1["conversation_id"], Value::Null, "{}", cleared.1);
    assert_eq!(cleared.1["activity_expires_at"], Value::Null);
    let stored: Option<String> = sqlx::query_scalar(
        "SELECT activity_conversation_id::text FROM client_presence_leases WHERE device_id = 'ada-laptop'",
    )
    .fetch_one(app.state.store.pool())
    .await?;
    assert_eq!(stored, None);
    assert_eq!(activity(&app, &bob, "c:ada", &scoped).await?, Value::Null);
    assert_eq!(activity(&app, &bob, "c:ada", "").await?, Value::Null);
    // A conversation named without an activity is not kept either.
    let idle = renew(
        &app,
        &ada,
        "ada-laptop",
        json!({"activity": null, "conversation_id": with_bob}),
    )
    .await?;
    expect_status(&idle, StatusCode::OK)?;
    assert_eq!(idle.1["conversation_id"], Value::Null);

    // Activity about no conversation (older clients, the CLI without --conversation) shows
    // wherever Ada is seen unscoped, and in no conversation when asked for one.
    expect_status(
        &renew(&app, &ada, "ada-laptop", json!({"activity": "typing"})).await?,
        StatusCode::OK,
    )?;
    assert_eq!(activity(&app, &bob, "c:ada", &scoped).await?, Value::Null);
    assert_eq!(activity(&app, &bob, "c:ada", "").await?, "typing");

    // Closing a device clears what it was doing; the other device's activity stays.
    let closed = app
        .call(
            Method::DELETE,
            "/api/v1/presence/devices/ada-phone",
            Some(&ada),
            None,
            &[],
        )
        .await?;
    assert_eq!(closed.0, StatusCode::NO_CONTENT, "{}", closed.1);
    assert_eq!(
        activity(&app, &cy, "c:ada", &format!("?conversation={with_cy}")).await?,
        Value::Null
    );
    let stored: Option<String> = sqlx::query_scalar(
        "SELECT activity_conversation_id::text FROM client_presence_leases WHERE device_id = 'ada-phone'",
    )
    .fetch_one(app.state.store.pool())
    .await?;
    assert_eq!(stored, None);
    app.close().await
}

#[tokio::test]
async fn a_custodian_sees_activity_as_its_silicon_would() -> Result<(), BoxError> {
    let app = TestApp::start().await?;
    let p = people(&app).await;
    let bob = app.accounts.token(&p.bob);
    let dee = app.accounts.token(&p.dee);
    let scout = app.accounts.token(&p.scout);
    // Scout (Dee's Silicon) talks with Bob; Dee talks with Bob herself too.
    let scout_and_bob = converse(&app, &scout, "c:bob").await?;
    let dee_and_bob = converse(&app, &dee, "c:bob").await?;
    expect_status(
        &renew(
            &app,
            &bob,
            "bob-laptop",
            json!({"activity": "typing", "conversation_id": scout_and_bob}),
        )
        .await?,
        StatusCode::OK,
    )?;
    let scoped = format!("?conversation={scout_and_bob}");
    // Scout sees Bob typing to it.
    assert_eq!(activity(&app, &scout, "c:bob", &scoped).await?, "typing");
    // Dee, as herself, is not in that conversation: Bob is online, his activity is not hers
    // to see, even naming the conversation she reads as Scout's custodian.
    assert_eq!(activity(&app, &dee, "c:bob", "").await?, Value::Null);
    assert_eq!(activity(&app, &dee, "c:bob", &scoped).await?, Value::Null);
    assert_eq!(
        activity(&app, &dee, "c:bob", &format!("?conversation={dee_and_bob}")).await?,
        Value::Null
    );
    // As Scout's view (`for`), she sees what Scout sees.
    assert_eq!(
        activity(&app, &dee, "c:bob", &format!("{scoped}&for=si:scout")).await?,
        "typing"
    );
    assert_eq!(
        activity(&app, &dee, "c:bob", "?for=si:scout").await?,
        "typing"
    );
    assert_eq!(
        activity(
            &app,
            &dee,
            "c:bob",
            &format!("?for=si:scout&conversation={dee_and_bob}")
        )
        .await?,
        Value::Null,
        "Scout is not in Dee's own conversation with Bob"
    );
    // Only a custodian may look through a Silicon's eyes.
    let not_custodian = seen(&app, &bob, "si:scout", "?for=si:scout").await?;
    assert_eq!(not_custodian.1["error"]["code"], "not_custodian");

    // A custodian's view is read only: her activity cannot be about Scout's conversation.
    let refused = renew(
        &app,
        &dee,
        "dee-laptop",
        json!({"activity": "typing", "conversation_id": scout_and_bob}),
    )
    .await?;
    assert_eq!(refused.0, StatusCode::FORBIDDEN, "{}", refused.1);
    assert_eq!(refused.1["error"]["code"], "read_only_view");
    assert_eq!(refused.1["error"]["details"]["field"], "conversation_id");
    // Nothing was renewed by the refusal.
    let leases: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM client_presence_leases WHERE device_id = 'dee-laptop'",
    )
    .fetch_one(app.state.store.pool())
    .await?;
    assert_eq!(leases, 0);
    app.close().await
}

#[tokio::test]
async fn activity_about_a_conversation_the_account_is_not_in_is_refused() -> Result<(), BoxError> {
    let app = TestApp::start().await?;
    let p = people(&app).await;
    let ada = app.accounts.token(&p.ada);
    let bob = app.accounts.token(&p.bob);
    let bob_and_cy = converse(&app, &bob, "c:cyra").await?;
    converse(&app, &ada, "c:bob").await?;

    // Bob and Cy's conversation is not Ada's: refused, and nothing renewed.
    let outsider = renew(
        &app,
        &ada,
        "ada-laptop",
        json!({"activity": "typing", "conversation_id": bob_and_cy}),
    )
    .await?;
    assert_eq!(outsider.0, StatusCode::NOT_FOUND, "{}", outsider.1);
    assert_eq!(outsider.1["error"]["code"], "not_found");
    assert_eq!(outsider.1["error"]["details"]["field"], "conversation_id");
    let hint = outsider.1["error"]["hint"].as_str().unwrap_or_default();
    assert!(hint.contains("leave conversation_id out"), "{hint}");
    // Something that is no address at all says what an address looks like.
    let malformed = renew(
        &app,
        &ada,
        "ada-laptop",
        json!({"activity": "typing", "conversation_id": "c:ada::"}),
    )
    .await?;
    assert_eq!(
        malformed.0,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        malformed.1
    );
    assert_eq!(malformed.1["error"]["details"]["field"], "conversation_id");
    let empty = renew(
        &app,
        &ada,
        "ada-laptop",
        json!({"activity": "typing", "conversation_id": " "}),
    )
    .await?;
    assert_eq!(empty.0, StatusCode::UNPROCESSABLE_ENTITY, "{}", empty.1);
    // A group Ada is only invited to is not hers to type in yet.
    let group = app
        .post(
            "/api/v1/groups",
            &bob,
            json!({"name": "Presence crew", "member_ids": ["c:ada"]}),
        )
        .await?;
    expect_status(&group, StatusCode::CREATED)?;
    let group_id = group.1["id"].as_str().unwrap_or_default().to_owned();
    let invited = renew(
        &app,
        &ada,
        "ada-laptop",
        json!({"activity": "typing", "conversation_id": group_id}),
    )
    .await?;
    assert_eq!(invited.0, StatusCode::FORBIDDEN, "{}", invited.1);
    assert_eq!(invited.1["error"]["code"], "invitation_pending");
    let leases: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM client_presence_leases WHERE device_id = 'ada-laptop'",
    )
    .fetch_one(app.state.store.pool())
    .await?;
    assert_eq!(leases, 0, "a refused renewal renews nothing");
    // A group she is in is fine, and its members see it there.
    let joined = app
        .post(
            &format!("/api/v1/groups/{group_id}/accept"),
            &ada,
            json!(null),
        )
        .await?;
    expect_status(&joined, StatusCode::OK)?;
    expect_status(
        &renew(
            &app,
            &ada,
            "ada-laptop",
            json!({"activity": "typing", "conversation_id": group_id}),
        )
        .await?,
        StatusCode::OK,
    )?;
    assert_eq!(
        activity(&app, &bob, "c:ada", &format!("?conversation={group_id}")).await?,
        "typing"
    );
    app.close().await
}
