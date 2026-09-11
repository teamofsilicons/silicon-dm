use anyhow::Result;
use serde_json::{Value, json};
use silicon_dm_client::runtime::store::{Profile, Store, session_key};
use std::sync::{Arc, Mutex};
use wiremock::{
    Mock, MockServer, Request, ResponseTemplate,
    matchers::{method, path, path_regex, query_param},
};

fn response(data: Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({"type":"response", "data":data}))
}

#[tokio::test]
async fn send_guard_precedes_queue_and_override_warns_only_after_delivery() -> Result<()> {
    for (sender, carbon, length, override_flag, data_file, state, expected_sends) in [
        ("silicon", true, 401, false, false, "completed", 0),
        ("silicon", true, 401, false, true, "completed", 0),
        ("silicon", true, 400, false, false, "completed", 1),
        ("carbon", true, 401, false, false, "completed", 1),
        ("silicon", false, 401, false, false, "completed", 1),
        ("silicon", true, 401, true, false, "completed", 1),
        ("silicon", true, 401, true, true, "failed", 1),
        ("silicon", true, 401, true, false, "pending", 1),
    ] {
        let server = MockServer::start().await;
        let directory = tempfile::tempdir()?;
        let id = "00000000-0000-0000-0000-000000000001";
        let store = Store::new(directory.path())?;
        let profile: Profile = serde_json::from_value(json!({
            "name":"default", "base_url":server.uri(), "device_id":"test",
            "expires_at":4102444800u64, "tokens":{
                "access_token":"test-access", "refresh_token":"test-refresh", "token_type":"Bearer",
                "expires_in":1800, "scope":"dm", "organization_id":"org",
                "actor":{"type":sender,"id":"sender:org"}
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
        // Put the target on page two, including both a Silicon and a Carbon
        // in guarded cases. Explicit --to must not evade group visibility.
        Mock::given(method("GET"))
            .and(path("/api/v1/conversations"))
            .respond_with(response(json!({"items":[], "next_cursor":"page-two"})))
            .mount(&server)
            .await;
        let mut participants = vec![json!({"type":"silicon","id":"peer:org"})];
        if carbon {
            participants.push(json!({"type":"carbon","id":"saket"}));
        }
        Mock::given(method("GET")).and(path("/api/v1/conversations"))
            .and(query_param("cursor", "page-two"))
            .respond_with(response(json!({"items":[{"id":id,"org_id":"org","participants":participants,"last_message":null,"created_at":"now","updated_at":"now"}]})))
            .with_priority(1).mount(&server).await;
        Mock::given(path("/status"))
            .respond_with(response(json!({"running":true})))
            .mount(&server)
            .await;
        let submitted = Arc::new(Mutex::new(None::<Value>));
        let capture = submitted.clone();
        Mock::given(method("POST"))
            .and(path("/requests"))
            .respond_with(move |request: &Request| {
                let body: Value = serde_json::from_slice(&request.body).unwrap();
                *capture.lock().unwrap() = Some(body.clone());
                response(
                    json!({"acknowledged":true,"request_id":body["data"]["request_id"],"request":body}),
                )
            })
            .expect(expected_sends)
            .mount(&server)
            .await;
        let capture = submitted.clone();
        Mock::given(method("GET")).and(path_regex("^/requests/"))
            .respond_with(move |_: &Request| {
                let body = capture.lock().unwrap().clone().unwrap();
                response(json!({"request_id":body["data"]["request_id"],"state":state,"request":body,"result":{},"error":if state == "failed" { json!({"code":"denied"}) } else { Value::Null }}))
            }).mount(&server).await;
        let text = "🙂".repeat(length);
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_dm"));
        command
            .env("SILICON_DM_HOME", directory.path())
            .env_remove("SILICON_DM_TEST")
            .args([
                "--wait-seconds",
                "0",
                "messages",
                "send",
                id,
                "--to",
                "peer:org",
            ]);
        if data_file {
            let file = directory.path().join("message.json");
            std::fs::write(&file, json!({"text":text}).to_string())?;
            command.arg("--data").arg(file);
        } else {
            command.arg("--text").arg(&text);
        }
        if override_flag {
            command.arg("--dangerously-send-long-message");
        }
        let output = command.output().await?;
        let stderr = String::from_utf8(output.stderr)?;
        assert_eq!(
            output.status.success(),
            expected_sends == 1 && state != "failed",
            "{stderr}"
        );
        if expected_sends == 0 {
            assert!(
                stderr.contains("message too long, not delivered."),
                "{stderr}"
            );
            assert!(submitted.lock().unwrap().is_none());
        } else {
            let body = submitted.lock().unwrap();
            assert_eq!(
                body.as_ref().unwrap()["data"]["request"]["message"]["message"],
                text
            );
        }
        assert_eq!(
            stderr.contains("Message sent but it was above the 400 characters safe read limits."),
            override_flag && state == "completed",
            "{stderr}"
        );
        server.verify().await;
    }
    Ok(())
}
