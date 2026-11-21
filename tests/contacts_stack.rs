//! Run with the Silicon Contacts service and its Accounts fixture on loopback test ports.
//! The configured Contacts database must contain only the disposable Alice and Bob fixtures.
mod support;

use axum::http::{Method, StatusCode};
use serde_json::{Value, json};
use support::{BoxError, TestDatabase, accounts::TestAccount, app::TestApp};
use wiremock::{
    Mock, ResponseTemplate,
    matchers::{body_partial_json, method, path},
};

const ALICE: &str = "11111111-1111-4111-8111-111111111111";
const BOB: &str = "22222222-2222-4222-8222-222222222222";

async fn linked_proof(
    app: &TestApp,
    fixture: &str,
    account: Option<&str>,
    scopes: &[&str],
) -> Result<(), BoxError> {
    let kind = if account.is_some() {
        "user_verification"
    } else {
        "app_verification"
    };
    let route = if account.is_some() {
        "/v1/proofs/user-verification"
    } else {
        "/v1/proofs/app-verification"
    };
    let proof:Value = reqwest::Client::new().post(format!("{fixture}/test/proof")).json(&json!({"who":"alice","kind":kind,"issuing_app":"dm","receiving_app":"silicon-contacts","scopes":scopes})).send().await?.error_for_status()?.json().await?;
    let mut expected = json!({"receiving_app":"silicon-contacts","scopes":scopes});
    if let Some(subject) = account {
        expected["subject_token"] = json!(subject);
    }
    Mock::given(method("POST")).and(path(route)).and(body_partial_json(expected))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"proof_id":"prf_integration","kind":kind,"proof_token":proof["token"],"expires_at":"2099-01-01T00:00:00Z","issuing_app":"dm","receiving_app":"silicon-contacts","scopes":scopes})))
        .mount(&app.accounts.server).await;
    Ok(())
}

struct Interface {
    http: reqwest::Client,
    base: String,
    token: String,
}

impl Interface {
    async fn call(
        &self,
        method: Method,
        route: &str,
        body: Option<Value>,
        wanted: StatusCode,
    ) -> Result<Value, BoxError> {
        let kind = silicon_dm_protocol::http_type(method.as_str(), route);
        let mut request = self
            .http
            .request(method, format!("{}{route}", self.base))
            .bearer_auth(&self.token)
            .header("X-DM-Contract-Version", "4")
            .header("Idempotency-Key", uuid::Uuid::new_v4().to_string());
        if let Some(data) = body {
            request = request.json(&json!({"type":kind,"data":data}));
        }
        let r = request.send().await?;
        let status = r.status();
        let v: Value = r.json().await?;
        assert_eq!(status, wanted, "{route} {}", v["error"]);
        Ok(v.get("data").cloned().unwrap_or(v))
    }
}

