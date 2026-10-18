//! Refusals say what happened, why and what to do: the words, hints and details of the
//! refusals people meet most (an unreachable Silicon, a group name without an address, an
//! unknown group, conversation, message or route, a malformed request, too many reports),
//! and a Silicon's account reference naming its custodian. Codes never change; only the words
//! around them do.
#![allow(
    clippy::too_many_lines,
    reason = "each test is one scenario, read top to bottom"
)]
mod support;

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use serde_json::{Value, json};
use support::{
    BoxError,
    accounts::TestAccount,
    app::{TestApp, data_of, expect_status},
};

fn text(value: &Value) -> &str {
    value.as_str().unwrap_or_default()
}

/// c:ada looks after si:cos; c:bob is a stranger to it.
async fn people(app: &TestApp) -> (TestAccount, TestAccount, TestAccount) {
    let ada = TestAccount::carbon("a8K", "c:ada");
    let bob = TestAccount::carbon("Bb2", "c:bob");
    let cos = TestAccount::silicon("K1E", "si:cos", &ada);
    app.accounts.add_accounts(&[&ada, &bob, &cos]).await;
    app.accounts.live_introspection().await;
    (ada, bob, cos)
}

#[tokio::test]
async fn an_unreachable_silicon_says_where_its_custodian_allows_you() -> Result<(), BoxError> {
    let app = TestApp::start().await?;
    let (ada, bob, cos) = people(&app).await;
    // DM learns the custodian's id when the custodian signs in (as on the website).
    expect_status(
        &app.get("/api/v1/me", &app.accounts.token(&ada)).await?,
        StatusCode::OK,
    )?;
    let refused = app
        .post(
            "/api/v1/messages",
            &app.accounts.token(&bob),
            json!({"recipient_id": "si:cos", "message": "hello"}),
        )
        .await?;
    assert_eq!(refused.0, StatusCode::FORBIDDEN, "{}", refused.1);
    let error = &refused.1["error"];
    assert_eq!(error["code"], "contact_not_allowed");
    let message = text(&error["message"]);
    assert!(
        message.contains("si:cos") && message.contains("its custodian c:ada"),
        "{message}"
    );
    assert!(message.contains("c:bob is not one of them"), "{message}");
    let hint = text(&error["hint"]);
    assert!(
        hint.contains("Settings → Your Silicons' reach")
            && hint.contains("/settings#reach")
            && hint.contains("dm allow add c:bob --for si:cos")
            && hint.contains("Ask si:cos to run dm allow add c:bob"),
        "{hint}"
    );
    assert_eq!(error["details"]["silicon"], "si:cos");
    assert_eq!(error["details"]["custodian"], "c:ada");
    assert_eq!(error["details"]["requester"], "c:bob");
    assert_eq!(
        error["details"]["allow"],
        json!({"by_silicon": "dm allow add c:bob", "by_custodian": "dm allow add c:bob --for si:cos",
            "website": "/settings#reach"})
    );
    // An invitation to a group is refused with the same words.
    let group = app
        .post(
            "/api/v1/groups",
            &app.accounts.token(&bob),
            json!({"name": "Strangers", "member_ids": ["si:cos"]}),
        )
        .await?;
    assert_eq!(
        group.1["error"]["code"], "contact_not_allowed",
        "{}",
        group.1
    );
    assert!(text(&group.1["error"]["hint"]).contains("/settings#reach"));
    drop(cos);
    app.close().await
}

