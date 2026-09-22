//! Local WebSocket contract tests, not evidence of a deployed Ting integration.

use std::{error::Error, time::Duration};

use futures::{SinkExt as _, StreamExt as _};
use secrecy::SecretString;
use serde_json::{Value, json};
use silicon_dm::{
    config::TingSettings,
    infrastructure::ting::{TingFailure, TingSendAuthority, TingSocket, TingTestingHeaders},
};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::oneshot,
    time::timeout,
};
use tokio_tungstenite::{
    WebSocketStream, accept_hdr_async,
    tungstenite::{
        Message,
        handshake::server::{Request, Response},
    },
};
use tokio_util::sync::CancellationToken;

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;
type ServerSocket = WebSocketStream<TcpStream>;

// Whitespace, key ordering, Unicode and escape spelling are deliberately retained.
const BODY: &str = r#" {
  "key" : "dm-original-key", "org_id":"tos",
  "data" : {"schema_version":1,"message_id":"000"},
  "metadata":{"isi":"plan\u006eer", "note":"नमस्ते"},
  "for":"cos:tos", "type":"tos>dm.sync.changed"
} "#;

fn authority(proof: &str) -> TingSendAuthority {
    TingSendAuthority {
        proof_token: SecretString::from(proof.to_owned()),
        testing: None,
    }
}

async fn fixture(request_timeout: Duration) -> TestResult<(TcpListener, TingSettings)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let settings = TingSettings {
        base_url: format!("http://{}", listener.local_addr()?).parse()?,
        request_timeout,
    };
    Ok((listener, settings))
}

#[allow(
    clippy::result_large_err,
    reason = "Tungstenite requires this concrete handshake error response type"
)]
async fn accept(listener: &TcpListener) -> TestResult<ServerSocket> {
    let (stream, _) = timeout(Duration::from_secs(4), listener.accept()).await??;
    let mut socket = accept_hdr_async(stream, |request: &Request, response: Response| {
        assert_eq!(request.uri().path(), "/v1/ws");
        assert_eq!(request.uri().query(), Some("protocol=v1"));
        assert!(!request.headers().contains_key("authorization"));
        Ok(response)
    })
    .await?;
    socket
        .send(Message::Text(
            json!({"op":"ready","receiver_id":"receiver-local","protocol":"v1"})
                .to_string()
                .into(),
        ))
        .await?;
    Ok(socket)
}

async fn next_message(socket: &mut ServerSocket) -> TestResult<Message> {
    timeout(Duration::from_secs(4), socket.next())
        .await?
        .ok_or_else(|| "peer disconnected".into())
        .and_then(|message| message.map_err(Into::into))
}

