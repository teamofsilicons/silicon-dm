//! Every API route authenticates with Silicon Accounts: access tokens issued to DM,
//! verified locally against the JWKS (refetched for an unknown key), sign-in cutoffs from
//! webhooks, introspection on sensitive routes, and User verification proofs from allowed
//! apps on the routes that accept them.
#![allow(
    clippy::too_many_lines,
    reason = "each test is one scenario, read top to bottom"
)]
mod support;

use axum::http::{Method, StatusCode};
use serde_json::json;
use support::{
    BoxError,
    accounts::{TestAccount, TestKey, now, webhook},
    app::{TestApp, expect_status},
};

#[tokio::test]
async fn tokens_are_verified_locally_with_exact_failures() -> Result<(), BoxError> {
    let app = TestApp::start().await?;
    let ada = TestAccount::carbon("a8K", "c:ada");
    app.accounts.add_account(&ada).await;

    // A valid token is accepted and the account is remembered with its current id.
    let me = app.get("/api/v1/me", &app.accounts.token(&ada)).await?;
    expect_status(&me, StatusCode::OK)?;
    assert_eq!(me.1["account"]["id"], "c:ada");
    assert_eq!(me.1["account"]["uuid"], "a8K");
    assert_eq!(me.1["account"]["type"], "carbon");

    let cases = [
        ("missing", None, "unauthorized"),
        (
            "wrong audience",
            Some(app.accounts.token_with(&ada, &json!({"aud": "briefcase"}))),
            "invalid_token",
        ),
        (
            "wrong issuer",
            Some(
                app.accounts
                    .token_with(&ada, &json!({"iss": "https://accounts.example.test"})),
            ),
            "invalid_token",
        ),
        (
            "expired",
            Some(
                app.accounts
                    .token_with(&ada, &json!({"exp": now() - 3600, "iat": now() - 7200})),
            ),
            "token_expired",
        ),
        (
            "not yet valid",
            Some(app.accounts.token_with(&ada, &json!({"nbf": now() + 3600}))),
            "invalid_token",
        ),
        ("not a JWT", Some("not-a-token".to_owned()), "invalid_token"),
        (
            "no kind",
            Some(app.accounts.token_with(&ada, &json!({"kind": null}))),
            "invalid_token",
        ),
    ];
    for (name, token, code) in cases {
        let response = app
            .call(Method::GET, "/api/v1/me", token.as_deref(), None, &[])
            .await?;
        assert_eq!(
            response.0,
            StatusCode::UNAUTHORIZED,
            "{name}: {}",
            response.1
        );
        assert_eq!(response.1["error"]["code"], code, "{name}: {}", response.1);
    }
    let wrong_audience = app
        .call(
            Method::GET,
            "/api/v1/me",
            Some(&app.accounts.token_with(&ada, &json!({"aud": "briefcase"}))),
            None,
            &[],
        )
        .await?;
    assert!(
        wrong_audience.1["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("issued to `dm`")),
        "{}",
        wrong_audience.1
    );

    // A token signed with a key the JWKS does not have (even after refetching) is refused.
    let stranger = TestKey::new(9, "other-key");
    let forged = stranger.sign(&app.accounts.claims(&ada));
    let refused = app
        .call(Method::GET, "/api/v1/me", Some(&forged), None, &[])
        .await?;
    assert_eq!(refused.0, StatusCode::UNAUTHORIZED);
    assert!(
        refused.1["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("other-key"))
    );
    // Same kid, different key: a bad signature.
    let tampered = TestKey::new(9, "test-key-1").sign(&app.accounts.claims(&ada));
    let refused = app
        .call(Method::GET, "/api/v1/me", Some(&tampered), None, &[])
        .await?;
    assert_eq!(refused.0, StatusCode::UNAUTHORIZED);
    app.close().await
}

#[tokio::test]
async fn a_rotated_signing_key_is_picked_up_from_the_jwks() -> Result<(), BoxError> {
    let app = TestApp::start().await?;
    let ada = TestAccount::carbon("a8K", "c:ada");
    app.accounts.add_account(&ada).await;
    // DM caches the first key set.
    expect_status(
        &app.get("/api/v1/me", &app.accounts.token(&ada)).await?,
        StatusCode::OK,
    )?;
    // Accounts rotates: the JWKS now also publishes a new key; DM refetches on the unknown kid.
    let rotated = TestKey::new(11, "test-key-2");
    app.accounts.server.reset().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/.well-known/jwks.json"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(json!({
            "keys": [app.accounts.key.jwk, rotated.jwk]
        })))
        .mount(&app.accounts.server)
        .await;
    app.accounts.add_account(&ada).await;
    let token = rotated.sign(&app.accounts.claims(&ada));
    expect_status(&app.get("/api/v1/me", &token).await?, StatusCode::OK)?;
    app.close().await
}

