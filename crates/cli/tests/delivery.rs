use anyhow::Result;
use serde_json::{Value, json};
use silicon_dm_client::runtime::store::{Profile, Store, TestKey, session_key};
use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use uuid::Uuid;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_json, header, method, path},
};

fn command(directory: &std::path::Path) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_dm"));
    command
        .env("SILICON_DM_HOME", directory)
        .env("DM_TELEMETRY_ENABLED", "false")
        .env_remove("SILICON_DM_TEST")
        .env_remove("DM_TEST_APP_SECRET")
        .env_remove("DM_TING_TEST_APP_SECRET")
        .env_remove("DM_TING_TEST_ENVIRONMENT_KEY")
        .env_remove("DM_TING_API_URL");
    command
}

fn state(directory: &std::path::Path, base: &str, test: Option<Uuid>) -> Result<Store> {
    let store = Store::new(directory)?;
    let profile: Profile = serde_json::from_value(json!({
        "name":"default","base_url":base,"device_id":"fixture-device","expires_at":4102444800u64,
        "testing_environment_id":test,"tokens":{"access_token":"dm-fixture-access","refresh_token":"dm-fixture-refresh",
            "token_type":"Bearer","expires_in":3600,"scope":"dm","organization_id":"tos","actor":{"type":"silicon","id":"bob"}}
    }))?;
    store.update(|config| {
        config.telemetry_enabled = false;
        config
            .profiles
            .insert(session_key("default", test), profile);
        if let Some(id) = test {
            config.testing_keys.insert(
                id,
                TestKey {
                    base_url: base.into(),
                    key: "a".repeat(32),
                },
            );
        }
        Ok(())
    })?;
    Ok(store)
}

fn dm_response(value: Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({"type":"response","data":value}))
}

async fn discovery(server: &MockServer, test: Option<Uuid>, generation: Option<i64>) {
    Mock::given(method("GET"))
        .and(path("/api/v1/iam"))
        .respond_with(dm_response(
            json!({"app_id":"tos>dm","iam_base_url":server.uri(),"api_base_url":server.uri(),
            "testing_environment_id":test,"testing_generation":generation}),
        ))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/auth/me"))
        .respond_with(dm_response(
            json!({"actor":{"type":"silicon","id":"bob"},"organization_id":"tos",
            "principal_id":"fixture-principal","capabilities":[]}),
        ))
        .mount(server)
        .await;
}

#[tokio::test]
async fn retired_and_ambiguous_secret_inputs_fail_before_read_or_http() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let server = MockServer::start().await;
    let base = server.uri();
    for (arguments, reason) in [
        (
            vec![
                "login",
                "--token-file",
                "/nonexistent/secret",
                "--webhook",
                "http://localhost/events",
                "--base-url",
                base.as_str(),
            ],
            "no token was read",
        ),
        (
            vec![
                "delivery",
                "login",
                "--token-file",
                "-",
                "--ting-app-secret-file",
                "/nonexistent/secret",
            ],
            "requires both",
        ),
        (
            vec!["--app-secret-file", "-", "login", "--token-file", "-"],
            "only one",
        ),
        (
            vec![
                "delivery",
                "login",
                "--token-file",
                "/nonexistent/secret",
                "--idempotency-key",
                "short-key",
            ],
            "16-200 visible ASCII bytes",
        ),
    ] {
        let output = command(directory.path()).args(arguments).output().await?;
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(reason),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert!(server.received_requests().await.unwrap().is_empty());
    Ok(())
}

#[tokio::test]
async fn ting_login_reads_stdin_and_never_substitutes_dm_tokens_or_registers_consent() -> Result<()>
{
    let dm = MockServer::start().await;
    let ting = MockServer::start().await;
    let directory = tempfile::tempdir()?;
    let store = state(directory.path(), &dm.uri(), None)?;
    discovery(&dm, None, None).await;
    Mock::given(method("GET"))
        .and(path("/v1/iam"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"app_id":"tos>ting"})))
        .mount(&ting)
        .await;
    Mock::given(method("POST")).and(path("/v1/session"))
        .and(header("Idempotency-Key", "fixture-ting-login-key"))
        .and(body_json(json!({"slt":"ting-fixture-slt"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"authenticated":true,"id":"bob","kind":"silicon","session_token":"ting-fixture-session"})))
        .expect(1).mount(&ting).await;
    Mock::given(method("GET"))
        .and(path("/v1/me"))
        .and(header("Authorization", "Bearer ting-fixture-session"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"authenticated":true,"id":"bob","kind":"silicon","environment":{"kind":"production"}})),
        )
        .mount(&ting)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/orgs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items":[{"id":"tos"}]})))
        .mount(&ting)
        .await;
    let mut child = command(directory.path())
        .args([
            "delivery",
            "login",
            "--token-file",
            "-",
            "--base-url",
            &ting.uri(),
            "--idempotency-key",
            "fixture-ting-login-key",
            "--json",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"  ting-fixture-slt\n")
        .await?;
    let output = child.wait_with_output().await?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(result["delivery_provider"], "ting");
    assert_eq!(result["idempotency_key"], "fixture-ting-login-key");
    for secret in [
        "ting-fixture-slt",
        "ting-fixture-session",
        "dm-fixture-access",
        "dm-fixture-refresh",
    ] {
        assert!(!String::from_utf8_lossy(&output.stdout).contains(secret));
        assert!(!String::from_utf8_lossy(&output.stderr).contains(secret));
    }
    assert!(
        dm.received_requests()
            .await
            .unwrap()
            .iter()
            .all(|r| r.method == "GET")
    );
    let config = store.load()?;
    assert!(config.profiles["default:production"].webhook_url.is_none());
    assert!(!directory.path().join("relay.sqlite3").exists());
    Ok(())
}

#[tokio::test]
async fn explicit_registration_binds_the_selected_test_generation_and_retry_key() -> Result<()> {
    let dm = MockServer::start().await;
    let directory = tempfile::tempdir()?;
    let test = Uuid::new_v4();
    state(directory.path(), &dm.uri(), Some(test))?;
    discovery(&dm, Some(test), Some(7)).await;
    Mock::given(method("POST"))
        .and(path("/api/v1/delivery/registration"))
        .and(header("Authorization", "Bearer dm-fixture-access"))
        .and(header("X-Testing-Environment-Generation", "7"))
        .and(header("Idempotency-Key", "fixture-enrollment-key"))
        .and(body_json(json!({"type":"delivery_registration","data":{}})))
        .respond_with(dm_response(
            json!({"id":"subscription-fixture","app_id":"tos>dm","for":"bob","active":true}),
        ))
        .expect(2)
        .mount(&dm)
        .await;
    for _ in 0..2 {
        let output = command(directory.path())
            .args([
                "--test",
                &test.to_string(),
                "delivery",
                "register",
                "--idempotency-key",
                "fixture-enrollment-key",
                "--json",
            ])
            .output()
            .await?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let result: Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(result["subscription"]["recipient"], Value::Null);
        assert_eq!(result["subscription"]["for"], "bob");
        assert_eq!(result["idempotency_key"], "fixture-enrollment-key");
    }
    assert!(!directory.path().join("relay.sqlite3").exists());
    Ok(())
}