async fn next_send(socket: &mut ServerSocket) -> TestResult<Value> {
    let Message::Text(text) = next_message(socket).await? else {
        return Err("expected send frame".into());
    };
    let frame: Value = serde_json::from_str(&text)?;
    assert_eq!(frame["op"], "send");
    assert!(
        frame["request_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty())
    );
    Ok(frame)
}

fn accepted(request: &Value) -> TestResult<Value> {
    let body: Value = serde_json::from_str(request["body"].as_str().ok_or("missing body")?)?;
    Ok(json!({
        "op":"accepted", "request_id":request["request_id"],
        "id":"ting-original-id", "created_at":"2026-09-22T10:00:00Z",
        "status":"accepted", "key":body["key"], "silent":false,
    }))
}

#[tokio::test]
async fn prewarms_and_preserves_exact_body_with_idle_and_inflight_heartbeats() -> TestResult {
    let (listener, settings) = fixture(Duration::from_secs(2)).await?;
    let (prewarmed, ready) = oneshot::channel();
    let server = tokio::spawn(async move {
        let mut socket = accept(&listener).await?;
        socket.send(Message::Ping(b"idle".to_vec().into())).await?;
        assert_eq!(
            next_message(&mut socket).await?,
            Message::Pong(b"idle".to_vec().into())
        );
        prewarmed.send(()).map_err(|()| "prewarm receiver closed")?;
        let first = next_send(&mut socket).await?;
        assert_eq!(
            first["body"], BODY,
            "proof-bound bytes must survive untouched"
        );
        assert_eq!(first["proof_token"], "proof-first");
        assert!(first.get("headers").is_none());
        socket
            .send(Message::Ping(b"awaiting-acceptance".to_vec().into()))
            .await?;
        assert_eq!(
            next_message(&mut socket).await?,
            Message::Pong(b"awaiting-acceptance".to_vec().into())
        );
        socket
            .send(Message::Text(accepted(&first)?.to_string().into()))
            .await?;

        let second = next_send(&mut socket).await?;
        assert_eq!(second["body"], BODY);
        assert_eq!(second["proof_token"], "proof-fresh");
        assert_ne!(first["request_id"], second["request_id"]);
        assert_eq!(
            second["headers"],
            json!({"IAM_TEST_APP_SECRET":"ting-test-secret", "X-Testing-Environment-Key":"iam-environment-key"})
        );
        socket
            .send(Message::Text(accepted(&second)?.to_string().into()))
            .await?;
        Ok::<_, Box<dyn Error + Send + Sync>>(())
    });
    let cancellation = CancellationToken::new();
    let publisher = TingSocket::start(&settings, cancellation.clone());
    timeout(Duration::from_secs(4), ready).await??;
    let first = publisher.send(BODY, authority("proof-first")).await?;
    let second = publisher
        .send(
            BODY,
            TingSendAuthority {
                proof_token: SecretString::from("proof-fresh"),
                testing: Some(TingTestingHeaders {
                    app_secret: SecretString::from("ting-test-secret"),
                    environment_key: SecretString::from("iam-environment-key"),
                }),
            },
        )
        .await?;
    assert_eq!(first, second);
    assert_eq!(first.id, "ting-original-id");
    assert!(!first.silent);
    cancellation.cancel();
    server.await??;
    Ok(())
}

#[tokio::test]
async fn rejected_and_mismatched_responses_never_confirm_acceptance() -> TestResult {
    for invalid in [
        "request_id",
        "key",
        "status",
        "id",
        "created_at",
        "silent",
        "op",
        "error",
        "unsafe_error",
    ] {
        let (listener, settings) = fixture(Duration::from_secs(2)).await?;
        let server = tokio::spawn(async move {
            let mut socket = accept(&listener).await?;
            let request = next_send(&mut socket).await?;
            let mut response = accepted(&request)?;
            match invalid {
                "request_id" => response["request_id"] = json!("another-request"),
                "key" => response["key"] = json!("another-event"),
                "status" => response["status"] = json!("queued"),
                "id" => response["id"] = json!("invalid\nidentifier"),
                "created_at" => response["created_at"] = json!("2026-09-22T10:00:00+00:00"),
                "silent" => {
                    response
                        .as_object_mut()
                        .ok_or("not object")?
                        .remove("silent");
                }
                "op" => response["op"] = json!("subscribed"),
                "error" | "unsafe_error" => {
                    response = json!({"op":"error","request_id":request["request_id"],"error":{
                        "code":if invalid=="error" { "recipient_not_registered" } else { "Secret: TEST_PROOF_SHOULD_NOT_APPEAR" },
                        "message":"upstream secret-bearing diagnostic", "hint":"do not echo me", "retryable":false,
                    }});
                }
                _ => return Err("unknown test case".into()),
            }
            socket
                .send(Message::Text(response.to_string().into()))
                .await?;
            Ok::<_, Box<dyn Error + Send + Sync>>(())
        });
        let cancellation = CancellationToken::new();
        let publisher = TingSocket::start(&settings, cancellation.clone());
        let result = publisher.send(BODY, authority("proof-test")).await;
        if matches!(invalid, "error" | "unsafe_error") {
            let Err(TingFailure::Rejected(code)) = result else {
                return Err("expected rejection".into());
            };
            assert_eq!(
                code,
                if invalid == "error" {
                    "recipient_not_registered"
                } else {
                    "ting_rejected"
                }
            );
        } else {
            assert!(
                matches!(result, Err(TingFailure::Protocol)),
                "invalid {invalid} was accepted: {result:?}"
            );
        }
        cancellation.cancel();
        server.await??;
    }
    Ok(())
}

#[tokio::test]
async fn timeout_is_uncertain_and_only_caller_retry_sends_a_fresh_proof() -> TestResult {
    assert_uncertain_retry(false).await
}

#[tokio::test]
async fn cancelled_inflight_caller_discards_socket_before_its_own_deadline() -> TestResult {
    let (listener, settings) = fixture(Duration::from_secs(30)).await?;
    let (received, sent) = oneshot::channel();
    let (reconnected, ready) = oneshot::channel();
    let server = tokio::spawn(async move {
        let mut socket = accept(&listener).await?;
        let first = next_send(&mut socket).await?;
        assert_eq!(first["proof_token"], "proof-cancelled");
        received.send(()).map_err(|()| "cancel receiver closed")?;
        let ended = timeout(Duration::from_secs(2), socket.next()).await?;
        assert!(
            !matches!(ended, Some(Ok(Message::Text(_)))),
            "caller cancellation must end the in-flight connection before its 30 second timeout"
        );
        drop(socket);
        let mut replacement = accept(&listener).await?;
        assert!(
            timeout(Duration::from_millis(150), replacement.next())
                .await
                .is_err(),
            "a dropped caller must not resend its consumed proof on reconnect"
        );
        reconnected.send(()).map_err(|()| "retry receiver closed")?;
        let second = next_send(&mut replacement).await?;
        assert_eq!(second["body"], first["body"]);
        assert_eq!(second["proof_token"], "proof-after-cancel");
        assert_ne!(second["request_id"], first["request_id"]);
        replacement
            .send(Message::Text(accepted(&second)?.to_string().into()))
            .await?;
        Ok::<_, Box<dyn Error + Send + Sync>>(())
    });
    let cancellation = CancellationToken::new();
    let _cancel_on_exit = cancellation.clone().drop_guard();
    let publisher = TingSocket::start(&settings, cancellation);
    let first_publisher = publisher.clone();
    let first = tokio::spawn(async move {
        first_publisher
            .send(BODY, authority("proof-cancelled"))
            .await
    });
    timeout(Duration::from_secs(4), sent).await??;
    first.abort();
    assert!(matches!(first.await,Err(error) if error.is_cancelled()));
    timeout(Duration::from_secs(4), ready).await??;
    assert_eq!(
        publisher
            .send(BODY, authority("proof-after-cancel"))
            .await?
            .id,
        "ting-original-id"
    );
    server.await??;
    Ok(())
}

#[tokio::test]
async fn a_valid_ready_greeting_is_required_before_transmitting_a_proof() -> TestResult {
    for greeting in [
        None,
        Some(json!({"op":"ready","protocol":"v2","receiver_id":"wrong-version"})),
        Some(json!({"op":"ready","protocol":"v1","receiver_id":""})),
    ] {
        let (listener, settings) = fixture(Duration::from_millis(150)).await?;
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await?;
            let mut socket = tokio_tungstenite::accept_async(stream).await?;
            if let Some(greeting) = greeting {
                socket
                    .send(Message::Text(greeting.to_string().into()))
                    .await?;
            }
            let frame = timeout(Duration::from_secs(2), socket.next()).await?;
            assert!(
                !matches!(frame, Some(Ok(Message::Text(_)))),
                "no proof may be sent before a valid ready greeting"
            );
            Ok::<_, Box<dyn Error + Send + Sync>>(())
        });
        let cancellation = CancellationToken::new();
        let publisher = TingSocket::start(&settings, cancellation.clone());
        assert!(
            publisher
                .send(BODY, authority("proof-must-stay-local"))
                .await
                .is_err()
        );
        cancellation.cancel();
        server.await??;
    }
    Ok(())
}

