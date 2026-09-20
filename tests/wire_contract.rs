//! Exercise the published bytes across independent backend and SDK implementations.
use axum::{
    Json, Router,
    http::{HeaderMap, StatusCode},
    routing::post,
};
use serde_json::{Value, json};
use silicon_dm::{api::extract::ApiJson, domain::MessageCreate, realtime as protocol};
use silicon_dm_client::{Client, ClientFrame};
use uuid::Uuid;

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

fn message(content: &MessageCreate) -> Result<silicon_dm::domain::Message> {
    let mut value = serde_json::to_value(content)?;
    let fields = value.as_object_mut().ok_or("content must be an object")?;
    fields.extend(
        json!({
            "id":Uuid::nil(), "conversation_id":Uuid::nil(),
            "sender":{"type":"silicon","id":"cos:tos"},
            "sequence":1,"status":"sent","version":1,"deleted_at":null,
            "created_at":"2026-09-11T00:00:00Z"
        })
        .as_object()
        .ok_or("fixture must be an object")?
        .clone(),
    );
    Ok(serde_json::from_value(value)?)
}

#[tokio::test]
async fn sdk_http_send_and_response_preserve_content_headers_and_metadata() -> Result {
    let app = Router::new()
        .route(
            "/api/v1/conversations/{id}/messages",
            post(
                |headers: HeaderMap, ApiJson(content): ApiJson<MessageCreate>| async move {
                    assert_eq!(headers["authorization"], "Bearer access");
                    assert_eq!(headers["x-org-id"], "tos");
                    assert_eq!(headers["idempotency-key"], "stable-message-key");
                    assert_eq!(content.text.as_deref(), Some("hello"));
                    let body = message(&content).unwrap_or_else(|error| panic!("fixture: {error}"));
                    (StatusCode::ACCEPTED, Json(body))
                },
            ),
        )
        .layer(axum::middleware::from_fn(silicon_dm_protocol::responses));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let client = Client::new(&base)?.with_auth("access", "tos");
    let content: silicon_dm_client::MessageCreate = serde_json::from_value(json!({
        "message":"hello", "metadata":{"type":"keep","data":{"message":"nested"},"text":"untouched"},
        "recipient_id":"deliberate@cos:tos",
        "attachments":[{"permanent_url":"https://files.example/note.pdf"}]
    }))?;
    let result = client
        .send_message(Uuid::nil(), &content, "stable-message-key")
        .await?;
    assert_eq!(result.content.text, content.text);
    assert!(result.content.metadata.is_empty());
    assert_eq!(result.content.recipient_id, content.recipient_id);
    assert_eq!(result.content.attachments.len(), 1);
    server.abort();
    Ok(())
}

#[tokio::test]
async fn http_rejects_flat_wrong_type_and_extra_top_level_fields() -> Result {
    let app = Router::new()
        .route(
            "/api/v1/conversations/{id}/messages",
            post(|ApiJson(content): ApiJson<MessageCreate>| async move { Json(content) }),
        )
        .layer(axum::extract::DefaultBodyLimit::max(1024))
        .layer(axum::middleware::from_fn(silicon_dm_protocol::responses));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let url = format!(
        "http://{}/api/v1/conversations/{}/messages",
        listener.local_addr()?,
        Uuid::nil()
    );
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let http = reqwest::Client::new();
    for invalid in [
        json!({"message":"old flat body"}),
        json!({"type":"receipt","data":{"message":"wrong operation"}}),
        json!({"type":"new_message","data":{"message":"hello"},"metadata":{}}),
        json!({"type":"new_message"}),
    ] {
        let response = http.post(&url).json(&invalid).send().await?;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body: Value = response.json().await?;
        assert_eq!(body.as_object().map(serde_json::Map::len), Some(2));
        assert_eq!(body["type"], "error");
        assert_eq!(body["data"]["error"]["code"], "validation_error");
    }
    let response = http
        .post(&url)
        .json(&json!({"type":"new_message","data":{"message":"x".repeat(2048)}}))
        .send()
        .await?;
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(response.json::<Value>().await?["type"], "error");
    server.abort();
    Ok(())
}

