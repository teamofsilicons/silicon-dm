//! `POST /webhook/`: Silicon Accounts events. Signatures are checked over the raw body,
//! bad bodies are 400, unknown types are acknowledged, each event applies once, and late
//! deliveries never move DM's view of an account backwards.
#![allow(
    clippy::too_many_lines,
    reason = "each test is one scenario, read top to bottom"
)]
mod support;

use axum::http::StatusCode;
use serde_json::{Value, json};
use support::{
    BoxError,
    accounts::{TestAccount, WEBHOOK_SECRET, now, signed, webhook},
    app::{TestApp, expect_status},
};

/// An RFC 3339 time `seconds` from now.
fn at(seconds: i64) -> String {
    (time::OffsetDateTime::now_utc() + time::Duration::seconds(seconds))
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

fn replace(
    headers: &[(&'static str, String)],
    name: &str,
    value: &str,
) -> Vec<(&'static str, String)> {
    headers
        .iter()
        .map(|(header, current)| {
            (
                *header,
                if *header == name {
                    value.to_owned()
                } else {
                    current.clone()
                },
            )
        })
        .collect()
}

fn id_changed(uuid: &str, old: &str, new: &str) -> Value {
    json!({"uuid": uuid, "membership_id": format!("dm:{uuid}"), "kind": "carbon", "old_id": old, "new_id": new})
}

async fn outcome(app: &TestApp, event_id: &str) -> Result<Option<String>, BoxError> {
    Ok(
        sqlx::query_scalar("SELECT outcome FROM accounts_webhook_receipts WHERE event_id = $1")
            .bind(event_id)
            .fetch_optional(app.state.store.pool())
            .await?,
    )
}

fn event_id(headers: &[(&'static str, String)]) -> String {
    headers
        .iter()
        .find(|(name, _)| *name == "x-accounts-event-id")
        .map(|(_, value)| value.clone())
        .unwrap_or_default()
}

#[tokio::test]
async fn deliveries_are_authenticated_parsed_and_applied_once() -> Result<(), BoxError> {
    let app = TestApp::start().await?;
    let ada = TestAccount::carbon("a8K", "c:ada");
    app.accounts.add_account(&ada).await;
    let token = app.accounts.token(&ada);
    expect_status(&app.get("/api/v1/me", &token).await?, StatusCode::OK)?;
    let (headers, body) = webhook(
        "account.id_changed",
        &id_changed("a8K", "c:ada", "c:ada2"),
        &at(0),
    );

    // Signed with another secret, signed too long ago, unsigned, or changed after signing.
    let wrong =
        silicon_accounts_client::sign_webhook("whsec_another-secret", now(), body.as_bytes());
    assert_eq!(
        app.deliver(&replace(&headers, "x-accounts-signature", &wrong), &body)
            .await?,
        StatusCode::UNAUTHORIZED
    );
    let old = now() - 600;
    let stale = replace(
        &replace(&headers, "x-accounts-timestamp", &old.to_string()),
        "x-accounts-signature",
        &silicon_accounts_client::sign_webhook(WEBHOOK_SECRET, old, body.as_bytes()),
    );
    assert_eq!(app.deliver(&stale, &body).await?, StatusCode::UNAUTHORIZED);
    let unsigned: Vec<_> = headers
        .iter()
        .filter(|(name, _)| *name != "x-accounts-signature")
        .cloned()
        .collect();
    assert_eq!(
        app.deliver(&unsigned, &body).await?,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        app.deliver(&headers, &body.replace("c:ada2", "c:mallory"))
            .await?,
        StatusCode::UNAUTHORIZED
    );
    let me = app.get("/api/v1/me", &token).await?;
    assert_eq!(me.1["account"]["id"], "c:ada", "nothing was applied");

    // Signed, but not an event; an event without its data; an invalid id.
    let (h, b) = signed("evt-not-json", "account.updated", "not json".to_owned());
    assert_eq!(app.deliver(&h, &b).await?, StatusCode::BAD_REQUEST);
    let no_data = json!({"event_id": "evt-no-data", "type": "account.id_changed", "data": "nope"})
        .to_string();
    let (h, b) = signed("evt-no-data", "account.id_changed", no_data);
    assert_eq!(app.deliver(&h, &b).await?, StatusCode::BAD_REQUEST);
    let (h, b) = webhook(
        "account.id_changed",
        &id_changed("a8K", "c:ada", "Not An Id"),
        &at(0),
    );
    assert_eq!(app.deliver(&h, &b).await?, StatusCode::BAD_REQUEST);

    // Types DM does not act on, and pings, are acknowledged.
    let (h, b) = webhook("account.recovered", &json!({"uuid": "a8K"}), &at(0));
    assert_eq!(app.deliver(&h, &b).await?, StatusCode::NO_CONTENT);
    assert_eq!(
        outcome(&app, &event_id(&h)).await?.as_deref(),
        Some("ignored")
    );
    let (h, b) = webhook("ping", &json!({}), &at(0));
    assert_eq!(app.deliver(&h, &b).await?, StatusCode::NO_CONTENT);

    // The genuine delivery applies; a redelivery of the same event is not applied again.
    assert_eq!(app.deliver(&headers, &body).await?, StatusCode::NO_CONTENT);
    assert_eq!(
        outcome(&app, &event_id(&headers)).await?.as_deref(),
        Some("applied")
    );
    assert_eq!(
        app.get("/api/v1/me", &token).await?.1["account"]["id"],
        "c:ada2"
    );
    sqlx::query(
        "UPDATE accounts SET current_id = 'c:ada', id_changed_at = NULL WHERE uuid = 'a8K'",
    )
    .execute(app.state.store.pool())
    .await?;
    assert_eq!(app.deliver(&headers, &body).await?, StatusCode::NO_CONTENT);
    let current: Option<String> =
        sqlx::query_scalar("SELECT current_id FROM accounts WHERE uuid = 'a8K'")
            .fetch_one(app.state.store.pool())
            .await?;
    assert_eq!(
        current.as_deref(),
        Some("c:ada"),
        "the event id was already seen"
    );
    app.close().await
}

#[tokio::test]
async fn an_id_change_renames_the_account_everywhere_and_never_moves_back() -> Result<(), BoxError>
{
    let app = TestApp::start().await?;
    let ada = TestAccount::carbon("a8K", "c:ada");
    let bob = TestAccount::carbon("Bb2", "c:bob");
    app.accounts.add_accounts(&[&ada, &bob]).await;
    let sent = app
        .post(
            "/api/v1/messages",
            &app.accounts.token(&ada),
            json!({"recipient_id": "c:bob", "message": "hello"}),
        )
        .await?;
    expect_status(&sent, StatusCode::ACCEPTED)?;
    let (h, b) = webhook(
        "account.id_changed",
        &id_changed("a8K", "c:ada", "c:ada-lovelace"),
        &at(0),
    );
    assert_eq!(app.deliver(&h, &b).await?, StatusCode::NO_CONTENT);

    // Accounts now knows the account by its new id only.
    let renamed = TestAccount {
        id: "c:ada-lovelace".to_owned(),
        ..ada.clone()
    };
    app.accounts.serve_only(&[&renamed, &bob]).await;
    let bob_token = app.accounts.token(&bob);
    let listed = app.get("/api/v1/conversations", &bob_token).await?;
    assert_eq!(
        listed.1["items"][0]["id"], "c:ada-lovelace::c:bob",
        "{}",
        listed.1
    );
    let messages = app
        .get(
            "/api/v1/conversations/c:ada-lovelace::c:bob/messages",
            &bob_token,
        )
        .await?;
    assert_eq!(messages.1["items"][0]["sender"]["id"], "c:ada-lovelace");
    assert_eq!(messages.1["items"][0]["sender"]["uuid"], "a8K");
    let reply = app
        .post(
            "/api/v1/messages",
            &bob_token,
            json!({"recipient_id": "c:ada-lovelace", "message": "hi"}),
        )
        .await?;
    expect_status(&reply, StatusCode::ACCEPTED)?;
    assert_eq!(reply.1["conversation_id"], "c:ada-lovelace::c:bob");
    let old = app
        .post(
            "/api/v1/messages",
            &bob_token,
            json!({"recipient_id": "c:ada", "message": "?"}),
        )
        .await?;
    assert_eq!(old.0, StatusCode::NOT_FOUND, "{}", old.1);
    assert_eq!(old.1["error"]["code"], "account_not_found");

    // An older change delivered late is recorded and ignored.
    let (h, b) = webhook(
        "account.id_changed",
        &id_changed("a8K", "c:ada", "c:ada-old"),
        &at(-3600),
    );
    assert_eq!(app.deliver(&h, &b).await?, StatusCode::NO_CONTENT);
    assert_eq!(
        outcome(&app, &event_id(&h)).await?.as_deref(),
        Some("stale")
    );
    // A token issued before the change still carries the old id; it does not win either.
    let me = app.get("/api/v1/me", &app.accounts.token(&ada)).await?;
    assert_eq!(me.1["account"]["id"], "c:ada-lovelace");
    app.close().await
}

#[tokio::test]
async fn profile_updates_apply_in_version_order() -> Result<(), BoxError> {
    let app = TestApp::start().await?;
    let ada = TestAccount::carbon("a8K", "c:ada");
    let bob = TestAccount::carbon("Bb2", "c:bob");
    app.accounts.add_accounts(&[&ada, &bob]).await;
    app.post(
        "/api/v1/messages",
        &app.accounts.token(&ada),
        json!({"recipient_id": "c:bob", "message": "hello"}),
    )
    .await?;
    let update = |version: i64, name: &str| {
        json!({"uuid": "a8K", "membership_id": "dm:a8K", "changed": ["display_name", "pfp_url"],
            "account": {"uuid": "a8K", "membership_id": "dm:a8K", "kind": "carbon", "id": "c:ada",
                "display_name": name, "pfp_url": format!("https://cdn.example/{version}.png"), "version": version}})
    };
    let (h, b) = webhook("account.updated", &update(5, "Ada Lovelace"), &at(0));
    assert_eq!(app.deliver(&h, &b).await?, StatusCode::NO_CONTENT);
    let (h, b) = webhook("account.updated", &update(3, "Ada (old)"), &at(1));
    assert_eq!(app.deliver(&h, &b).await?, StatusCode::NO_CONTENT);
    assert_eq!(
        outcome(&app, &event_id(&h)).await?.as_deref(),
        Some("stale")
    );
    let listed = app
        .get("/api/v1/conversations", &app.accounts.token(&bob))
        .await?;
    let participants = listed.1["items"][0]["participants"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let shown = participants
        .iter()
        .find(|p| p["uuid"] == "a8K")
        .cloned()
        .unwrap_or_default();
    assert_eq!(shown["display_name"], "Ada Lovelace", "{shown}");
    assert_eq!(shown["pfp_url"], "https://cdn.example/5.png");
    app.close().await
}

#[tokio::test]
async fn a_custodian_change_hands_the_silicon_to_its_new_custodian() -> Result<(), BoxError> {
    let app = TestApp::start().await?;
    let ada = TestAccount::carbon("a8K", "c:ada");
    let bob = TestAccount::carbon("Bb2", "c:bob");
    let cos = TestAccount::silicon("K1E", "si:cos", &ada);
    app.accounts.add_accounts(&[&ada, &bob, &cos]).await;
    expect_status(
        &app.get("/api/v1/me", &app.accounts.token(&cos)).await?,
        StatusCode::OK,
    )?;
    expect_status(
        &app.get(
            "/api/v1/conversations?for=si:cos",
            &app.accounts.token(&ada),
        )
        .await?,
        StatusCode::OK,
    )?;

    let change = |from: &TestAccount, to: &TestAccount| json!({"uuid": "K1E", "membership_id": "dm:K1E", "from": {"uuid": from.uuid, "id": from.id}, "to": {"uuid": to.uuid, "id": to.id}});
    let (h, b) = webhook("silicon.custodian_changed", &change(&ada, &bob), &at(0));
    assert_eq!(app.deliver(&h, &b).await?, StatusCode::NO_CONTENT);
    let moved = TestAccount::silicon("K1E", "si:cos", &bob);
    app.accounts.serve_only(&[&ada, &bob, &moved]).await;
    // The new custodian reads for the Silicon and has a conversation with it.
    let looked_after = app
        .get(
            "/api/v1/conversations?for=si:cos",
            &app.accounts.token(&bob),
        )
        .await?;
    expect_status(&looked_after, StatusCode::OK)?;
    let ids: Vec<&str> = looked_after.1["items"]
        .as_array()
        .map(|items| items.iter().filter_map(|c| c["id"].as_str()).collect())
        .unwrap_or_default();
    assert!(ids.contains(&"c:bob::si:cos"), "{ids:?}");
    let former = app
        .get(
            "/api/v1/conversations?for=si:cos",
            &app.accounts.token(&ada),
        )
        .await?;
    assert_eq!(former.0, StatusCode::FORBIDDEN);
    assert_eq!(former.1["error"]["code"], "not_custodian");
    let me = app.get("/api/v1/me", &app.accounts.token(&moved)).await?;
    assert_eq!(me.1["custodian"]["id"], "c:bob");
    // A late, older change does not hand it back.
    let (h, b) = webhook("silicon.custodian_changed", &change(&bob, &ada), &at(-3600));
    assert_eq!(app.deliver(&h, &b).await?, StatusCode::NO_CONTENT);
    assert_eq!(
        outcome(&app, &event_id(&h)).await?.as_deref(),
        Some("stale")
    );
    app.close().await
}

#[tokio::test]
async fn removed_access_ends_earlier_sign_ins_and_stored_proofs() -> Result<(), BoxError> {
    let app = TestApp::start().await?;
    let ada = TestAccount::carbon("a8K", "c:ada");
    app.accounts.add_account(&ada).await;
    let earlier = app
        .accounts
        .token_with(&ada, &json!({"iat": now() - 120, "nbf": now() - 120}));
    expect_status(&app.get("/api/v1/me", &earlier).await?, StatusCode::OK)?;
    sqlx::query(
        "INSERT INTO ting_proof_grants (app_id, actor_kind, actor_id, purpose, proof_id, nonce, ciphertext)
         VALUES ('dm', 'carbon', 'a8K', 'tings.subscribe', 'prf_1', '\\x000000000000000000000000', '\\x0000000000000000000000000000000000')",
    )
    .execute(app.state.store.pool())
    .await?;
    let (h, b) = webhook(
        "membership.access_removed",
        &json!({"uuid": "a8K", "membership_id": "dm:a8K"}),
        &at(0),
    );
    assert_eq!(app.deliver(&h, &b).await?, StatusCode::NO_CONTENT);
    let refused = app.get("/api/v1/me", &earlier).await?;
    assert_eq!(refused.0, StatusCode::UNAUTHORIZED);
    assert_eq!(refused.1["error"]["code"], "session_revoked");
    let grants: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ting_proof_grants WHERE actor_id = 'a8K'")
            .fetch_one(app.state.store.pool())
            .await?;
    assert_eq!(grants, 0);
    // Granting access again means a new sign-in, which works.
    let later = app
        .accounts
        .token_with(&ada, &json!({"iat": now() + 1, "nbf": now() - 1}));
    expect_status(&app.get("/api/v1/me", &later).await?, StatusCode::OK)?;
    app.close().await
}

#[tokio::test]
async fn a_deleted_account_keeps_its_messages_and_loses_what_was_only_its_own()
-> Result<(), BoxError> {
    let app = TestApp::start().await?;
    let ada = TestAccount::carbon("a8K", "c:ada");
    let bob = TestAccount::carbon("Bb2", "c:bob");
    let eve = TestAccount::silicon("Ev3", "si:eve", &bob);
    app.accounts.add_accounts(&[&ada, &bob, &eve]).await;
    app.accounts.live_introspection().await;
    let ada_token = app.accounts.token(&ada);
    let bob_token = app.accounts.token(&bob);
    let sent = app
        .post(
            "/api/v1/messages",
            &ada_token,
            json!({"recipient_id": "c:bob", "message": "before deletion"}),
        )
        .await?;
    expect_status(&sent, StatusCode::ACCEPTED)?;
    let draft = app
        .call(
            axum::http::Method::PUT,
            "/api/v1/conversations/c:ada::c:bob/draft",
            Some(&ada_token),
            Some(json!({"message": "never sent", "metadata": {}})),
            &[],
        )
        .await?;
    expect_status(&draft, StatusCode::OK)?;
    let group = app
        .post(
            "/api/v1/groups",
            &ada_token,
            json!({"name": "Team", "member_ids": ["c:bob"]}),
        )
        .await?;
    expect_status(&group, StatusCode::CREATED)?;
    let group_id = group.1["id"].as_str().unwrap_or_default().to_owned();
    expect_status(
        &app.post(
            &format!("/api/v1/groups/{group_id}/accept"),
            &bob_token,
            json!(null),
        )
        .await?,
        StatusCode::OK,
    )?;
    let allowed = app
        .post(
            "/api/v1/allow-list",
            &app.accounts.token(&eve),
            json!({"account_ids": ["c:ada"]}),
        )
        .await?;
    expect_status(&allowed, StatusCode::OK)?;

    let (h, b) = webhook(
        "account.deleted",
        &json!({"uuid": "a8K", "membership_id": "dm:a8K"}),
        &at(0),
    );
    assert_eq!(app.deliver(&h, &b).await?, StatusCode::NO_CONTENT);
    app.accounts.serve_only(&[&bob, &eve]).await;

    // The account cannot sign in any more.
    let refused = app.get("/api/v1/me", &ada_token).await?;
    assert_eq!(refused.0, StatusCode::UNAUTHORIZED);
    assert_eq!(refused.1["error"]["code"], "account_deleted");
    // Its messages stay with the people it wrote to, shown as from a deleted account.
    let listed = app.get("/api/v1/conversations", &bob_token).await?;
    let direct = listed.1["items"]
        .as_array()
        .and_then(|items| {
            items.iter().find(|c| {
                c["group"].is_null()
                    && c["participants"]
                        .as_array()
                        .is_some_and(|p| p.iter().any(|p| p["uuid"] == "a8K"))
            })
        })
        .and_then(|c| c["id"].as_str())
        .unwrap_or_default()
        .to_owned();
    let messages = app
        .get(
            &format!("/api/v1/conversations/{direct}/messages"),
            &bob_token,
        )
        .await?;
    expect_status(&messages, StatusCode::OK)?;
    assert_eq!(messages.1["items"][0]["message"], "before deletion");
    assert_eq!(
        messages.1["items"][0]["sender"]["deleted"], true,
        "{}",
        messages.1
    );
    assert_eq!(messages.1["items"][0]["sender"]["uuid"], "a8K");
    assert!(
        messages.1["items"][0]["sender"]
            .get("display_name")
            .is_none(),
        "no name or photo is kept"
    );
    assert_eq!(
        direct, "a8K::c:bob",
        "addressed by the uuid once the id is gone"
    );
    // It takes no new messages.
    let closed = app
        .post(
            &format!("/api/v1/conversations/{direct}/messages"),
            &bob_token,
            json!({"message": "are you there?"}),
        )
        .await?;
    assert_eq!(closed.0, StatusCode::CONFLICT);
    assert_eq!(closed.1["error"]["code"], "recipient_deleted");
    let by_uuid = app
        .post(
            "/api/v1/messages",
            &bob_token,
            json!({"recipient_id": "a8K", "message": "?"}),
        )
        .await?;
    assert_eq!(by_uuid.1["error"]["code"], "account_deleted");
    assert!(
        messages.1["items"][0]["sender"]
            .get("display_name")
            .is_none(),
        "no name or photo is kept"
    );
    assert_eq!(
        direct, "a8K::c:bob",
        "addressed by the uuid once the id is gone"
    );
    // It takes no new messages.
    let closed = app
        .post(
            &format!("/api/v1/conversations/{direct}/messages"),
            &bob_token,
            json!({"message": "are you there?"}),
        )
        .await?;
    assert_eq!(closed.0, StatusCode::CONFLICT);
    assert_eq!(closed.1["error"]["code"], "recipient_deleted");
    let by_uuid = app
        .post(
            "/api/v1/messages",
            &bob_token,
            json!({"recipient_id": "a8K", "message": "?"}),
        )
        .await?;
    assert_eq!(by_uuid.1["error"]["code"], "account_deleted");
    // The group passed to its longest-standing member.
    let details = app
        .get(&format!("/api/v1/groups/{group_id}"), &bob_token)
        .await?;
    expect_status(&details, StatusCode::OK)?;
    assert_eq!(
        details.1["group"]["owners"],
        json!([{"type": "carbon", "id": "c:bob", "uuid": "Bb2", "display_name": "bob"}])
    );
    assert_eq!(
        details.1["group"]["members"].as_array().map(Vec::len),
        Some(1),
        "{}",
        details.1
    );
    assert_eq!(
        details.1["participants"].as_array().map(Vec::len),
        Some(1),
        "{}",
        details.1
    );
    // Its draft and the allow-list entry naming it are gone.
    let drafts: i64 = sqlx::query_scalar("SELECT count(*) FROM drafts WHERE actor_id = 'a8K'")
        .fetch_one(app.state.store.pool())
        .await?;
    assert_eq!(drafts, 0);
    let allow_list = app
        .get("/api/v1/allow-list", &app.accounts.token(&eve))
        .await?;
    assert_eq!(allow_list.1["items"], json!([]), "{}", allow_list.1);
    // A late profile update does not bring it back.
    let update = json!({"uuid": "a8K", "membership_id": "dm:a8K", "changed": ["display_name"],
        "account": {"uuid": "a8K", "kind": "carbon", "id": "c:ada", "display_name": "Ada", "pfp_url": "", "version": 99}});
    let (h, b) = webhook("account.updated", &update, &at(1));
    assert_eq!(app.deliver(&h, &b).await?, StatusCode::NO_CONTENT);
    let status: String = sqlx::query_scalar("SELECT status FROM accounts WHERE uuid = 'a8K'")
        .fetch_one(app.state.store.pool())
        .await?;
    assert_eq!(status, "deleted");
    app.close().await
}

#[tokio::test]
async fn retired_webhook_subjects_and_nested_custodians_cannot_return() -> Result<(), BoxError> {
    let app = TestApp::start().await?;
    let owner = TestAccount::carbon("a750a68a-1bc2-4b3f-888e-0349c9d7289a", "c:owner");
    let silicon = TestAccount::silicon("b750a68a-1bc2-4b3f-888e-0349c9d7289a", "si:worker", &owner);
    app.accounts.add_accounts(&[&owner, &silicon]).await;
    expect_status(
        &app.get("/api/v1/me", &app.accounts.token(&silicon)).await?,
        StatusCode::OK,
    )?;
    sqlx::query("INSERT INTO accounts_uuid128_map(old_uuid,new_uuid,kind,mapping_sha256) VALUES('OldRetired','c750a68a-1bc2-4b3f-888e-0349c9d7289a','carbon','test')").execute(app.state.store.pool()).await?;
    for (kind, data) in [
        ("membership.signed_out", json!({"uuid":"OldRetired"})),
        (
            "silicon.custodian_changed",
            json!({"uuid":silicon.uuid,"to":{"uuid":"OldRetired","id":"c:retired"}}),
        ),
        (
            "account.updated",
            json!({"uuid":silicon.uuid,"account":{"uuid":silicon.uuid,"kind":"silicon","id":silicon.id,"version":999,"custodian":{"uuid":"OldRetired","id":"c:retired"}}}),
        ),
    ] {
        let (headers, body) = webhook(kind, &data, &at(0));
        assert_eq!(app.deliver(&headers, &body).await?, StatusCode::NO_CONTENT);
        assert_eq!(app.deliver(&headers, &body).await?, StatusCode::NO_CONTENT);
    }
    let current: Option<String> =
        sqlx::query_scalar("SELECT custodian_uuid FROM accounts WHERE uuid=$1")
            .bind(&silicon.uuid)
            .fetch_one(app.state.store.pool())
            .await?;
    assert_eq!(current.as_deref(), Some(owner.uuid.as_str()));
    let recreated: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM accounts WHERE uuid='OldRetired')")
            .fetch_one(app.state.store.pool())
            .await?;
    assert!(!recreated);
    app.close().await
}