async fn fixture_accounts(
    app: &TestApp,
    fixture: &str,
    http: &reqwest::Client,
) -> Result<(String, String, String), BoxError> {
    let alice = TestAccount::carbon(ALICE, "c:alice");
    let bob = TestAccount::silicon(BOB, "si:bob", &alice);
    app.accounts.add_accounts(&[&alice, &bob]).await;
    let alice_token = app.accounts.token(&alice);
    for perm in ["contacts.read", "contacts.write"] {
        linked_proof(app, fixture, Some(&alice_token), &[perm]).await?;
    }
    linked_proof(
        app,
        fixture,
        None,
        &[
            "dm.contact.check",
            &format!("recipient:{ALICE}"),
            &format!("sender:{BOB}"),
        ],
    )
    .await?;
    http.post(format!("{fixture}/test/profile"))
        .json(&json!({"who":"bob","changes":{"id":"si:bob"}}))
        .send()
        .await?
        .error_for_status()?;
    let native: Value = http
        .post(format!("{fixture}/test/app-access"))
        .json(&json!({"who":"alice"}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let access = native["access_token"]
        .as_str()
        .ok_or("missing Contacts fixture access")?;
    Ok((alice_token, app.accounts.token(&bob), access.to_owned()))
}

async fn interfaces(
    app: &TestApp,
    http: &reqwest::Client,
    alice_token: String,
    bob_token: String,
) -> Result<
    (
        Interface,
        Interface,
        tokio::task::JoinHandle<std::io::Result<()>>,
    ),
    BoxError,
> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let router = app.router.clone();
    let server = tokio::spawn(async move { axum::serve(listener, router).await });
    let a = Interface {
        http: http.clone(),
        base: base.clone(),
        token: alice_token,
    };
    let b = Interface {
        http: http.clone(),
        base,
        token: bob_token,
    };
    Ok((a, b, server))
}

#[tokio::test]
#[ignore = "requires a running local Silicon Contacts service, its disposable database and Accounts fixture"]
async fn standalone_dm_and_contacts_share_admission_and_accepted_requests() -> Result<(), BoxError>
{
    let service: u16 = std::env::var("DM_E2E_CONTACTS_PORT")?.parse()?;
    let fixture: u16 = std::env::var("DM_E2E_CONTACTS_FIXTURE_PORT")?.parse()?;
    let url: url::Url = format!("http://127.0.0.1:{service}").parse()?;
    let fixture = format!("http://127.0.0.1:{fixture}");
    let http = reqwest::Client::new();
    let contacts_origin = url.as_str().trim_end_matches('/').to_owned();
    let app = TestApp::start_configured(TestDatabase::start().await?, "", |s| s.contacts_url = url)
        .await?;
    let (alice_token, bob_token, access) = fixture_accounts(&app, &fixture, &http).await?;
    let record = format!("{contacts_origin}/v1/contacts/{BOB}");
    let remove = http.delete(&record).bearer_auth(&access).send().await?;
    assert!(remove.status().is_success() || remove.status() == StatusCode::NOT_FOUND);
    let (a, b, server) = interfaces(&app, &http, alice_token, bob_token).await?;
    a.call(
        Method::PUT,
        "/api/v1/direct-settings",
        Some(json!({"contacts_only":true})),
        StatusCode::OK,
    )
    .await?;
    let msg =
        || Some(json!({"recipient_id":"c:alice","message":"local cross-service verification"}));
    let held = b
        .call(
            Method::POST,
            "/api/v1/messages",
            msg(),
            StatusCode::CONFLICT,
        )
        .await?;
    assert_eq!(held["error"]["code"], "chat_request_pending");
    let requests = a
        .call(Method::GET, "/api/v1/chat-requests", None, StatusCode::OK)
        .await?;
    let request = &requests["items"][0];
    assert_eq!(request["sender"]["id"], "si:bob");
    a.call(
        Method::POST,
        "/api/v1/contacts",
        Some(json!({"id":"si:bob","name":"Research Bob","note":"private","trust":240})),
        StatusCode::OK,
    )
    .await?;
    let contact: Value = http
        .get(&record)
        .bearer_auth(&access)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(contact["trust"], 240);
    assert_eq!(contact["note"], "private");
    b.call(
        Method::POST,
        "/api/v1/messages",
        msg(),
        StatusCode::ACCEPTED,
    )
    .await?;
    let accepted = a
        .call(
            Method::PUT,
            &format!(
                "/api/v1/chat-requests/{}",
                request["id"].as_str().ok_or("id")?
            ),
            Some(json!({"accept":true})),
            StatusCode::OK,
        )
        .await?;
    assert!(accepted["conversation_id"].is_string());
    a.call(
        Method::DELETE,
        &format!("/api/v1/contacts/{BOB}"),
        None,
        StatusCode::OK,
    )
    .await?;
    b.call(
        Method::POST,
        "/api/v1/messages",
        msg(),
        StatusCode::ACCEPTED,
    )
    .await?;
    assert_eq!(
        http.get(&record)
            .bearer_auth(&access)
            .send()
            .await?
            .status(),
        StatusCode::NOT_FOUND
    );
    server.abort();
    app.close().await
}
