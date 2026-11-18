//! Direct admission, approvals and address-book suggestions through the public HTTP contract.
#![allow(
    clippy::too_many_lines,
    reason = "the request lifecycle is exercised in order"
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
    matchers::{body_json, header, method, path},
};

async fn preference(app: &TestApp, token: &str, enabled: bool) -> Result<(), BoxError> {
    let r = app
        .call(
            Method::PUT,
            "/api/v1/direct-settings",
            Some(token),
            Some(json!({"contacts_only":enabled})),
            &[],
        )
        .await?;
    expect_status(&r, StatusCode::OK)?;
    assert_eq!(r.1["contacts_only"], enabled);
    Ok(())
}

async fn pending(app: &TestApp, token: &str) -> Result<Vec<Value>, BoxError> {
    let r = app.get("/api/v1/chat-requests", token).await?;
    expect_status(&r, StatusCode::OK)?;
    Ok(r.1["items"].as_array().ok_or("no request list")?.clone())
}

#[tokio::test]
async fn contacts_only_requests_cover_new_and_existing_direct_threads() -> Result<(), BoxError> {
    let app = TestApp::start().await?;
    let ada = TestAccount::carbon("Ada", "c:ada");
    let bo = TestAccount::carbon("Bob", "c:bo");
    let scout = TestAccount::silicon("Sct", "si:scout", &ada);
    app.accounts.add_accounts(&[&ada, &bo, &scout]).await;
    app.accounts.issue_proofs().await;
    let a = app.accounts.token(&ada);
    let b = app.accounts.token(&bo);
    let s = app.accounts.token(&scout);
    let settings = app.get("/api/v1/direct-settings", &b).await?;
    expect_status(&settings, StatusCode::OK)?;
    assert_eq!(settings.1["contacts_only"], false);
    let opened = app
        .post(
            "/api/v1/conversations",
            &a,
            json!({"participant_ids":["c:bo"]}),
        )
        .await?;
    expect_status(&opened, StatusCode::CREATED)?;
    preference(&app, &b, true).await?;
    Mock::given(method("POST"))
        .and(path("/v1/integrations/dm/contact-check"))
        .and(header("Authorization", "Proof sap_app"))
        .and(body_json(json!({"recipient":"Bob","sender":"Ada"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"saved":false})))
        .mount(&app.accounts.server)
        .await;
    let thread = opened.1["id"].as_str().ok_or("thread id")?;
    let send_path = format!("/api/v1/conversations/{thread}/messages");
    for (path, body) in [
        (
            "/api/v1/messages",
            json!({"recipient_id":"c:bo","message":"private draft"}),
        ),
        (send_path.as_str(), json!({"message":"private draft"})),
        ("/api/v1/conversations", json!({"participant_ids":["c:bo"]})),
    ] {
        let refused = app.post(path, &a, body).await?;
        expect_status(&refused, StatusCode::CONFLICT)?;
        assert_eq!(refused.1["error"]["code"], "chat_request_pending");
    }
    let inbox = pending(&app, &b).await?;
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0]["sender"]["id"], "c:ada");
    let answer = format!(
        "/api/v1/chat-requests/{}",
        inbox[0]["id"].as_str().ok_or("request id")?
    );
    let accept = Some(json!({"accept":true}));
    let foreign = app
        .call(Method::PUT, &answer, Some(&s), accept.clone(), &[])
        .await?;
    expect_status(&foreign, StatusCode::NOT_FOUND)?;
    let accepted = app
        .call(Method::PUT, &answer, Some(&b), accept, &[])
        .await?;
    expect_status(&accepted, StatusCode::OK)?;
    assert!(accepted.1["conversation_id"].is_string());
    assert!(pending(&app, &b).await?.is_empty());
    let sent = app
        .post(&send_path, &a, json!({"message":"approved message"}))
        .await?;
    expect_status(&sent, StatusCode::ACCEPTED)?;
    let check = "/v1/integrations/dm/contact-check";
    Mock::given(method("POST"))
        .and(path(check))
        .and(body_json(json!({"recipient":"Bob","sender":"Sct"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"saved":false})))
        .mount(&app.accounts.server)
        .await;
    let declined = app
        .post(
            "/api/v1/conversations",
            &s,
            json!({"participant_ids":["c:bo"]}),
        )
        .await?;
    expect_status(&declined, StatusCode::CONFLICT)?;
    let inbox = pending(&app, &b).await?;
    let answer = format!(
        "/api/v1/chat-requests/{}",
        inbox[0]["id"].as_str().ok_or("request id")?
    );
    let declined = app
        .call(
            Method::PUT,
            &answer,
            Some(&b),
            Some(json!({"accept":false})),
            &[],
        )
        .await?;
    expect_status(&declined, StatusCode::OK)?;
    assert!(pending(&app, &b).await?.is_empty());
    Mock::given(method("POST"))
        .and(path(check))
        .and(body_json(json!({"recipient":"Bob","sender":"Sct"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"saved":true})))
        .with_priority(1)
        .mount(&app.accounts.server)
        .await;
    let saved = app
        .post(
            "/api/v1/conversations",
            &s,
            json!({"participant_ids":["c:bo"]}),
        )
        .await?;
    expect_status(&saved, StatusCode::CREATED)?;
    app.close().await
}