#[tokio::test]
async fn a_group_name_needs_a_letter_or_a_digit_to_make_its_address() -> Result<(), BoxError> {
    let app = TestApp::start().await?;
    let (ada, _, _) = people(&app).await;
    let token = app.accounts.token(&ada);
    let refused = app
        .post("/api/v1/groups", &token, json!({"name": "🎉🎉"}))
        .await?;
    assert_eq!(refused.0, StatusCode::UNPROCESSABLE_ENTITY, "{}", refused.1);
    let error = &refused.1["error"];
    assert_eq!(error["code"], "validation_error");
    assert_eq!(
        text(&error["message"]),
        "A group's address is made from its name, so the name needs a letter or a digit (a to z, 0 to 9): \"🎉🎉\" has none."
    );
    assert!(text(&error["hint"]).contains("g:launch"), "{error}");
    assert_eq!(error["details"]["rule"], "needs_letter_or_digit");
    // Letters outside a to z do not make an address either; one digit does.
    let accented = app
        .post("/api/v1/groups", &token, json!({"name": "Été à Ωmega"}))
        .await?;
    assert_eq!(accented.0, StatusCode::CREATED, "é is dropped, mega stays");
    assert_eq!(accented.1["id"], "g:t-mega");
    let only_accents = app
        .post("/api/v1/groups", &token, json!({"name": "Éé Ω"}))
        .await?;
    assert_eq!(
        only_accents.1["error"]["details"]["rule"],
        "needs_letter_or_digit"
    );
    let party = app
        .post("/api/v1/groups", &token, json!({"name": "🎉 2026"}))
        .await?;
    expect_status(&party, StatusCode::CREATED)?;
    assert_eq!(party.1["id"], "g:2026");
    // A rename keeps the address, so any name is fine afterwards.
    let group = app.get("/api/v1/groups/g:2026", &token).await?;
    let version = group.1["group"]["version"].as_i64().unwrap_or_default();
    let renamed = app
        .call(
            Method::PATCH,
            "/api/v1/groups/g:2026",
            Some(&token),
            Some(json!({"name": "🎉🎉"})),
            &[("if-match", version.to_string())],
        )
        .await?;
    expect_status(&renamed, StatusCode::OK)?;
    assert_eq!(renamed.1["name"], "🎉🎉");
    // Other name rules say what is wrong, each in its own words.
    let long = app
        .post("/api/v1/groups", &token, json!({"name": "x".repeat(121)}))
        .await?;
    assert_eq!(
        text(&long.1["error"]["message"]),
        "A group's name is at most 120 characters; this one has 121."
    );
    let blank = app
        .post("/api/v1/groups", &token, json!({"name": "   "}))
        .await?;
    assert!(text(&blank.1["error"]["message"]).contains("this one is empty"));
    app.close().await
}