#[test]
fn websocket_command_and_delivery_round_trip_between_backend_and_sdk() -> Result {
    let content: silicon_dm_client::MessageCreate = serde_json::from_value(json!({
        "message":"hello", "metadata":{"data":{"type":"user metadata"}},
        "voice":{"permanent_url":"https://files.example/voice.ogg","duration_milliseconds":1000},
        "voice_transcript":"spoken hello"
    }))?;
    let encoded = serde_json::to_value(ClientFrame::SendMessage {
        actor_id: "cos:tos".into(),
        org_id: "tos".into(),
        conversation_id: Uuid::nil().to_string(),
        idempotency_key: "retry-message-key".into(),
        message: Box::new(content),
    })?;
    assert_eq!(encoded.as_object().map(serde_json::Map::len), Some(2));
    assert_eq!(encoded["type"], "message.create");
    assert_eq!(encoded["data"]["message"], "hello");
    let protocol::ClientFrame::SendMessage {
        message: content, ..
    } = serde_json::from_value(encoded.clone())?
    else {
        return Err("unexpected command".into());
    };
    let frame = protocol::ServerFrame::Message {
        delivery_id: Uuid::nil(),
        actor_id: "cos:tos".parse()?,
        delivery_sequence: 1,
        message: Box::new(message(&serde_json::from_value(*content)?)?),
    };
    let delivery = serde_json::to_value(frame)?;
    assert_eq!(delivery.as_object().map(serde_json::Map::len), Some(2));
    assert_eq!(delivery["data"]["message"], "hello");
    assert_eq!(delivery["data"]["metadata"], json!({}));
    assert!(encoded["data"].get("metadata").is_none());
    let received: silicon_dm_client::ServerFrame = serde_json::from_value(delivery)?;
    let silicon_dm_client::ServerFrame::Message {
        message,
        delivery_sequence,
        ..
    } = received
    else {
        return Err("unexpected delivery".into());
    };
    assert_eq!(message.content.text.as_deref(), Some("hello"));
    assert_eq!(delivery_sequence, 1);
    assert!(
        serde_json::from_value::<protocol::ClientFrame>(json!({"type":"pong","ping_id":"p"}))
            .is_err()
    );
    assert!(
        serde_json::from_value::<protocol::ClientFrame>(
            json!({"type":"pong","data":{"ping_id":"p"},"extra":true})
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn creation_delivery_distinguishes_sender_from_recipient_and_command_reply() -> Result {
    let content: MessageCreate = serde_json::from_value(json!({"message":"hello"}))?;
    let message: silicon_dm_client::Message =
        serde_json::from_value(serde_json::to_value(message(&content)?)?)?;
    for (recipient, kind) in [
        ("cos:tos", "message.create.successful"),
        ("alice", "message.create"),
    ] {
        let frame = silicon_dm_client::ServerFrame::Message {
            delivery_id: Uuid::nil(),
            actor_id: recipient.into(),
            delivery_sequence: 1,
            message: Box::new(message.clone()),
        };
        let encoded = serde_json::to_value(frame)?;
        assert_eq!(encoded["type"], kind);
        let decoded: silicon_dm_client::ServerFrame = serde_json::from_value(encoded)?;
        assert_eq!(
            decoded
                .delivery_position()
                .map(|(_, actor, sequence)| (actor, sequence)),
            Some((recipient, 1))
        );
    }
    for kind in ["message.create.successful", "message.create.success"] {
        let mut data = serde_json::to_value(&message)?;
        data["idempotency_key"] = json!("send-retry");
        let decoded: silicon_dm_client::ServerFrame =
            serde_json::from_value(json!({"type":kind,"data":data}))?;
        assert!(matches!(
            decoded,
            silicon_dm_client::ServerFrame::MessageAccepted { .. }
        ));
        assert!(decoded.delivery_position().is_none());
    }
    Ok(())
}

#[tokio::test]
async fn rest_creation_uses_a_command_and_a_successful_response() -> Result {
    let app = Router::new()
        .route(
            "/api/v1/messages",
            post(|ApiJson(value): ApiJson<Value>| async move { Json(value) }),
        )
        .layer(axum::middleware::from_fn(silicon_dm_protocol::responses));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}/api/v1/messages", listener.local_addr()?);
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    for kind in ["message.create", "message.created"] {
        let response: Value = reqwest::Client::new()
            .post(&url)
            .json(&json!({"type":kind,"data":{"message":"hello"}}))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        assert_eq!(response["type"], "message.create.successful");
        assert_eq!(response["data"]["message"], "hello");
    }
    server.abort();
    Ok(())
}

#[tokio::test]
async fn legacy_draft_conflict_keeps_remote_content_and_explains_recovery() -> Result {
    let remote = json!({"conversation_id":Uuid::nil(),"member_id":"alice","version":3,"message_content":"newer text","metadata":{}});
    let body = remote.clone();
    let app = Router::new().route(
        "/api/v1/conversations/{id}/draft",
        axum::routing::put(move || {
            let body = body.clone();
            async move {
                (
                    StatusCode::CONFLICT,
                    Json(json!({"type":"error","data":body})),
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let error = Client::new(base)?
        .put_draft(Uuid::nil(), &silicon_dm_client::DraftInput::default(), 1)
        .await
        .err()
        .ok_or("expected operation to fail")?;
    let silicon_dm_client::Error::Api {
        status,
        code,
        message,
        body,
        ..
    } = error
    else {
        return Err("expected draft conflict".into());
    };
    assert_eq!(status, 409);
    assert_eq!(code, "draft_conflict");
    assert!(message.contains("not applied"));
    assert_eq!(*body, remote);
    server.abort();
    Ok(())
}

#[tokio::test]
async fn non_enveloped_http_failures_keep_the_upstream_body() -> Result {
    let app = Router::new().route(
        "/api/v1/conversations/{id}/draft",
        axum::routing::get(|| async {
            (StatusCode::BAD_GATEWAY, "upstream temporarily unavailable")
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let error = Client::new(base)?
        .draft("alice+work@example.com::bob")
        .await
        .err()
        .ok_or("expected operation to fail")?;
    assert!(error.retryable());
    let silicon_dm_client::Error::Api {
        status,
        message,
        body,
        ..
    } = error
    else {
        return Err("expected HTTP failure".into());
    };
    assert_eq!(status, 502);
    assert_eq!(message, "Bad Gateway");
    assert_eq!(body["raw"], "upstream temporarily unavailable");
    server.abort();
    Ok(())
}