#[tokio::test]
async fn disconnect_is_uncertain_and_never_automatically_reuses_a_proof() -> TestResult {
    assert_uncertain_retry(true).await
}

async fn assert_uncertain_retry(disconnect: bool) -> TestResult {
    let (listener, settings) = fixture(Duration::from_millis(300)).await?;
    let (reconnected, ready) = oneshot::channel();
    let server = tokio::spawn(async move {
        let mut socket = accept(&listener).await?;
        let first = next_send(&mut socket).await?;
        assert_eq!(first["proof_token"], "proof-uncertain");
        if disconnect {
            socket.close(None).await?;
        }
        // With no response, the client reaches its deadline and closes the socket.
        // A transport reconnect must not replay the consumed/uncertain proof.
        let ended = timeout(Duration::from_secs(2), socket.next()).await?;
        assert!(
            !matches!(ended, Some(Ok(Message::Text(_)))),
            "no attempt may be retried on the old connection"
        );
        drop(socket);
        let mut replacement = accept(&listener).await?;
        assert!(
            timeout(Duration::from_millis(150), replacement.next())
                .await
                .is_err(),
            "reconnect must not automatically resend"
        );
        reconnected.send(()).map_err(|()| "retry receiver closed")?;
        let second = next_send(&mut replacement).await?;
        assert_eq!(second["body"], first["body"]);
        assert_eq!(second["proof_token"], "proof-after-uncertain");
        assert_ne!(second["request_id"], first["request_id"]);
        replacement
            .send(Message::Text(accepted(&second)?.to_string().into()))
            .await?;
        Ok::<_, Box<dyn Error + Send + Sync>>(())
    });
    let cancellation = CancellationToken::new();
    let publisher = TingSocket::start(&settings, cancellation.clone());
    let result = publisher.send(BODY, authority("proof-uncertain")).await;
    if disconnect {
        assert!(matches!(result, Err(TingFailure::Transport)));
    } else {
        assert!(matches!(result, Err(TingFailure::Timeout)));
    }
    timeout(Duration::from_secs(4), ready).await??;
    let accepted = publisher
        .send(BODY, authority("proof-after-uncertain"))
        .await?;
    assert_eq!(accepted.id, "ting-original-id");
    cancellation.cancel();
    server.await??;
    Ok(())
}