#[tokio::test]
async fn missing_groups_conversations_messages_and_routes_are_named() -> Result<(), BoxError> {
    let app = TestApp::start().await?;
    let (ada, bob, _) = people(&app).await;
    let token = app.accounts.token(&ada);

    let group = app.get("/api/v1/groups/g:nope", &token).await?;
    assert_eq!(group.0, StatusCode::NOT_FOUND);
    assert_eq!(group.1["error"]["code"], "not_found", "the code stays");
    assert_eq!(
        text(&group.1["error"]["message"]),
        "No group g:nope is visible to you: it does not exist, or you are not a member of it (DM does not say which)."
    );
    assert!(text(&group.1["error"]["hint"]).contains("dm groups list"));
    // Someone else's group is answered the same way, so nobody can probe for groups.
    let bobs = app
        .post(
            "/api/v1/groups",
            &app.accounts.token(&bob),
            json!({"name": "Bob only"}),
        )
        .await?;
    expect_status(&bobs, StatusCode::CREATED)?;
    let hidden = app.get("/api/v1/groups/g:bob-only", &token).await?;
    assert_eq!(
        text(&hidden.1["error"]["message"]),
        "No group g:bob-only is visible to you: it does not exist, or you are not a member of it (DM does not say which)."
    );
    let as_conversation = app.get("/api/v1/conversations/g:bob-only", &token).await?;
    assert_eq!(as_conversation.0, StatusCode::NOT_FOUND);
    assert!(
        text(&as_conversation.1["error"]["message"])
            .starts_with("No group g:bob-only is visible to you"),
        "{}",
        as_conversation.1
    );
    let bad_id = app.get("/api/v1/groups/g:Not_A_Slug!", &token).await?;
    assert_eq!(bad_id.0, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(text(&bad_id.1["error"]["message"]).contains("is not a group id"));

    // A direct conversation that does not exist yet says how to start it.
    let none_yet = app.get("/api/v1/conversations/c:bob", &token).await?;
    assert_eq!(none_yet.0, StatusCode::NOT_FOUND, "{}", none_yet.1);
    assert_eq!(
        text(&none_yet.1["error"]["message"]),
        "You have no conversation with c:bob yet."
    );
    assert!(text(&none_yet.1["error"]["hint"]).contains("dm messages send c:bob"));
    let unknown_uuid = app
        .get(
            "/api/v1/conversations/0b7e3c1e-7f3a-4f5e-9c1d-2a6b8e4f0c11",
            &token,
        )
        .await?;
    assert_eq!(unknown_uuid.0, StatusCode::NOT_FOUND, "{}", unknown_uuid.1);
    assert_eq!(
        unknown_uuid.1["error"]["code"], "account_not_found",
        "the code a UUID nobody has always answered"
    );
    assert!(
        text(&unknown_uuid.1["error"]["message"]).starts_with("Neither a conversation you can see"),
        "{}",
        unknown_uuid.1
    );
    assert!(text(&unknown_uuid.1["error"]["hint"]).contains("dm conversations list"));
    let malformed = app.get("/api/v1/conversations/c:ada::", &token).await?;
    assert!(
        text(&malformed.1["error"]["message"]).contains("is not a direct address"),
        "{}",
        malformed.1
    );

    // A message code the conversation does not have.
    let sent = app
        .post(
            "/api/v1/messages",
            &token,
            json!({"recipient_id": "c:bob", "message": "hi"}),
        )
        .await?;
    expect_status(&sent, StatusCode::ACCEPTED)?;
    let missing = app
        .get("/api/v1/conversations/c:bob/messages/0zz", &token)
        .await?;
    assert_eq!(missing.0, StatusCode::NOT_FOUND);
    assert_eq!(
        text(&missing.1["error"]["message"]),
        "This conversation has no message 0zz."
    );
    let not_a_code = app
        .get("/api/v1/conversations/c:bob/messages/hello!", &token)
        .await?;
    assert_eq!(not_a_code.0, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(text(&not_a_code.1["error"]["message"]).contains("is not a message code"));
    let (status, _, body) = app
        .post_envelope(
            "/api/v1/conversations/c:ada/receipts",
            &app.accounts.token(&bob),
            &json!({"type": "receipts.read", "data": {"up_to": "0zz"}}),
        )
        .await?;
    let body = data_of(&body);
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(
        text(&body["error"]["message"]),
        "This conversation has no message 0zz."
    );

    // No draft yet, and presence of an account nobody here talks with.
    let draft = app.get("/api/v1/conversations/c:bob/draft", &token).await?;
    assert_eq!(draft.0, StatusCode::NOT_FOUND);
    assert_eq!(
        text(&draft.1["error"]["message"]),
        "There is no draft in this conversation."
    );
    let presence = app
        .get("/api/v1/presence/si:cos", &app.accounts.token(&bob))
        .await?;
    assert_eq!(presence.0, StatusCode::NOT_FOUND, "{}", presence.1);
    assert!(
        text(&presence.1["error"]["message"]).starts_with("si:cos shares its presence only with"),
        "{}",
        presence.1
    );

    // Routes DM does not have, and methods a route does not take.
    let route = app.get("/api/v1/nothing-here", &token).await?;
    assert_eq!(route.0, StatusCode::NOT_FOUND);
    assert_eq!(route.1["error"]["code"], "not_found");
    assert_eq!(
        text(&route.1["error"]["message"]),
        "DM has no route GET /api/v1/nothing-here."
    );
    assert!(text(&route.1["error"]["hint"]).contains("openapi.yaml"));
    let (status, headers, body) = app
        .raw(
            Request::builder()
                .method(Method::GET)
                .uri("/api/v1/groups")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(headers["allow"], "POST");
    let body = data_of(&body);
    assert_eq!(body["error"]["code"], "http_error", "the code stays");
    assert_eq!(
        text(&body["error"]["message"]),
        "The route /api/v1/groups does not take GET; the Allow header lists the methods it takes."
    );
    app.close().await
}

#[tokio::test]
async fn malformed_requests_say_what_does_not_fit() -> Result<(), BoxError> {
    let app = TestApp::start().await?;
    let (ada, _, _) = people(&app).await;
    let token = app.accounts.token(&ada);
    // The wrong request type names the one the route takes.
    let (status, _, body) = app
        .post_envelope(
            "/api/v1/groups",
            &token,
            &json!({"type": "group.create", "data": {"name": "Typed"}}),
        )
        .await?;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let body = data_of(&body);
    assert_eq!(body["error"]["code"], "validation_error");
    assert_eq!(
        text(&body["error"]["message"]),
        "This route takes the request type \"create_group\", but the request's type is \"group.create\"."
    );
    assert!(text(&body["error"]["hint"]).contains("openapi.yaml"));
    // A misspelt field leaves the one the route needs missing, and that is named.
    let (_, _, body) = app
        .post_envelope(
            "/api/v1/groups",
            &token,
            &json!({"type": "create_group", "data": {"nmae": "Typo"}}),
        )
        .await?;
    let body = data_of(&body);
    let message = text(&body["error"]["message"]);
    assert!(
        message == "The request's data does not fit this route: missing field `name`.",
        "{message}"
    );
    // A body that is not an envelope, and one that is not JSON.
    let (_, _, body) = app
        .post_envelope("/api/v1/groups", &token, &json!({"name": "Bare"}))
        .await?;
    let body = data_of(&body);
    assert!(
        text(&body["error"]["message"]).starts_with("The body must be one JSON envelope"),
        "{body}"
    );
    let (status, _, body) = app
        .raw(
            Request::builder()
                .method(Method::POST)
                .uri("/api/v1/groups")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .header("idempotency-key", "not-json-1")
                .body(Body::from("{\"type\":"))?,
        )
        .await?;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let body = data_of(&body);
    assert_eq!(body["error"]["code"], "invalid_json");
    assert!(text(&body["error"]["message"]).starts_with("The body is not valid JSON: "));
    // A query string that does not fit.
    let page = app.get("/api/v1/conversations?limit=lots", &token).await?;
    assert_eq!(page.0, StatusCode::UNPROCESSABLE_ENTITY, "{}", page.1);
    let message = text(&page.1["error"]["message"]);
    assert!(
        message.starts_with("The query string does not fit this route: ")
            && !message.contains("Failed to"),
        "{message}"
    );
    // A change without an Idempotency-Key, or with an unusable one, says how to send one.
    let (status, _, body) = app
        .raw(
            Request::builder()
                .method(Method::POST)
                .uri("/api/v1/groups")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"type": "create_group", "data": {"name": "Keyless"}}).to_string(),
                ))?,
        )
        .await?;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let body = data_of(&body);
    assert!(
        text(&body["error"]["message"]).starts_with("This change needs an Idempotency-Key header"),
        "{body}"
    );
    assert!(text(&body["error"]["hint"]).contains("8 to 255 visible ASCII characters"));
    let short = app
        .call(
            Method::POST,
            "/api/v1/groups",
            Some(&token),
            Some(json!({"name": "Short key"})),
            &[("idempotency-key", "abc".to_owned())],
        )
        .await?;
    assert!(
        text(&short.1["error"]["message"]).starts_with("The Idempotency-Key header is not usable"),
        "{}",
        short.1
    );
    // A group change without If-Match names the version to send.
    let group = app
        .post("/api/v1/groups", &token, json!({"name": "Versioned"}))
        .await?;
    expect_status(&group, StatusCode::CREATED)?;
    let unversioned = app
        .call(
            Method::PATCH,
            "/api/v1/groups/g:versioned",
            Some(&token),
            Some(json!({"name": "Renamed"})),
            &[],
        )
        .await?;
    assert_eq!(unversioned.0, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(text(&unversioned.1["error"]["hint"]).contains("dm groups show"));
    let bad_version = app
        .call(
            Method::PATCH,
            "/api/v1/groups/g:versioned",
            Some(&token),
            Some(json!({"name": "Renamed"})),
            &[("if-match", "latest".to_owned())],
        )
        .await?;
    assert!(
        text(&bad_version.1["error"]["message"])
            .starts_with("If-Match must be the version you last read"),
        "{}",
        bad_version.1
    );
    app.close().await
}

