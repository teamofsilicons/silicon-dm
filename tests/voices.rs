//! Voices: the public catalog, every account's deterministic default voice, choosing and
//! resetting it (the account itself or a Silicon's custodian), and the speech discovery block.
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
use silicon_dm::domain::VoiceCatalog;
use support::{
    BoxError,
    accounts::TestAccount,
    app::{TestApp, expect_status},
    deepgram::FakeDeepgram,
};

struct People {
    ada: TestAccount,
    bob: TestAccount,
    scout: TestAccount,
}

async fn people(app: &TestApp) -> People {
    let ada = TestAccount::carbon("a8K", "c:ada");
    let bob = TestAccount::carbon("Bb2", "c:bob");
    let scout = TestAccount::silicon("Sc0", "si:scout", &ada);
    app.accounts.add_accounts(&[&ada, &bob, &scout]).await;
    app.accounts.live_introspection().await;
    People { ada, bob, scout }
}

fn keys(catalog: &Value) -> Vec<String> {
    catalog["items"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item["key"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn the_catalog_is_public_and_every_account_has_a_deterministic_default_voice()
-> Result<(), BoxError> {
    let deepgram = FakeDeepgram::start().await;
    let app = TestApp::start_with_speech(&deepgram, |_| {}).await?;
    let p = people(&app).await;
    let catalog = app
        .call(Method::GET, "/api/v1/voices", None, None, &[])
        .await?;
    expect_status(&catalog, StatusCode::OK)?;
    assert_eq!(catalog.1["model"], "eleven_v4");
    assert_eq!(catalog.1["enabled"], true);
    assert_eq!(
        keys(&catalog.1),
        vec!["mark", "brittney", "jamal", "jessa", "hope"]
    );
    assert_eq!(catalog.1["items"][0]["name"], "Mark");
    assert_eq!(catalog.1["items"][0]["voice_id"], "UgBBYS2sOqTuMpoF3BR0");
    assert_eq!(catalog.1["items"][2]["name"], "Young Jamal");
    assert!(
        catalog.1["default_rule"]
            .as_str()
            .is_some_and(|rule| rule.contains("SHA-256")),
        "{}",
        catalog.1
    );

    let ada = app.accounts.token(&p.ada);
    let catalog = VoiceCatalog::default();
    for (who, uuid) in [("c:ada", "a8K"), ("si:scout", "Sc0"), ("Bb2", "Bb2")] {
        let view = app.get(&format!("/api/v1/voices/{who}"), &ada).await?;
        expect_status(&view, StatusCode::OK)?;
        let expected = catalog.default_for(uuid);
        assert_eq!(view.1["voice"]["key"], expected.key.as_str(), "{who}");
        assert_eq!(view.1["voice"]["name"], expected.name.as_str());
        assert_eq!(
            view.1["voice"]["description"],
            expected.description.as_str()
        );
        assert_eq!(view.1["chosen"], false);
        assert_eq!(view.1["chosen_by"], Value::Null);
        assert_eq!(view.1["updated_at"], Value::Null);
        assert_eq!(view.1["account"]["uuid"], uuid);
    }
    let own = app.get("/api/v1/voices/c:ada", &ada).await?;
    assert_eq!(own.1["account"]["id"], "c:ada");
    assert_eq!(
        own.1["can_change"], true,
        "an account chooses its own voice"
    );
    let silicon = app.get("/api/v1/voices/si:scout", &ada).await?;
    assert_eq!(
        silicon.1["can_change"], true,
        "a custodian chooses its Silicon's voice"
    );
    let other = app.get("/api/v1/voices/c:bob", &ada).await?;
    assert_eq!(other.1["can_change"], false);
    let anonymous = app
        .call(Method::GET, "/api/v1/voices/c:ada", None, None, &[])
        .await?;
    assert_eq!(anonymous.0, StatusCode::UNAUTHORIZED);
    let nobody = app.get("/api/v1/voices/c:nobody-here", &ada).await?;
    assert_eq!(nobody.0, StatusCode::NOT_FOUND, "{}", nobody.1);
    app.close().await
}

#[tokio::test]
async fn voices_are_chosen_by_the_account_or_its_custodian_and_reset_to_the_default()
-> Result<(), BoxError> {
    let deepgram = FakeDeepgram::start().await;
    let app = TestApp::start_with_speech(&deepgram, |_| {}).await?;
    let p = people(&app).await;
    let ada = app.accounts.token(&p.ada);
    let bob = app.accounts.token(&p.bob);
    let scout = app.accounts.token(&p.scout);

    // A Silicon chooses its own voice; the answer is a `voice` envelope.
    let request = Request::builder()
        .method(Method::PUT)
        .uri("/api/v1/voices/si:scout")
        .header("authorization", format!("Bearer {scout}"))
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"type": "voice.set", "data": {"voice": "brittney"}}).to_string(),
        ))?;
    let (status, _, body) = app.raw(request).await?;
    let envelope: Value = serde_json::from_slice(&body)?;
    assert_eq!(status, StatusCode::OK, "{envelope}");
    assert_eq!(envelope["type"], "voice");
    assert_eq!(envelope["data"]["voice"]["key"], "brittney");
    assert_eq!(envelope["data"]["chosen"], true);
    assert_eq!(envelope["data"]["chosen_by"]["id"], "si:scout");
    assert!(envelope["data"]["updated_at"].is_string());

    // Its custodian changes it; the chooser is recorded.
    let custodian = app
        .call(
            Method::PUT,
            "/api/v1/voices/si:scout",
            Some(&ada),
            Some(json!({"voice": "hope"})),
            &[],
        )
        .await?;
    expect_status(&custodian, StatusCode::OK)?;
    assert_eq!(custodian.1["voice"]["key"], "hope");
    assert_eq!(custodian.1["chosen_by"]["id"], "c:ada");
    assert_eq!(custodian.1["chosen_by"]["type"], "carbon");
    let seen_by_bob = app.get("/api/v1/voices/si:scout", &bob).await?;
    assert_eq!(seen_by_bob.1["voice"]["key"], "hope");
    assert_eq!(seen_by_bob.1["can_change"], false);

    // Anyone else is refused, with a hint naming who may.
    let stranger = app
        .call(
            Method::PUT,
            "/api/v1/voices/si:scout",
            Some(&bob),
            Some(json!({"voice": "mark"})),
            &[],
        )
        .await?;
    assert_eq!(stranger.0, StatusCode::FORBIDDEN);
    assert_eq!(stranger.1["error"]["code"], "voice_change_forbidden");
    assert!(
        stranger.1["error"]["hint"]
            .as_str()
            .is_some_and(|hint| hint.contains("si:scout") && hint.contains("c:ada")),
        "{}",
        stranger.1
    );
    let carbon = app
        .call(
            Method::PUT,
            "/api/v1/voices/c:ada",
            Some(&bob),
            Some(json!({"voice": "mark"})),
            &[],
        )
        .await?;
    assert_eq!(carbon.1["error"]["code"], "voice_change_forbidden");
    let reset_by_stranger = app
        .call(
            Method::DELETE,
            "/api/v1/voices/si:scout",
            Some(&bob),
            None,
            &[],
        )
        .await?;
    assert_eq!(reset_by_stranger.0, StatusCode::FORBIDDEN);

    // Unknown keys list the valid ones.
    let unknown = app
        .call(
            Method::PUT,
            "/api/v1/voices/c:ada",
            Some(&ada),
            Some(json!({"voice": "george"})),
            &[],
        )
        .await?;
    assert_eq!(unknown.0, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(unknown.1["error"]["code"], "voice_not_found");
    assert_eq!(
        unknown.1["error"]["details"]["voices"],
        json!(["mark", "brittney", "jamal", "jessa", "hope"])
    );
    let extra = app
        .call(
            Method::PUT,
            "/api/v1/voices/c:ada",
            Some(&ada),
            Some(json!({"voice": "mark", "speed": 2})),
            &[],
        )
        .await?;
    assert_eq!(
        extra.0,
        StatusCode::UNPROCESSABLE_ENTITY,
        "unknown fields are refused"
    );

    // An idempotency key replays; the same key with another body is a conflict.
    let key = vec![("idempotency-key", "voice-key-0001".to_owned())];
    for _ in 0..2 {
        let set = app
            .call(
                Method::PUT,
                "/api/v1/voices/c:ada",
                Some(&ada),
                Some(json!({"voice": "jamal"})),
                &key,
            )
            .await?;
        expect_status(&set, StatusCode::OK)?;
        assert_eq!(set.1["voice"]["key"], "jamal");
    }
    let conflict = app
        .call(
            Method::PUT,
            "/api/v1/voices/c:ada",
            Some(&ada),
            Some(json!({"voice": "jessa"})),
            &key,
        )
        .await?;
    assert_eq!(conflict.0, StatusCode::CONFLICT, "{}", conflict.1);

    // Resetting goes back to the default.
    let reset = app
        .call(
            Method::DELETE,
            "/api/v1/voices/si:scout",
            Some(&scout),
            None,
            &[],
        )
        .await?;
    expect_status(&reset, StatusCode::OK)?;
    assert_eq!(reset.1["chosen"], false);
    assert_eq!(reset.1["chosen_by"], Value::Null);
    assert_eq!(
        reset.1["voice"]["key"],
        VoiceCatalog::default().default_for("Sc0").key.as_str()
    );
    app.close().await
}

