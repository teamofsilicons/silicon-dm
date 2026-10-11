//! The routes outside conversations and groups: GIF discovery and each Carbon's recent GIFs,
//! bug reports, client diagnostics and contract discovery; and the rule that every API
//! route needs the account's own Silicon Accounts sign-in, with another app's proof
//! accepted only where its scopes reach.
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
use wiremock::{
    Mock, ResponseTemplate,
    matchers::{method, path, query_param},
};

/// Every authenticated route family of contract 4, with a body where one is needed.
fn route_families() -> Vec<(Method, &'static str, Option<Value>)> {
    vec![
        (Method::GET, "/api/v1/me", None),
        (Method::GET, "/api/v1/delivery", None),
        (Method::GET, "/api/v1/sync", None),
        (Method::GET, "/api/v1/conversations", None),
        (
            Method::POST,
            "/api/v1/conversations",
            Some(json!({"participant_ids": ["c:ada"]})),
        ),
        (
            Method::POST,
            "/api/v1/messages",
            Some(json!({"recipient_id": "c:ada", "message": "hi"})),
        ),
        (Method::GET, "/api/v1/conversations/c:ada/messages", None),
        (Method::GET, "/api/v1/conversations/c:ada/draft", None),
        (Method::GET, "/api/v1/groups/g:missing", None),
        (Method::POST, "/api/v1/groups", Some(json!({"name": "Ops"}))),
        (Method::GET, "/api/v1/invitations", None),
        (Method::GET, "/api/v1/allow-list", None),
        (Method::GET, "/api/v1/presence/c:ada", None),
        (Method::DELETE, "/api/v1/presence/devices/laptop", None),
        (Method::GET, "/api/v1/gifs/trending", None),
        (Method::GET, "/api/v1/gifs/search?q=party", None),
        (Method::GET, "/api/v1/gifs/recent", None),
        (
            Method::POST,
            "/api/v1/reports",
            Some(json!({"message": "broken", "client_version": "dm 0.15.0"})),
        ),
    ]
}

#[tokio::test]
async fn every_api_route_needs_the_accounts_own_sign_in() -> Result<(), BoxError> {
    let app = TestApp::start().await?;
    let ada = TestAccount::carbon("a8K", "c:ada");
    app.accounts.add_account(&ada).await;
    app.accounts.invalid_proofs().await;
    let elsewhere = app.accounts.token_with(&ada, &json!({"aud": "briefcase"}));
    for (verb, route, body) in route_families() {
        // No credential at all.
        let anonymous = app
            .call(verb.clone(), route, None, body.clone(), &[])
            .await?;
        assert_eq!(
            anonymous.0,
            StatusCode::UNAUTHORIZED,
            "{verb} {route}: {}",
            anonymous.1
        );
        assert_eq!(
            anonymous.1["error"]["code"], "unauthorized",
            "{verb} {route}"
        );
        // A Silicon Accounts token issued to another app.
        let foreign = app
            .call(verb.clone(), route, Some(&elsewhere), body.clone(), &[])
            .await?;
        assert_eq!(
            foreign.1["error"]["code"], "invalid_token",
            "{verb} {route}: {}",
            foreign.1
        );
        // A proof on a route no proof scope reaches is refused before Accounts is asked.
        let proof = app
            .call(
                verb.clone(),
                route,
                None,
                body.clone(),
                &[("authorization", "Proof sap_from_somewhere".to_owned())],
            )
            .await?;
        // Routes a proof scope reaches ask Accounts (and this proof is not valid); the rest
        // refuse a proof outright.
        let expected = match (verb.as_str(), route) {
            (
                "GET",
                "/api/v1/sync"
                | "/api/v1/conversations"
                | "/api/v1/conversations/c:ada/messages"
                | "/api/v1/conversations/c:ada/draft"
                | "/api/v1/groups/g:missing",
            )
            | ("POST", "/api/v1/conversations" | "/api/v1/messages") => "invalid_proof",
            _ => "proof_not_accepted",
        };
        assert_eq!(
            proof.1["error"]["code"], expected,
            "{verb} {route}: {}",
            proof.1
        );
    }
    // Discovery and contract negotiation are the only API routes without a sign-in.
    expect_status(
        &app.call(Method::GET, "/api/v1/accounts", None, None, &[])
            .await?,
        StatusCode::OK,
    )?;
    let contracts = app
        .call(Method::GET, "/api/v1/contracts", None, None, &[])
        .await?;
    expect_status(&contracts, StatusCode::OK)?;
    assert_eq!(contracts.1["identity"]["provider"], "silicon-accounts");
    assert_eq!(contracts.1["identity"]["app_id"], "dm");
    assert_eq!(contracts.1["compatibility"][0]["http"], 4);
    // An explicit older contract is refused with the versions DM speaks.
    let old = app
        .call(
            Method::GET,
            "/api/v1/me",
            Some(&app.accounts.token(&ada)),
            None,
            &[("x-dm-contract-version", "3".to_owned())],
        )
        .await?;
    assert_eq!(old.0, StatusCode::NOT_ACCEPTABLE, "{}", old.1);
    assert_eq!(old.1["error"]["code"], "unsupported_contract");
    // The IAM-era routes are gone.
    for retired in [
        "/api/v1/iam",
        "/api/v1/auth/me",
        "/api/v1/testing-environments",
        "/api/v1/delivery/authorization",
    ] {
        let gone = app
            .call(
                Method::GET,
                retired,
                Some(&app.accounts.token(&ada)),
                None,
                &[],
            )
            .await?;
        assert_eq!(gone.0, StatusCode::NOT_FOUND, "{retired}: {}", gone.1);
    }
    app.close().await
}