#[tokio::test]
async fn the_first_contact_window_says_when_the_next_conversation_fits() -> Result<(), BoxError> {
    let app = TestApp::start().await?;
    let (ada, bob, cos) = people(&app).await;
    for to in ["c:bob", "si:cos"] {
        expect_status(
            &app.post(
                "/api/v1/messages",
                &app.accounts.token(&ada),
                json!({"recipient_id": to, "message": "first contact"}),
            )
            .await?,
            StatusCode::ACCEPTED,
        )?;
    }
    // Two conversations started this hour: at a limit of two, the next fits in about an hour;
    // at three, now.
    let at_limit = app.state.store.first_contact_wait(&ada.actor(), 2).await?;
    assert!(
        at_limit.is_some_and(|wait| (3500.0..=3600.0).contains(&wait)),
        "{at_limit:?}"
    );
    assert_eq!(
        app.state.store.first_contact_wait(&ada.actor(), 3).await?,
        None
    );
    drop((bob, cos));
    app.close().await
}

#[tokio::test]
async fn the_report_limit_is_configured_and_says_when_to_retry() -> Result<(), BoxError> {
    let database = support::TestDatabase::start().await?;
    let app = TestApp::start_configured(database, "", |settings| {
        settings.reports_per_hour = 2;
    })
    .await?;
    let ada = TestAccount::carbon("a8K", "c:ada");
    app.accounts.add_accounts(&[&ada]).await;
    let token = app.accounts.token(&ada);
    for n in 1..=2 {
        let sent = app
            .post(
                "/api/v1/reports",
                &token,
                json!({"message": format!("report {n}"), "client_version": "dm 0.17.0"}),
            )
            .await?;
        expect_status(&sent, StatusCode::ACCEPTED)?;
    }
    let (status, headers, body) = app
        .post_envelope(
            "/api/v1/reports",
            &token,
            &json!({"type": "report", "data": {"message": "a third", "client_version": "dm 0.17.0"}}),
        )
        .await?;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    let body = data_of(&body);
    assert_eq!(body["error"]["code"], "rate_limited");
    assert_eq!(body["error"]["details"]["limit"], 2);
    let retry: u64 = headers["retry-after"].to_str()?.parse()?;
    assert!((3000..=3600).contains(&retry), "{retry}");
    assert_eq!(body["error"]["details"]["retry_after_seconds"], retry);
    assert!(
        text(&body["error"]["message"]).contains("sent 2 bug reports in the last hour"),
        "{body}"
    );
    // Empty and oversized reports say which rule they break.
    let empty = app
        .post(
            "/api/v1/reports",
            &token,
            json!({"message": "  ", "client_version": "dm 0.17.0"}),
        )
        .await?;
    assert!(text(&empty.1["error"]["message"]).contains("this one is empty"));
    app.close().await
}