#[tokio::test]
async fn a_chosen_voice_that_left_the_catalog_falls_back_to_the_default() -> Result<(), BoxError> {
    let deepgram = FakeDeepgram::start().await;
    let app = TestApp::start_with_speech(&deepgram, |_| {}).await?;
    let p = people(&app).await;
    let ada = app.accounts.token(&p.ada);
    let chosen = app
        .call(
            Method::PUT,
            "/api/v1/voices/c:ada",
            Some(&ada),
            Some(json!({"voice": "mark"})),
            &[],
        )
        .await?;
    expect_status(&chosen, StatusCode::OK)?;
    sqlx::query("UPDATE account_voices SET voice_key = 'george' WHERE account_uuid = 'a8K'")
        .execute(app.state.store.pool())
        .await?;
    let view = app.get("/api/v1/voices/c:ada", &ada).await?;
    assert_eq!(
        view.1["voice"]["key"],
        VoiceCatalog::default().default_for("a8K").key.as_str()
    );
    assert_eq!(view.1["chosen"], false);
    app.close().await
}

#[tokio::test]
async fn sign_in_discovery_and_me_describe_speech() -> Result<(), BoxError> {
    let deepgram = FakeDeepgram::start().await;
    let app = TestApp::start_with_speech(&deepgram, |speech| speech.renders_per_minute = 7).await?;
    let p = people(&app).await;
    let accounts = app
        .call(Method::GET, "/api/v1/accounts", None, None, &[])
        .await?;
    expect_status(&accounts, StatusCode::OK)?;
    let speech = &accounts.1["speech"];
    assert_eq!(speech["enabled"], true);
    assert_eq!(speech["model"], "eleven_v4");
    assert_eq!(speech["stt_model"], "nova-3");
    assert_eq!(speech["voices_path"], "/api/v1/voices");
    assert_eq!(speech["limits"]["renders_per_minute"], 7);
    assert_eq!(speech["limits"]["script_characters"], 5000);
    let me = app.get("/api/v1/me", &app.accounts.token(&p.ada)).await?;
    expect_status(&me, StatusCode::OK)?;
    assert_eq!(me.1["speech"]["enabled"], true);
    assert_eq!(
        me.1["speech"]["voice"]["key"],
        VoiceCatalog::default().default_for("a8K").key.as_str()
    );
    assert_eq!(me.1["speech"]["voice"]["chosen"], false);
    let contracts = app
        .call(Method::GET, "/api/v1/contracts", None, None, &[])
        .await?;
    assert_eq!(
        contracts.1["features"]["speech"]["voices_path"],
        "/api/v1/voices"
    );
    app.close().await?;

    // Without a Deepgram key the same documents say speech is off.
    let off = TestApp::start().await?;
    let accounts = off
        .call(Method::GET, "/api/v1/accounts", None, None, &[])
        .await?;
    assert_eq!(accounts.1["speech"]["enabled"], false);
    let catalog = off
        .call(Method::GET, "/api/v1/voices", None, None, &[])
        .await?;
    assert_eq!(catalog.1["enabled"], false);
    assert_eq!(keys(&catalog.1).len(), 5, "the catalog is still listed");
    off.close().await
}