#[tokio::test]
async fn gifs_are_discovered_through_giphy_and_recent_ones_belong_to_each_carbon()
-> Result<(), BoxError> {
    let app = TestApp::start().await?;
    let ada = TestAccount::carbon("a8K", "c:ada");
    let bob = TestAccount::carbon("Bb2", "c:bob");
    let cos = TestAccount::silicon("K1E", "si:cos", &ada);
    app.accounts.add_accounts(&[&ada, &bob, &cos]).await;
    let found = json!({
        "data": [{"id": "party-1", "title": "Party", "images": {
            "original": {"url": "https://media.giphy.com/party.gif"},
            "fixed_width_small": {"url": "https://media.giphy.com/party-small.gif"}}}],
        "meta": {"status": 200}
    });
    Mock::given(method("GET"))
        .and(path("/giphy/trending"))
        .and(query_param("api_key", "giphy-test"))
        .and(query_param("rating", "g"))
        .respond_with(ResponseTemplate::new(200).set_body_json(found.clone()))
        .mount(&app.accounts.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/giphy/search"))
        .and(query_param("q", "party"))
        .respond_with(ResponseTemplate::new(200).set_body_json(found))
        .mount(&app.accounts.server)
        .await;
    let ada_token = app.accounts.token(&ada);
    let trending = app.get("/api/v1/gifs/trending", &ada_token).await?;
    expect_status(&trending, StatusCode::OK)?;
    assert_eq!(trending.1["items"][0]["provider_id"], "party-1");
    // Silicons may search too.
    let search = app
        .get("/api/v1/gifs/search?q=party", &app.accounts.token(&cos))
        .await?;
    expect_status(&search, StatusCode::OK)?;
    assert_eq!(
        search.1["items"][0]["preview_url"],
        "https://media.giphy.com/party-small.gif"
    );
    let bad = app.get("/api/v1/gifs/search?q=%20", &ada_token).await?;
    assert_eq!(bad.0, StatusCode::UNPROCESSABLE_ENTITY, "{}", bad.1);

    // c:ada sends a GIF to c:bob: it joins c:ada's recent GIFs, nobody else's.
    let sent = app
        .post(
            "/api/v1/messages",
            &ada_token,
            json!({"recipient_id": "c:bob", "gif": {
                "provider_id": "party-1", "url": "https://media.giphy.com/party.gif", "title": "Party"}}),
        )
        .await?;
    expect_status(&sent, StatusCode::ACCEPTED)?;
    let recent = app.get("/api/v1/gifs/recent", &ada_token).await?;
    expect_status(&recent, StatusCode::OK)?;
    assert_eq!(recent.1["items"][0]["provider_id"], "party-1");
    let others = app
        .get("/api/v1/gifs/recent", &app.accounts.token(&bob))
        .await?;
    assert_eq!(others.1["items"].as_array().map(Vec::len), Some(0));
    // A Silicon's GIFs are not remembered (recent GIFs are a Carbon convenience).
    let to_ada = app
        .post(
            "/api/v1/messages",
            &app.accounts.token(&cos),
            json!({"recipient_id": "c:ada", "gif": {
                "provider_id": "party-1", "url": "https://media.giphy.com/party.gif"}}),
        )
        .await?;
    expect_status(&to_ada, StatusCode::ACCEPTED)?;
    let silicon = app
        .get("/api/v1/gifs/recent", &app.accounts.token(&cos))
        .await?;
    assert_eq!(silicon.1["items"].as_array().map(Vec::len), Some(0));
    app.close().await
}

#[tokio::test]
async fn reports_and_diagnostics_are_kept_per_account() -> Result<(), BoxError> {
    let app = TestApp::start().await?;
    let ada = TestAccount::carbon("a8K", "c:ada");
    let bob = TestAccount::carbon("Bb2", "c:bob");
    app.accounts.add_accounts(&[&ada, &bob]).await;
    let ada_token = app.accounts.token(&ada);
    let report = json!({"message": "the inbox froze", "client_version": "dm 0.15.0"});
    let key = ("idempotency-key", "report-1".to_owned());
    let first = app
        .call(
            Method::POST,
            "/api/v1/reports",
            Some(&ada_token),
            Some(report.clone()),
            std::slice::from_ref(&key),
        )
        .await?;
    expect_status(&first, StatusCode::ACCEPTED)?;
    // Without Postmark (outside production) the report is stored and marked simulated.
    assert_eq!(first.1["notification"], "simulated");
    // The same key replays the same report; other content under it is a conflict.
    let replay = app
        .call(
            Method::POST,
            "/api/v1/reports",
            Some(&ada_token),
            Some(report.clone()),
            std::slice::from_ref(&key),
        )
        .await?;
    assert_eq!(replay.1["id"], first.1["id"]);
    let conflict = app
        .call(
            Method::POST,
            "/api/v1/reports",
            Some(&ada_token),
            Some(json!({"message": "something else", "client_version": "dm 0.15.0"})),
            std::slice::from_ref(&key),
        )
        .await?;
    assert_eq!(conflict.0, StatusCode::CONFLICT, "{}", conflict.1);
    // Keys are per account: c:bob's report-1 is his own report.
    let bobs = app
        .call(
            Method::POST,
            "/api/v1/reports",
            Some(&app.accounts.token(&bob)),
            Some(report.clone()),
            std::slice::from_ref(&key),
        )
        .await?;
    expect_status(&bobs, StatusCode::ACCEPTED)?;
    assert_ne!(bobs.1["id"], first.1["id"]);
    let stored: Vec<(String, String)> = sqlx::query_as(
        "SELECT actor_id, organization_id FROM bug_reports ORDER BY created_at, actor_id",
    )
    .fetch_all(app.state.store.pool())
    .await?;
    assert_eq!(
        stored,
        vec![
            ("a8K".to_owned(), "@accounts".to_owned()),
            ("Bb2".to_owned(), "@accounts".to_owned())
        ]
    );
    // Ten reports an hour per account.
    for n in 2..=10 {
        let more = app
            .call(
                Method::POST,
                "/api/v1/reports",
                Some(&ada_token),
                Some(json!({"message": format!("report {n}"), "client_version": "dm 0.15.0"})),
                &[],
            )
            .await?;
        expect_status(&more, StatusCode::ACCEPTED)?;
    }
    let limited = app
        .call(
            Method::POST,
            "/api/v1/reports",
            Some(&ada_token),
            Some(json!({"message": "one too many", "client_version": "dm 0.15.0"})),
            &[],
        )
        .await?;
    assert_eq!(limited.0, StatusCode::TOO_MANY_REQUESTS, "{}", limited.1);

    // DM records no telemetry: the diagnostics route older clients post to is gone.
    let telemetry = app
        .call(
            Method::POST,
            "/api/v1/telemetry",
            Some(&ada_token),
            Some(json!({"source": "cli", "event": "command", "success": true, "duration_ms": 5})),
            &[],
        )
        .await?;
    assert_eq!(telemetry.0, StatusCode::NOT_FOUND, "{}", telemetry.1);
    app.close().await
}