#[tokio::test]
async fn a_silicons_account_reference_names_its_custodian() -> Result<(), BoxError> {
    let app = TestApp::start().await?;
    let (ada, bob, cos) = people(&app).await;
    // Both sign in once, so DM knows their ids and names (as the website and the CLI do).
    for account in [&ada, &cos] {
        expect_status(
            &app.get("/api/v1/me", &app.accounts.token(account)).await?,
            StatusCode::OK,
        )?;
    }
    let sent = app
        .post(
            "/api/v1/messages",
            &app.accounts.token(&cos),
            json!({"recipient_id": "c:ada", "message": "report ready"}),
        )
        .await?;
    expect_status(&sent, StatusCode::ACCEPTED)?;
    let custodian = &sent.1["sender"]["custodian"];
    assert_eq!(custodian["type"], "carbon", "{}", sent.1);
    assert_eq!(custodian["id"], "c:ada");
    assert_eq!(custodian["uuid"], "a8K");
    assert!(custodian.get("custodian").is_none(), "one level deep");
    // Everywhere an account is named: participants, group members and owners.
    let conversation = app
        .get(
            "/api/v1/conversations/c:ada::si:cos",
            &app.accounts.token(&ada),
        )
        .await?;
    expect_status(&conversation, StatusCode::OK)?;
    let participants = conversation.1["participants"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let silicon = participants
        .iter()
        .find(|account| account["id"] == "si:cos")
        .ok_or("si:cos is a participant")?;
    assert_eq!(silicon["custodian"]["id"], "c:ada");
    let carbon = participants
        .iter()
        .find(|account| account["id"] == "c:ada")
        .ok_or("c:ada is a participant")?;
    assert!(carbon.get("custodian").is_none(), "a Carbon has none");
    let group = app
        .post(
            "/api/v1/groups",
            &app.accounts.token(&ada),
            json!({"name": "Custodians", "member_ids": ["si:cos"]}),
        )
        .await?;
    expect_status(&group, StatusCode::CREATED)?;
    let details = app
        .get("/api/v1/groups/g:custodians", &app.accounts.token(&ada))
        .await?;
    let members = details.1["group"]["members"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        members
            .iter()
            .any(|member| member["id"] == "si:cos" && member["custodian"]["id"] == "c:ada"),
        "{members:?}"
    );
    drop(bob);
    app.close().await
}

#[tokio::test]
async fn everyday_refusals_say_what_to_do_next() -> Result<(), BoxError> {
    let app = TestApp::start().await?;
    let (ada, bob, _) = people(&app).await;
    let ada_token = app.accounts.token(&ada);
    let bob_token = app.accounts.token(&bob);
    let sent = app
        .post(
            "/api/v1/messages",
            &ada_token,
            json!({"recipient_id": "c:bob", "message": "mine"}),
        )
        .await?;
    let code = sent.1["message-id"].as_str().unwrap_or_default().to_owned();
    // A receipt on your own message.
    let own = app
        .post(
            &format!("/api/v1/conversations/c:ada::c:bob/messages/{code}/receipts"),
            &ada_token,
            json!({"status": "delivered", "device_id": "ada-phone"}),
        )
        .await?;
    assert_eq!(own.0, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        text(&own.1["error"]["hint"]).contains("Skip your own messages"),
        "{}",
        own.1
    );
    // A cursor that was changed, a cursor with reset, a wait that is too long.
    for (query, needle) in [
        ("cursor=not-a-cursor", "reset=true"),
        ("cursor=abc.def&reset=true", "not both"),
        ("wait=31", "wait=30"),
    ] {
        let refused = app
            .get(&format!("/api/v1/sync?{query}"), &bob_token)
            .await?;
        assert_eq!(refused.0, StatusCode::UNPROCESSABLE_ENTITY, "{query}");
        assert!(
            text(&refused.1["error"]["hint"]).contains(needle),
            "{query}: {}",
            refused.1
        );
    }
    // A voice note without its length, and a transcript without its recording.
    for (body, needle) in [
        (
            json!({"recipient_id": "c:bob", "voice": {"permanent_url": "https://files.example/n.webm"}}),
            "duration_milliseconds",
        ),
        (
            json!({"recipient_id": "c:bob", "message": 7}),
            "MessageCreate",
        ),
        (
            json!({"recipient_id": "c:bob", "message": "hi", "voice_transcript": "hi"}),
            "voice note",
        ),
    ] {
        let refused = app.post("/api/v1/messages", &ada_token, body).await?;
        assert_eq!(refused.0, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            text(&refused.1["error"]["hint"]).contains(needle),
            "{}",
            refused.1
        );
    }
    // Messaging yourself, and a token that is not DM's.
    let yourself = app
        .post(
            "/api/v1/messages",
            &ada_token,
            json!({"recipient_id": "c:ada", "message": "me"}),
        )
        .await?;
    assert!(
        text(&yourself.1["error"]["hint"]).contains("draft"),
        "{}",
        yourself.1
    );
    let stranger = app.get("/api/v1/me", "not-a-token").await?;
    assert_eq!(stranger.0, StatusCode::UNAUTHORIZED);
    assert!(
        text(&stranger.1["error"]["hint"]).contains("dm login"),
        "{}",
        stranger.1
    );
    app.close().await
}
