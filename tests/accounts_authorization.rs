//! Who may do what: anyone signed in can reach any account, Carbon or Silicon (a Silicon's
//! allow-list no longer limits anything); a custodian reads its Silicons' conversations but
//! never acts as them; groups have co-equal owners and invitations that must be accepted.
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

/// c:ada looks after si:cos and si:scout; c:bob looks after si:eve.
struct People {
    ada: TestAccount,
    cos: TestAccount,
    scout: TestAccount,
    bob: TestAccount,
    eve: TestAccount,
}

async fn people(app: &TestApp) -> People {
    let ada = TestAccount::carbon("a8K", "c:ada");
    let bob = TestAccount::carbon("Bb2", "c:bob");
    let cos = TestAccount::silicon("K1E", "si:cos", &ada);
    let scout = TestAccount::silicon("Sc0", "si:scout", &ada);
    let eve = TestAccount::silicon("Ev3", "si:eve", &bob);
    app.accounts
        .add_accounts(&[&ada, &bob, &cos, &scout, &eve])
        .await;
    app.accounts.live_introspection().await;
    People {
        ada,
        cos,
        scout,
        bob,
        eve,
    }
}

async fn send(
    app: &TestApp,
    from: &TestAccount,
    to: &str,
    text: &str,
) -> Result<(StatusCode, Value), BoxError> {
    app.post(
        "/api/v1/messages",
        &app.accounts.token(from),
        json!({"recipient_id": to, "message": text}),
    )
    .await
}