#[tokio::test]
async fn sign_outs_cut_off_older_tokens_except_dms_own_revocation() -> Result<(), BoxError> {
    let app = TestApp::start().await?;
    let ada = TestAccount::carbon("a8K", "c:ada");
    app.accounts.add_account(&ada).await;
    let earlier = app
        .accounts
        .token_with(&ada, &json!({"iat": now() - 120, "nbf": now() - 120}));
    expect_status(&app.get("/api/v1/me", &earlier).await?, StatusCode::OK)?;

    // DM revoked one refresh family itself (one machine's `dm logout`): nothing else ends.
    let event = time::OffsetDateTime::now_utc();
    let at = event.format(&time::format_description::well_known::Rfc3339)?;
    let (headers, body) = webhook(
        "membership.signed_out",
        &json!({"uuid": "a8K", "membership_id": "dm:a8K", "reason": "app_revoked"}),
        &at,
    );
    assert_eq!(app.deliver(&headers, &body).await?, StatusCode::NO_CONTENT);
    expect_status(&app.get("/api/v1/me", &earlier).await?, StatusCode::OK)?;

    // The STK was rotated: every token issued before the event is refused from now on.
    let (headers, body) = webhook(
        "membership.signed_out",
        &json!({"uuid": "a8K", "membership_id": "dm:a8K", "reason": "stk_rotated"}),
        &at,
    );
    assert_eq!(app.deliver(&headers, &body).await?, StatusCode::NO_CONTENT);
    let refused = app.get("/api/v1/me", &earlier).await?;
    assert_eq!(refused.0, StatusCode::UNAUTHORIZED);
    assert_eq!(refused.1["error"]["code"], "session_revoked");
    // A sign-in after the event works, even one in the same second (iat has whole seconds:
    // a Silicon that rotates its STK and signs straight back in is not refused); Silicon
    // Accounts says whether a same-second token's sign-in is the one that ended.
    let same_second = app.accounts.token_with(
        &ada,
        &json!({"iat": event.unix_timestamp(), "nbf": event.unix_timestamp() - 1}),
    );
    app.accounts
        .introspection_of(&same_second, true, "a8K", Some(1))
        .await;
    expect_status(&app.get("/api/v1/me", &same_second).await?, StatusCode::OK)?;
    let later = app
        .accounts
        .token_with(&ada, &json!({"iat": now() + 1, "nbf": now() - 1}));
    expect_status(&app.get("/api/v1/me", &later).await?, StatusCode::OK)?;
    // One second earlier is certainly before the sign-out.
    let second_before = app.accounts.token_with(
        &ada,
        &json!({"iat": event.unix_timestamp() - 1, "nbf": event.unix_timestamp() - 2}),
    );
    let refused = app.get("/api/v1/me", &second_before).await?;
    assert_eq!(refused.1["error"]["code"], "session_revoked");
    app.close().await
}