#[tokio::test]
async fn standalone_people_use_private_contact_names_trust_and_fuzzy_search() -> Result<(), BoxError>
{
    let app = TestApp::start().await?;
    let ada = TestAccount::carbon("Ada", "c:ada");
    app.accounts.add_account(&ada).await;
    app.accounts.issue_proofs().await;
    let token = app.accounts.token(&ada);
    let saved = |uuid: &str, id: &str, name: &str, trust: u8, blocked: bool| json!({"uuid":uuid,"name":name,"trust":trust,"blocked":blocked,"photo_id":"e15b0802-b722-4097-89c0-c86ebc40ef8d","account":{"uuid":uuid,"id":id,"kind":"silicon","display_name":"public","pfp_url":"https://example.test/photo.png"}});
    let contacts = json!({"items":[saved("Met","si:meteor","Meteor",3,false),saved("Mas","si:master","Master",190,false),saved("Mst","si:mister","Mister",255,true)],"total":3});
    Mock::given(method("GET"))
        .and(path("/v1/contacts"))
        .and(header("Authorization", "Proof sap_user"))
        .respond_with(ResponseTemplate::new(200).set_body_json(contacts))
        .mount(&app.accounts.server)
        .await;
    let result = app.get("/api/v1/people?q=mtr", &token).await?;
    expect_status(&result, StatusCode::OK)?;
    let matches = result.1["items"].as_array().ok_or("expected suggestions")?;
    assert_eq!(matches.len(), 2);
    assert_eq!(matches[0]["display_name"], "Master");
    assert_eq!(matches[0]["trust"], 190);
    assert!(matches[0]["photo_id"].is_string());
    assert_eq!(matches[1]["id"], "si:meteor");
    assert_eq!(
        matches[0]["pfp_url"],
        "/api/v1/contact-photos/e15b0802-b722-4097-89c0-c86ebc40ef8d"
    );
    Mock::given(method("GET"))
        .and(path("/v1/lookup"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"uuid":"Mas","id":"si:master","kind":"silicon","display_name":"Public account"}),
        ))
        .mount(&app.accounts.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/contacts/Mas"))
        .and(header("Authorization", "Proof sap_user"))
        .respond_with(ResponseTemplate::new(200).set_body_json(saved(
            "Mas",
            "si:master",
            "Master",
            190,
            false,
        )))
        .mount(&app.accounts.server)
        .await;
    let exact = app
        .get("/api/v1/people/lookup?q=si%3Amaster", &token)
        .await?;
    expect_status(&exact, StatusCode::OK)?;
    assert_eq!(exact.1, matches[0]);
    app.close().await
}