#[tokio::test]
async fn anyone_signed_in_reaches_any_carbon_or_silicon() -> Result<(), BoxError> {
    let app = TestApp::start().await?;
    let p = people(&app).await;
    // Anyone signed in can message a Carbon.
    let sent = send(&app, &p.bob, "c:ada", "hello Ada").await?;
    expect_status(&sent, StatusCode::ACCEPTED)?;
    assert_eq!(sent.1["sender"]["id"], "c:bob");
    // And any Silicon, as a Carbon is reached: c:bob is not si:cos's custodian, not one of
    // the custodian's Silicons and on no allow-list.
    let first = send(&app, &p.bob, "si:cos", "hello cos").await?;
    expect_status(&first, StatusCode::ACCEPTED)?;
    assert_eq!(first.1["conversation_id"], "c:bob::si:cos");
    // The custodian, the custodian's other Silicons and another Carbon's Silicon too.
    for (from, text) in [
        (&p.ada, "hi cos"),
        (&p.scout, "hi sibling"),
        (&p.eve, "hi stranger"),
    ] {
        expect_status(
            &send(&app, from, "si:cos", text).await?,
            StatusCode::ACCEPTED,
        )?;
    }
    // The service says so, and which refusal code it no longer returns.
    let contracts = app
        .get("/api/v1/contracts", &app.accounts.token(&p.bob))
        .await?;
    let reach = &contracts.1["features"]["reach"];
    assert_eq!(reach["anyone_may_start"], true, "{}", contracts.1);
    assert_eq!(reach["retired_codes"], json!(["contact_not_allowed"]));
    assert_eq!(reach["first_contacts_per_hour"], 100);
    assert_eq!(
        contracts.1["features"]["accounts"]["silicon_allow_list"],
        "/api/v1/allow-list"
    );

    // The allow-list routes answer as before, for older clients, but limit nothing.
    let allowed = app
        .post(
            "/api/v1/allow-list",
            &app.accounts.token(&p.cos),
            json!({"account_ids": ["c:bob"]}),
        )
        .await?;
    expect_status(&allowed, StatusCode::OK)?;
    assert_eq!(allowed.1["items"][0]["account"]["id"], "c:bob");
    assert!(
        allowed.1["note"]
            .as_str()
            .is_some_and(|note| note.contains("no longer limits anything")),
        "{}",
        allowed.1
    );
    // The custodian manages the list too; a Carbon has none; an outsider cannot touch it.
    let custodian_view = app
        .get("/api/v1/allow-list?for=si:cos", &app.accounts.token(&p.ada))
        .await?;
    expect_status(&custodian_view, StatusCode::OK)?;
    assert_eq!(custodian_view.1["silicon"]["id"], "si:cos");
    let carbon = app
        .get("/api/v1/allow-list", &app.accounts.token(&p.bob))
        .await?;
    assert_eq!(carbon.1["error"]["code"], "carbon_has_no_allow_list");
    let outsider = app
        .get("/api/v1/allow-list?for=si:cos", &app.accounts.token(&p.bob))
        .await?;
    assert_eq!(outsider.1["error"]["code"], "not_custodian");
    let removed = app
        .call(
            Method::DELETE,
            "/api/v1/allow-list?for=si:cos",
            Some(&app.accounts.token(&p.ada)),
            Some(json!({"account_ids": ["c:bob"]})),
            &[],
        )
        .await?;
    expect_status(&removed, StatusCode::OK)?;
    assert_eq!(removed.1["items"].as_array().map(Vec::len), Some(0));
    // Taking c:bob off changes nothing: its conversation goes on, and it still starts new
    // ones with si:cos (a conversation of several here) and invites it to groups.
    expect_status(
        &send(&app, &p.bob, "si:cos", "still talking").await?,
        StatusCode::ACCEPTED,
    )?;
    let several = app
        .post(
            "/api/v1/conversations",
            &app.accounts.token(&p.bob),
            json!({"participant_ids": ["si:cos", "si:eve"]}),
        )
        .await?;
    expect_status(&several, StatusCode::CREATED)?;
    let invited = app
        .post(
            "/api/v1/groups",
            &app.accounts.token(&p.bob),
            json!({"name": "Bob and cos", "member_ids": ["si:cos"]}),
        )
        .await?;
    expect_status(&invited, StatusCode::CREATED)?;
    assert_eq!(invited.1["group"]["pending_invitations"][0]["id"], "si:cos");

    // Unknown ids fail with the id named; a deleted one says so.
    app.accounts.unknown_ids().await;
    let unknown = send(&app, &p.bob, "c:nobody", "hi").await?;
    assert_eq!(unknown.0, StatusCode::NOT_FOUND);
    assert_eq!(unknown.1["error"]["code"], "account_not_found");
    assert!(
        unknown.1["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("c:nobody"))
    );
    let to_self = send(&app, &p.bob, "c:bob", "me").await?;
    assert_eq!(to_self.0, StatusCode::UNPROCESSABLE_ENTITY);
    app.close().await
}

#[tokio::test]
async fn one_canonical_direct_conversation_per_set_of_accounts() -> Result<(), BoxError> {
    let app = TestApp::start().await?;
    let p = people(&app).await;
    let first = send(&app, &p.bob, "c:ada", "one").await?;
    let second = send(&app, &p.ada, "c:bob", "two").await?;
    assert_eq!(first.1["conversation_id"], second.1["conversation_id"]);
    assert_eq!(
        first.1["conversation_id"], "c:ada::c:bob",
        "addressed by current public ids"
    );
    let created = app
        .post(
            "/api/v1/conversations",
            &app.accounts.token(&p.ada),
            json!({"participant_ids": ["c:bob"]}),
        )
        .await?;
    expect_status(&created, StatusCode::CREATED)?;
    assert_eq!(created.1["id"], "c:ada::c:bob");
    // The address resolves in paths, from either side, with any id spelling.
    for (who, path) in [
        (&p.ada, "/api/v1/conversations/c:ada::c:bob/messages"),
        (&p.bob, "/api/v1/conversations/c:ada::c:bob/messages"),
        (&p.bob, "/api/v1/conversations/c:ada/messages"),
        (&p.bob, "/api/v1/conversations/a8K/messages"),
    ] {
        let listed = app.get(path, &app.accounts.token(who)).await?;
        expect_status(&listed, StatusCode::OK)?;
        assert_eq!(
            listed.1["items"].as_array().map(Vec::len),
            Some(2),
            "{path}"
        );
    }
    // An outsider learns nothing about it.
    let hidden = app
        .get(
            "/api/v1/conversations/c:ada::c:bob/messages",
            &app.accounts.token(&p.eve),
        )
        .await?;
    assert_eq!(hidden.0, StatusCode::NOT_FOUND);
    app.close().await
}

#[tokio::test]
async fn custodians_read_their_silicons_conversations_but_never_act_as_them() -> Result<(), BoxError>
{
    let app = TestApp::start().await?;
    let p = people(&app).await;
    let from_bob = send(&app, &p.bob, "si:cos", "from bob").await?;
    expect_status(&from_bob, StatusCode::ACCEPTED)?;
    let from_cos = send(&app, &p.cos, "c:bob", "from cos").await?;
    expect_status(&from_cos, StatusCode::ACCEPTED)?;
    let conversation = from_cos.1["conversation_id"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert_eq!(conversation, "c:bob::si:cos");
    let ada = app.accounts.token(&p.ada);

    // The custodian lists and reads the Silicon's conversations, and is named on them.
    let listed = app.get("/api/v1/conversations?for=si:cos", &ada).await?;
    expect_status(&listed, StatusCode::OK)?;
    let found = listed.1["items"]
        .as_array()
        .and_then(|items| items.iter().find(|item| item["id"] == conversation))
        .cloned()
        .ok_or("the Silicon's conversation is listed for its custodian")?;
    assert_eq!(found["visible_to_custodians"][0]["id"], "c:ada");
    let messages = app
        .get(
            &format!("/api/v1/conversations/{conversation}/messages"),
            &ada,
        )
        .await?;
    expect_status(&messages, StatusCode::OK)?;
    assert_eq!(messages.1["items"].as_array().map(Vec::len), Some(2));
    // ... but cannot send, edit, receipt or write drafts there.
    let path = format!("/api/v1/conversations/{conversation}/messages");
    let refused = app.post(&path, &ada, json!({"message": "as cos?"})).await?;
    assert_eq!(refused.1["error"]["code"], "custodian_read_only");
    let cos_message = format!(
        "{path}/{}",
        from_cos.1["message-id"].as_str().unwrap_or_default()
    );
    let bob_message = format!(
        "{path}/{}",
        from_bob.1["message-id"].as_str().unwrap_or_default()
    );
    let edit = app
        .call(
            Method::PATCH,
            &cos_message,
            Some(&ada),
            Some(json!({"message": "edited"})),
            &[],
        )
        .await?;
    assert_eq!(edit.0, StatusCode::FORBIDDEN, "{}", edit.1);
    let receipt = app
        .post(
            &format!("{bob_message}/receipts"),
            &ada,
            json!({"status": "read", "device_id": "ada-phone"}),
        )
        .await?;
    assert_eq!(receipt.0, StatusCode::FORBIDDEN);
    let draft = app
        .call(
            Method::PUT,
            &format!("/api/v1/conversations/{conversation}/draft"),
            Some(&ada),
            Some(json!({"message_content": "x", "metadata": {}})),
            &[],
        )
        .await?;
    assert_eq!(draft.0, StatusCode::FORBIDDEN);
    // It may tombstone its Silicon's message, never someone else's.
    let tombstone = app
        .call(Method::DELETE, &cos_message, Some(&ada), None, &[])
        .await?;
    expect_status(&tombstone, StatusCode::OK)?;
    assert!(tombstone.1["deleted_at"].is_string());
    let not_its = app
        .call(Method::DELETE, &bob_message, Some(&ada), None, &[])
        .await?;
    assert_eq!(not_its.1["error"]["code"], "not_message_sender");
    // The Silicon's draft is readable by its custodian.
    let saved = app
        .call(
            Method::PUT,
            &format!("/api/v1/conversations/{conversation}/draft"),
            Some(&app.accounts.token(&p.cos)),
            Some(json!({"message_content": "thinking", "metadata": {}})),
            &[],
        )
        .await?;
    expect_status(&saved, StatusCode::OK)?;
    let read = app
        .get(
            &format!("/api/v1/conversations/{conversation}/draft?for=si:cos"),
            &ada,
        )
        .await?;
    expect_status(&read, StatusCode::OK)?;
    assert_eq!(read.1["message_content"], "thinking");
    assert_eq!(read.1["member_id"], "si:cos");
    assert_eq!(read.1["member_uuid"], "K1E");
    // Presence: partners and the custodian see it; others do not.
    app.call(
        Method::PUT,
        "/api/v1/presence/devices/cos-box",
        Some(&app.accounts.token(&p.cos)),
        Some(json!({"activity": "typing"})),
        &[],
    )
    .await?;
    let seen = app
        .get("/api/v1/presence/si:cos", &app.accounts.token(&p.bob))
        .await?;
    expect_status(&seen, StatusCode::OK)?;
    assert_eq!(seen.1["availability"], "online");
    assert_eq!(seen.1["member_id"], "si:cos");
    expect_status(
        &app.get("/api/v1/presence/si:cos", &ada).await?,
        StatusCode::OK,
    )?;
    assert_eq!(
        app.get("/api/v1/presence/si:cos", &app.accounts.token(&p.eve))
            .await?
            .0,
        StatusCode::NOT_FOUND
    );
    // Another Carbon cannot use `for`.
    let not_custodian = app
        .get(
            "/api/v1/conversations?for=si:cos",
            &app.accounts.token(&p.bob),
        )
        .await?;
    assert_eq!(not_custodian.1["error"]["code"], "not_custodian");
    app.close().await
}

#[tokio::test]
async fn groups_have_co_equal_owners_and_invitations_that_must_be_accepted() -> Result<(), BoxError>
{
    let app = TestApp::start().await?;
    let p = people(&app).await;
    let ada = app.accounts.token(&p.ada);
    let bob = app.accounts.token(&p.bob);
    let cos = app.accounts.token(&p.cos);

    // The creator owns it; its own Silicon joins at once; everyone else is invited, a Silicon
    // another Carbon looks after included: anyone may invite any Silicon, which answers for
    // itself.
    let created = app
        .post(
            "/api/v1/groups",
            &ada,
            json!({"name": "Design", "description": "reviews", "member_ids": ["c:bob", "si:cos", "si:eve"]}),
        )
        .await?;
    expect_status(&created, StatusCode::CREATED)?;
    let group = created.1["id"].as_str().unwrap_or_default().to_owned();
    assert_eq!(
        group, "g:design",
        "new groups have no organization in their id"
    );
    assert_eq!(created.1["group"]["owners"][0]["id"], "c:ada");
    let members: Vec<&str> = created.1["group"]["members"]
        .as_array()
        .map(|members| members.iter().filter_map(|m| m["id"].as_str()).collect())
        .unwrap_or_default();
    assert_eq!(members, vec!["c:ada", "si:cos"]);
    let mut pending: Vec<&str> = created.1["group"]["pending_invitations"]
        .as_array()
        .map(|invitees| invitees.iter().filter_map(|i| i["id"].as_str()).collect())
        .unwrap_or_default();
    pending.sort_unstable();
    assert_eq!(pending, vec!["c:bob", "si:eve"]);
    // Same name again: a suffix keeps ids unique.
    let again = app
        .post("/api/v1/groups", &ada, json!({"name": "Design"}))
        .await?;
    assert_eq!(again.1["id"], "g:design-2");

    // History before c:bob accepts.
    let early = app
        .post(
            &format!("/api/v1/conversations/{group}/messages"),
            &cos,
            json!({"message": "design update"}),
        )
        .await?;
    expect_status(&early, StatusCode::ACCEPTED)?;
    // The invitee sees the invitation, not the group.
    let pending = app.get(&format!("/api/v1/groups/{group}"), &bob).await?;
    assert_eq!(pending.1["error"]["code"], "invitation_pending");
    let hidden = app
        .get(&format!("/api/v1/conversations/{group}/messages"), &bob)
        .await?;
    assert_eq!(hidden.0, StatusCode::FORBIDDEN, "{}", hidden.1);
    let invitations = app.get("/api/v1/invitations", &bob).await?;
    expect_status(&invitations, StatusCode::OK)?;
    assert_eq!(invitations.1["items"][0]["conversation_id"], group);
    assert_eq!(invitations.1["items"][0]["invited_by"]["id"], "c:ada");
    // Members do not see c:bob until it accepts; then it sees the full history.
    let as_member = app.get(&format!("/api/v1/groups/{group}"), &cos).await?;
    assert_eq!(
        as_member.1["group"]["pending_invitations"],
        json!([]),
        "only owners see pending invitations"
    );
    let accepted = app
        .post(&format!("/api/v1/groups/{group}/accept"), &bob, json!(null))
        .await;
    let accepted = match accepted {
        Ok(response) => response,
        Err(error) => return Err(error),
    };
    expect_status(&accepted, StatusCode::OK)?;
    let history = app
        .get(&format!("/api/v1/conversations/{group}/messages"), &bob)
        .await?;
    expect_status(&history, StatusCode::OK)?;
    assert_eq!(history.1["items"][0]["message"], "design update");

    // Only owners manage; owners are co-equal; the last owner cannot leave.
    let not_owner = app
        .post(
            &format!("/api/v1/groups/{group}/members"),
            &bob,
            json!({"member_ids": ["si:scout"]}),
        )
        .await?;
    assert_eq!(not_owner.1["error"]["code"], "not_group_owner");
    let promoted = app
        .post(
            &format!("/api/v1/groups/{group}/owners"),
            &ada,
            json!({"member_ids": ["c:bob"]}),
        )
        .await?;
    expect_status(&promoted, StatusCode::OK)?;
    assert_eq!(promoted.1["owners"].as_array().map(Vec::len), Some(2));
    let renamed = app
        .call(
            Method::PATCH,
            &format!("/api/v1/groups/{group}"),
            Some(&bob),
            Some(json!({"name": "Design reviews", "description": "weekly"})),
            &[("if-match", promoted.1["version"].to_string())],
        )
        .await?;
    expect_status(&renamed, StatusCode::OK)?;
    assert_eq!(renamed.1["name"], "Design reviews");
    let left = app
        .post(&format!("/api/v1/groups/{group}/leave"), &ada, json!(null))
        .await?;
    assert_eq!(left.0, StatusCode::NO_CONTENT, "{}", left.1);
    let last = app
        .post(&format!("/api/v1/groups/{group}/leave"), &bob, json!(null))
        .await?;
    assert_eq!(last.1["error"]["code"], "last_owner");
    let demote_last = app
        .call(
            Method::DELETE,
            &format!("/api/v1/groups/{group}/owners"),
            Some(&bob),
            Some(json!({"member_ids": ["c:bob"]})),
            &[],
        )
        .await?;
    assert_eq!(demote_last.1["error"]["code"], "last_owner");

    // A custodian takes its own Silicon out of a group it does not manage.
    let removed = app
        .call(
            Method::DELETE,
            &format!("/api/v1/groups/{group}/members"),
            Some(&ada),
            Some(json!({"member_ids": ["si:cos"]})),
            &[],
        )
        .await;
    // c:ada left the group: it is no longer a member, but still custodian of a member.
    let removed = removed?;
    expect_status(&removed, StatusCode::OK)?;
    let members: Vec<&str> = removed.1["members"]
        .as_array()
        .map(|members| members.iter().filter_map(|m| m["id"].as_str()).collect())
        .unwrap_or_default();
    assert_eq!(members, vec!["c:bob"]);
    // An invitee declines; a custodian may decline for its Silicon.
    app.post(
        &format!("/api/v1/groups/{group}/members"),
        &bob,
        json!({"member_ids": ["c:ada"]}),
    )
    .await?;
    let declined = app
        .post(
            &format!("/api/v1/groups/{group}/decline"),
            &ada,
            json!(null),
        )
        .await?;
    assert_eq!(declined.0, StatusCode::NO_CONTENT, "{}", declined.1);
    let again = app
        .post(
            &format!("/api/v1/groups/{group}/decline"),
            &ada,
            json!(null),
        )
        .await?;
    assert_eq!(again.0, StatusCode::NOT_FOUND);
    // si:eve was asked, not added: it sees its invitation, and its custodian may decline it
    // for it.
    let eve = app.accounts.token(&p.eve);
    let asked = app.get("/api/v1/invitations", &eve).await?;
    expect_status(&asked, StatusCode::OK)?;
    assert_eq!(asked.1["items"][0]["conversation_id"], group);
    let for_eve = app
        .post(
            &format!("/api/v1/groups/{group}/decline?for=si:eve"),
            &bob,
            json!(null),
        )
        .await?;
    assert_eq!(for_eve.0, StatusCode::NO_CONTENT, "{}", for_eve.1);
    let answered = app.get("/api/v1/invitations", &eve).await?;
    assert_eq!(answered.1["items"], json!([]), "{}", answered.1);
    app.close().await
}

#[tokio::test]
async fn a_custodian_manages_the_groups_its_silicon_owns() -> Result<(), BoxError> {
    let app = TestApp::start().await?;
    let p = people(&app).await;
    let created = app
        .post(
            "/api/v1/groups",
            &app.accounts.token(&p.cos),
            json!({"name": "Ops", "member_ids": ["si:scout"]}),
        )
        .await?;
    expect_status(&created, StatusCode::CREATED)?;
    let group = created.1["id"].as_str().unwrap_or_default().to_owned();
    // The custodian is not itself an owner but manages the group for its Silicon.
    let renamed = app
        .call(
            Method::PATCH,
            &format!("/api/v1/groups/{group}"),
            Some(&app.accounts.token(&p.ada)),
            Some(json!({"name": "Operations"})),
            &[("if-match", created.1["group"]["version"].to_string())],
        )
        .await?;
    expect_status(&renamed, StatusCode::OK)?;
    let outsider = app
        .call(
            Method::PATCH,
            &format!("/api/v1/groups/{group}"),
            Some(&app.accounts.token(&p.bob)),
            Some(json!({"name": "Mine"})),
            &[("if-match", "2".to_owned())],
        )
        .await?;
    assert_eq!(
        outsider.0,
        StatusCode::NOT_FOUND,
        "an outsider cannot even see it: {}",
        outsider.1
    );
    app.close().await
}