#[tokio::test]
async fn a_token_from_the_cutoffs_own_second_is_decided_by_silicon_accounts() -> Result<(), BoxError>
{
    let app = TestApp::start().await?;
    let ada = TestAccount::carbon("a8K", "c:ada");
    app.accounts.add_account(&ada).await;
    let event = time::OffsetDateTime::now_utc();
    let second = event.unix_timestamp();
    // Two tokens from the second Ada removed DM's access in: one from the sign-in that the
    // removal ended, one from a sign-in made just after it. `iat` cannot tell them apart.
    let ended = app
        .accounts
        .token_with(&ada, &json!({"iat": second, "nbf": second - 1}));
    let fresh = app
        .accounts
        .token_with(&ada, &json!({"iat": second, "nbf": second - 1}));
    app.accounts
        .introspection_of(&ended, false, "a8K", Some(1))
        .await;
    app.accounts
        .introspection_of(&fresh, true, "a8K", Some(1))
        .await;
    // Before any sign-out nothing needs asking.
    expect_status(&app.get("/api/v1/me", &ended).await?, StatusCode::OK)?;
    let at = event.format(&time::format_description::well_known::Rfc3339)?;
    let (headers, body) = webhook(
        "membership.access_removed",
        &json!({"uuid": "a8K", "membership_id": "dm:a8K"}),
        &at,
    );
    assert_eq!(app.deliver(&headers, &body).await?, StatusCode::NO_CONTENT);
    for _ in 0..2 {
        let refused = app.get("/api/v1/me", &ended).await?;
        assert_eq!(refused.0, StatusCode::UNAUTHORIZED, "{}", refused.1);
        assert_eq!(refused.1["error"]["code"], "session_revoked");
        expect_status(&app.get("/api/v1/me", &fresh).await?, StatusCode::OK)?;
    }
    // Each token was asked about once: the answer is kept for that token and cutoff.
    app.accounts.server.verify().await;
    app.close().await
}

#[tokio::test]
async fn sensitive_routes_never_rest_on_a_cached_live_answer() -> Result<(), BoxError> {
    let app = TestApp::start().await?;
    let ada = TestAccount::carbon("a8K", "c:ada");
    let bob = TestAccount::carbon("B0b", "c:bob");
    let cos = TestAccount::silicon("K1E", "si:cos", &ada);
    app.accounts.add_accounts(&[&ada, &bob, &cos]).await;
    let token = app.accounts.token(&cos);
    // Live for the first change; then the sign-in ends (DM's own sign-out sends no cutoff,
    // so only Silicon Accounts knows).
    app.accounts
        .introspection_of(&token, true, "K1E", Some(1))
        .await;
    app.accounts
        .introspection_of(&token, false, "K1E", None)
        .await;
    let allowed = app
        .post(
            "/api/v1/allow-list",
            &token,
            json!({"account_ids": ["c:bob"]}),
        )
        .await?;
    expect_status(&allowed, StatusCode::OK)?;
    let refused = app
        .call(
            Method::DELETE,
            "/api/v1/allow-list",
            Some(&token),
            Some(json!({"account_ids": ["c:bob"]})),
            &[],
        )
        .await?;
    assert_eq!(refused.0, StatusCode::UNAUTHORIZED, "{}", refused.1);
    assert_eq!(refused.1["error"]["code"], "session_revoked");
    app.close().await
}

#[tokio::test]
async fn sensitive_routes_ask_accounts_whether_the_sign_in_is_still_live() -> Result<(), BoxError> {
    let app = TestApp::start().await?;
    let ada = TestAccount::carbon("a8K", "c:ada");
    let cos = TestAccount::silicon("K1E", "si:cos", &ada);
    app.accounts.add_accounts(&[&ada, &cos]).await;
    app.accounts.introspection(false, "K1E").await;
    // Allow-list changes are sensitive: introspection says the sign-in ended.
    let refused = app
        .post(
            "/api/v1/allow-list",
            &app.accounts.token(&cos),
            json!({"account_ids": ["c:ada"]}),
        )
        .await?;
    assert_eq!(refused.0, StatusCode::UNAUTHORIZED, "{}", refused.1);
    assert_eq!(refused.1["error"]["code"], "session_revoked");
    // Ordinary reads never ask.
    expect_status(
        &app.get("/api/v1/allow-list", &app.accounts.token(&cos))
            .await?,
        StatusCode::OK,
    )?;
    app.close().await
}

