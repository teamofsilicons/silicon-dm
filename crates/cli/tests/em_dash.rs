use anyhow::Result;
use serde_json::{Value, json};
use silicon_dm_client::runtime::store::{Profile, Store, session_key};
use std::sync::{Arc, Mutex};
use tokio::io::AsyncWriteExt;
use wiremock::{
    Mock, MockServer, Request, ResponseTemplate,
    matchers::{method, path, path_regex},
};

fn response(data: Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({"type":"response", "data":data}))
}

#[tokio::test]
async fn cli_normalizes_silicon_text_before_queueing_and_checks_final_length() -> Result<()> {
    let boundary = format!("{}—b", "a".repeat(138));
    for (sender, input, source, preserve, state, expected) in [
        (
            "silicon",
            "hello—world",
            "text",
            false,
            "completed",
            Some("hello - world"),
        ),
        (
            "silicon",
            "hello — world",
            "file",
            false,
            "completed",
            Some("hello - world"),
        ),
        (
            "silicon",
            "hello— world",
            "stdin",
            false,
            "completed",
            Some("hello - world"),
        ),
        (
            "silicon",
            "hello—world",
            "text",
            true,
            "completed",
            Some("hello—world"),
        ),
        (
            "silicon",
            "hello—world",
            "file",
            true,
            "completed",
            Some("hello—world"),
        ),
        (
            "silicon",
            "hello—world",
            "stdin",
            true,
            "completed",
            Some("hello—world"),
        ),
        (
            "carbon",
            "hello—world",
            "text",
            false,
            "completed",
            Some("hello—world"),
        ),
        (
            "silicon",
            "hello - world",
            "text",
            false,
            "completed",
            Some("hello - world"),
        ),
        (
            "silicon",
            "hello—world",
            "text",
            false,
            "failed",
            Some("hello - world"),
        ),
        (
            "silicon",
            boundary.as_str(),
            "text",
            false,
            "completed",
            None,
        ),
        (
            "silicon",
            boundary.as_str(),
            "text",
            true,
            "completed",
            Some(boundary.as_str()),
        ),
    ] {
        let server = MockServer::start().await;
        let directory = tempfile::tempdir()?;
        let store = Store::new(directory.path())?;
        let conversation = "sender:org::reader";
        let profile: Profile = serde_json::from_value(json!({
            "name":"default", "base_url":server.uri(), "device_id":"test",
            "expires_at":4102444800u64, "tokens":{
                "access_token":"test-access", "refresh_token":"test-refresh", "token_type":"Bearer",
                "expires_in":1800, "scope":"dm", "organization_id":"org",
                "actor":{"type":sender,"id":"si:sender"}
            }
        }))?;
        store.update(|config| {
            config.auto_update = false;
            config.relay_port = server.address().port();
            config
                .profiles
                .insert(session_key("default", None), profile);
            Ok(())
        })?;
        Mock::given(method("GET")).and(path("/api/v1/conversations"))
            .respond_with(response(json!({"items":[{"id":conversation,"org_id":"org","participants":[{"type":"carbon","id":"reader"}],"last_message":null,"created_at":"now","updated_at":"now"}]})))
            .mount(&server).await;
        Mock::given(path("/status"))
            .respond_with(response(json!({"running":true,"incoming_delivery":{"code":"delivery_moved_to_ting","provider":"ting","forwarding":false},"host":{"shared":true}})))
            .mount(&server)
            .await;
        let submitted = Arc::new(Mutex::new(None::<Value>));
        let capture = submitted.clone();
        Mock::given(method("POST")).and(path("/requests"))
            .respond_with(move |request: &Request| {
                let body: Value = serde_json::from_slice(&request.body).unwrap();
                *capture.lock().unwrap() = Some(body.clone());
                response(json!({"acknowledged":true,"request_id":body["data"]["request_id"],"request":body}))
            }).expect(u64::from(expected.is_some())).mount(&server).await;
        let capture = submitted.clone();
        Mock::given(method("GET")).and(path_regex("^/requests/"))
            .respond_with(move |_: &Request| {
                let body = capture.lock().unwrap().clone().unwrap();
                response(json!({"request_id":body["data"]["request_id"],"state":state,"request":body,"result":{},"error":if state == "failed" { json!({"code":"denied"}) } else { Value::Null }}))
            }).mount(&server).await;
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_dm"));
        command
            .env("SILICON_DM_HOME", directory.path())
            .env(
                "SILICON_DM_RELAY_HOME",
                directory.path().join("shared-relay"),
            )
            .env_remove("SILICON_DM_TEST")
            .args([
                "--json",
                "--wait-seconds",
                "0",
                "messages",
                "send",
                conversation,
            ]);
        let content = json!({"message":input,"attachments":["https://example.com/file.pdf"],"voice_transcript":"keep—transcript"}).to_string();
        match source {
            "file" => {
                let file = directory.path().join("message.json");
                std::fs::write(&file, &content)?;
                command.arg("--data").arg(file);
            }
            "stdin" => {
                command
                    .args(["--data", "-"])
                    .stdin(std::process::Stdio::piped());
            }
            _ => {
                command.arg("--text").arg(input);
            }
        }
        if preserve {
            command.arg("--dangerously-use-em-dash");
        }
        command
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = command.spawn()?;
        if source == "stdin" {
            let mut stdin = child.stdin.take().unwrap();
            stdin.write_all(content.as_bytes()).await?;
            stdin.shutdown().await?;
        }
        let output = child.wait_with_output().await?;
        let stderr = String::from_utf8(output.stderr)?;
        assert_eq!(
            output.status.success(),
            expected.is_some() && state != "failed",
            "{stderr}"
        );
        assert_eq!(
            stderr.contains("Replaced em dashes"),
            sender == "silicon" && input.contains('—') && !preserve,
            "{stderr}"
        );
        if stderr.contains("Replaced em dashes") {
            assert!(stderr.contains("--dangerously-use-em-dash"));
        }
        if let Some(expected) = expected {
            let body = submitted.lock().unwrap().clone().unwrap();
            assert_eq!(body["data"]["request"]["message"]["message"], expected);
            if source != "text" {
                assert_eq!(
                    body["data"]["request"]["message"]["voice_transcript"],
                    "keep—transcript"
                );
                assert_eq!(
                    body["data"]["request"]["message"]["attachments"],
                    json!(["https://example.com/file.pdf"])
                );
            }
            let result: Value = serde_json::from_slice(&output.stdout)?;
            if state == "failed" {
                assert!(result.get("acknowledgement").is_none());
            }
        } else {
            assert!(submitted.lock().unwrap().is_none());
            assert!(
                stderr.contains("message too long, not delivered."),
                "{stderr}"
            );
        }
        server.verify().await;
    }
    Ok(())
}