#[tokio::test]
async fn invalid_local_requests_and_incomplete_test_credentials_are_not_transmitted() -> TestResult
{
    let (listener, settings) = fixture(Duration::from_secs(2)).await?;
    let (prewarmed, ready) = oneshot::channel();
    let (inspected, finish) = oneshot::channel();
    let server = tokio::spawn(async move {
        let mut socket = accept(&listener).await?;
        prewarmed.send(()).map_err(|()| "prewarm receiver closed")?;
        finish.await?;
        assert!(
            timeout(Duration::from_millis(100), socket.next())
                .await
                .is_err(),
            "invalid request must remain local"
        );
        Ok::<_, Box<dyn Error + Send + Sync>>(())
    });
    let cancellation = CancellationToken::new();
    let publisher = TingSocket::start(&settings, cancellation.clone());
    ready.await?;
    for body in [
        "[]".to_owned(),
        "{".into(),
        json!({"key":"x".repeat(201)}).to_string(),
        json!({"key":"control\nkey"}).to_string(),
        "x".repeat(256 * 1024 + 1),
    ] {
        assert!(matches!(
            publisher.send(&body, authority("test-proof")).await,
            Err(TingFailure::InvalidRequest)
        ));
    }
    assert!(matches!(
        publisher.send(BODY, authority("")).await,
        Err(TingFailure::InvalidRequest)
    ));
    for (secret, key) in [("", "iam-key"), ("ting-secret", "")] {
        assert!(matches!(
            publisher
                .send(
                    BODY,
                    TingSendAuthority {
                        proof_token: SecretString::from("test-proof"),
                        testing: Some(TingTestingHeaders {
                            app_secret: SecretString::from(secret),
                            environment_key: SecretString::from(key),
                        }),
                    }
                )
                .await,
            Err(TingFailure::InvalidRequest)
        ));
    }
    assert!(matches!(
        publisher
            .send(BODY, authority(&"x".repeat(1024 * 1024)))
            .await,
        Err(TingFailure::InvalidRequest)
    ));
    inspected
        .send(())
        .map_err(|()| "inspection receiver closed")?;
    server.await??;
    cancellation.cancel();
    Ok(())
}