#[tokio::test]
async fn proofs_from_allowed_apps_act_for_the_account_on_their_scopes_only() -> Result<(), BoxError>
{
    let app = TestApp::start().await?;
    let ada = TestAccount::carbon("a8K", "c:ada");
    let bob = TestAccount::carbon("Bb2", "c:bob");
    app.accounts.add_accounts(&[&ada, &bob]).await;
    app.accounts
        .proof(
            "sap_interface_ada",
            &ada,
            "interface",
            &[
                "dm.messages.write",
                "dm.messages.read",
                "dm.conversations.read",
            ],
        )
        .await;
    app.accounts
        .proof(
            "sap_stranger_ada",
            &ada,
            "stranger-app",
            &["dm.messages.write"],
        )
        .await;
    app.accounts
        .proof("sap_readonly_ada", &ada, "interface", &["dm.messages.read"])
        .await;
    app.accounts.invalid_proofs().await;
    let with_proof = |proof: &str| vec![("authorization", format!("Proof {proof}"))];

    // Interface sends for c:ada (allowed issuer, scope present).
    let sent = app
        .call(
            Method::POST,
            "/api/v1/messages",
            None,
            Some(json!({"recipient_id": "c:bob", "message": "hi from Interface"})),
            &with_proof("sap_interface_ada"),
        )
        .await?;
    expect_status(&sent, StatusCode::ACCEPTED)?;
    assert_eq!(sent.1["sender"]["id"], "c:ada");
    assert_eq!(sent.1["sender"]["uuid"], "a8K");
    let stored: Option<String> =
        sqlx::query_scalar("SELECT via_app FROM messages ORDER BY created_at DESC LIMIT 1")
            .fetch_one(app.state.store.pool())
            .await?;
    assert_eq!(
        stored.as_deref(),
        Some("interface"),
        "the issuing app is recorded"
    );
    let listed = app
        .call(
            Method::GET,
            "/api/v1/conversations",
            None,
            None,
            &with_proof("sap_interface_ada"),
        )
        .await?;
    expect_status(&listed, StatusCode::OK)?;
    assert_eq!(listed.1["items"].as_array().map(Vec::len), Some(1));

    // An issuer DM_PROOF_ISSUERS does not list.
    let refused = app
        .call(
            Method::POST,
            "/api/v1/messages",
            None,
            Some(json!({"recipient_id": "c:bob", "message": "x"})),
            &with_proof("sap_stranger_ada"),
        )
        .await?;
    assert_eq!(refused.0, StatusCode::FORBIDDEN);
    assert_eq!(refused.1["error"]["code"], "proof_issuer_not_allowed");
    // A scope the proof does not carry.
    let refused = app
        .call(
            Method::POST,
            "/api/v1/messages",
            None,
            Some(json!({"recipient_id": "c:bob", "message": "x"})),
            &with_proof("sap_readonly_ada"),
        )
        .await?;
    assert_eq!(refused.1["error"]["code"], "proof_scope_missing");
    // A route that takes no proofs at all.
    let refused = app
        .call(
            Method::POST,
            "/api/v1/groups",
            None,
            Some(json!({"name": "Team"})),
            &with_proof("sap_interface_ada"),
        )
        .await?;
    assert_eq!(refused.0, StatusCode::FORBIDDEN);
    assert_eq!(refused.1["error"]["code"], "proof_not_accepted");
    // A proof Accounts does not recognise.
    let refused = app
        .call(
            Method::GET,
            "/api/v1/conversations",
            None,
            None,
            &with_proof("sap_forged"),
        )
        .await?;
    assert_eq!(refused.0, StatusCode::UNAUTHORIZED);
    assert_eq!(refused.1["error"]["code"], "invalid_proof");
    app.close().await
}

#[tokio::test]
async fn discovery_needs_no_sign_in_and_names_accounts() -> Result<(), BoxError> {
    let app = TestApp::start().await?;
    let discovery = app
        .call(Method::GET, "/api/v1/accounts", None, None, &[])
        .await?;
    expect_status(&discovery, StatusCode::OK)?;
    assert_eq!(discovery.1["app_id"], "dm");
    assert_eq!(discovery.1["accounts_url"], app.accounts.issuer());
    assert_eq!(discovery.1["contract_version"], 4);
    assert_eq!(discovery.1["delivery"]["ting_enabled"], false);
    assert_eq!(discovery.1["delivery"]["sync_path"], "/api/v1/sync");
    // Contract 3 clients are told to upgrade; old IAM routes are gone.
    let old = app
        .call(
            Method::GET,
            "/api/v1/accounts",
            None,
            None,
            &[("x-dm-contract-version", "3".to_owned())],
        )
        .await?;
    assert_eq!(old.0, StatusCode::NOT_ACCEPTABLE);
    for path in [
        "/api/v1/iam",
        "/api/v1/auth/me",
        "/api/v1/testing-environments",
    ] {
        let gone = app.call(Method::GET, path, None, None, &[]).await?;
        assert_eq!(gone.0, StatusCode::NOT_FOUND, "{path}");
    }
    app.close().await
}
