#![cfg(feature = "runtime")]
//! Local HTTP fixtures verify client boundaries; these are not live Ting proof.

use std::sync::{Arc, Mutex};

use axum::{
    Json, Router,
    body::to_bytes,
    extract::{Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::any,
};
use serde_json::{Value, json};
use silicon_dm_client::{
    Activity, Actor, ActorType, Client, Error, SyncRequest,
    ting::{HydratedTingItem, TingItem, TingReceiverContext, TingRejection},
};
use uuid::Uuid;

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Clone)]
struct Seen {
    method: String,
    path: String,
    query: Option<String>,
    headers: axum::http::HeaderMap,
    body: Value,
}
type Requests = Arc<Mutex<Vec<Seen>>>;

struct Fixture {
    client: Client,
    requests: Requests,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Fixture {
    async fn new() -> Result<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let client = Client::new(format!("http://{}", listener.local_addr()?))?
            .with_auth("fixture-oat", "tos")
            .with_telemetry(false);
        let requests = Arc::new(Mutex::new(Vec::new()));
        let router = Router::new()
            .route("/{*path}", any(handler))
            .with_state(requests.clone());
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        Ok(Self {
            client,
            requests,
            task,
        })
    }
}

async fn handler(State(requests): State<Requests>, request: Request) -> Response {
    let method = request.method().to_string();
    let path = request.uri().path().to_owned();
    let query = request.uri().query().map(str::to_owned);
    let headers = request.headers().clone();
    let body = to_bytes(request.into_body(), 1024 * 1024).await.unwrap();
    requests.lock().unwrap().push(Seen {
        method: method.clone(),
        path: path.clone(),
        query: query.clone(),
        headers,
        body: serde_json::from_slice(&body).unwrap_or(Value::Null),
    });
    let value = match path.as_str() {
        "/api/v1/auth/me" => {
            json!({"member":{"type":"carbon","id":"alice"},"organization_id":"tos","principal_id":"alice","session_id":null,"org_role":null,"capabilities":[]})
        }
        "/api/v1/iam" => {
            json!({"app_id":"tos>dm","iam_base_url":"https://iam.invalid","api_base_url":"https://dm.invalid","testing_environment_id":null,"testing_generation":null})
        }
        "/api/v1/delivery/registration" => {
            json!({"id":"sub_fixture","app_id":"tos>dm","for":"alice","active":true})
        }
        "/api/v1/sync" => {
            if query
                .as_deref()
                .is_some_and(|query| query.contains("cursor=expired"))
            {
                return failure(StatusCode::CONFLICT, "sync_reset_required");
            }
            json!({"events":[],"cursor":"durable-resume-cursor","has_more":false,"upper_sequence":7,"testing_environment_id":null,"testing_generation":null})
        }
        path if path.starts_with("/api/v1/presence/devices/") => {
            if method == "DELETE" {
                return StatusCode::NO_CONTENT.into_response();
            }
            json!({"presence":{"member_id":"alice","availability":"online","activity":"typing","last_seen_at":null},"lease_expires_at":"2026-09-22T10:02:00Z","activity_expires_at":"2026-09-22T10:00:10Z"})
        }
        "/api/v1/conversations/alice::bob/messages/000" => {
            json!({"message-id":"000","conversation_id":"alice::bob","sender":{"type":"carbon","id":"bob"},"recipient_id":"alice","message":null,"attachments":[],"created_at":"2026-09-22T10:00:00Z","deleted_at":"2026-09-22T10:01:00Z"})
        }
        "/api/v1/conversations/alice::bob/messages/001" => {
            return failure(StatusCode::NOT_FOUND, "not_found");
        }
        "/api/v1/conversations/alice::bob/messages/002" => {
            return failure(StatusCode::SERVICE_UNAVAILABLE, "dependency_unavailable");
        }
        _ => return failure(StatusCode::NOT_FOUND, "not_found"),
    };
    Json(json!({"type":"fixture","data":value})).into_response()
}

fn failure(status: StatusCode, code: &str) -> Response {
    (
        status,
        Json(json!({"type":"error","data":{"error":{"code":code,"message":"fixture error"}}})),
    )
        .into_response()
}

#[tokio::test]
async fn typed_delivery_sync_and_presence_use_http_contracts() -> Result {
    let fixture = Fixture::new().await?;
    let registration = fixture
        .client
        .register_delivery("explicit-registration")
        .await?;
    assert!(registration.active);
    assert_eq!(registration.recipient, "alice");
    let page = fixture
        .client
        .sync(&SyncRequest {
            limit: Some(1),
            ..SyncRequest::default()
        })
        .await?;
    assert_eq!(page.cursor, "durable-resume-cursor");
    assert!(!page.has_more);
    assert_eq!(fixture.client.sync_reset().await?.upper_sequence, 7);
    let expired = fixture
        .client
        .sync(&SyncRequest {
            cursor: Some("expired".into()),
            ..SyncRequest::default()
        })
        .await
        .unwrap_err();
    assert!(expired.sync_reset_required());
    assert!(!expired.retryable());
    let presence = fixture
        .client
        .renew_presence("desk /+", Some(Activity::Typing))
        .await?;
    assert_eq!(presence.presence.actor_id, "alice");
    fixture.client.close_presence("desk /+").await?;
    let selected = fixture
        .client
        .clone()
        .with_test_key("a".repeat(32))?
        .with_testing_generation(4)?;
    selected.sync(&SyncRequest::default()).await?;
    let seen = fixture.requests.lock().unwrap();
    assert_eq!(seen[0].headers["idempotency-key"], "explicit-registration");
    assert_eq!(
        seen[0].body,
        json!({"type":"delivery_registration","data":{}})
    );
    assert_eq!(seen[1].query.as_deref(), Some("limit=1"));
    assert_eq!(seen[2].query.as_deref(), Some("reset=true"));
    assert_eq!(seen[4].method, "PUT");
    assert_eq!(seen[4].path, "/api/v1/presence/devices/desk%20%2F%2B");
    assert_eq!(
        seen[4].body,
        json!({"type":"renew_presence","data":{"activity":"typing"}})
    );
    assert_eq!(seen[5].method, "DELETE");
    assert_eq!(seen[6].headers["x-testing-environment-generation"], "4");
    assert!(
        seen.iter()
            .all(|request| request.headers["x-org-id"] == "tos"
                && request.headers["authorization"] == "Bearer fixture-oat")
    );
    Ok(())
}

fn reference(message: &str) -> Value {
    let id = Uuid::new_v4();
    json!({"id":format!("msg_{message}"),"type":"tos>dm.sync.changed","key":id,
        "metadata":{"testing_environment_id":null,"testing_generation":null},
        "data":{"schema_version":1,"event":"message.created","org_id":"tos","conversation_id":"alice::bob","message_id":message,"delivery_id":id,"delivery_sequence":1}})
}

fn receiver() -> TingReceiverContext {
    TingReceiverContext {
        ting_organization_id: None,
        app_id: "tos>dm".into(),
        organization_id: "tos".into(),
        actor: Actor {
            actor_type: ActorType::Carbon,
            id: "alice".into(),
        },
        testing_environment_id: None,
        testing_generation: None,
    }
}

#[tokio::test]
async fn hydration_rechecks_identity_and_fetches_only_valid_dm_references_without_acks() -> Result {
    let fixture = Fixture::new().await?;
    let mut other = reference("000");
    other["type"] = json!("tos>remind.changed");
    let mut wrong = reference("000");
    wrong["for"] = json!("bob");
    let batch = serde_json::to_vec(
        &json!({"tings":[reference("000"),reference("001"),reference("002"),other,wrong]}),
    )?;
    let items = fixture
        .client
        .hydrate_ting_batch(&batch, &receiver())
        .await?;
    assert_eq!(items.len(), 5);
    assert!(
        matches!(&items[0],HydratedTingItem::Message{message,..} if message.deleted_at.is_some())
    );
    assert!(matches!(&items[1], HydratedTingItem::Inaccessible { .. }));
    assert!(matches!(&items[2],HydratedTingItem::Failed{error,..} if error.retryable()));
    assert!(matches!(
        &items[3],
        HydratedTingItem::Skipped(TingItem::Unrelated { .. })
    ));
    assert!(matches!(
        &items[4],
        HydratedTingItem::Skipped(TingItem::Rejected {
            reason: TingRejection::WrongRecipient,
            ..
        })
    ));
    let seen = fixture.requests.lock().unwrap();
    assert_eq!(
        seen.len(),
        5,
        "only fresh me, iam and three authorized message GETs"
    );
    assert!(seen.iter().all(|request| request.method == "GET"));
    Ok(())
}

#[tokio::test]
async fn canonical_inbox_org_hydrates_only_the_mapped_dm_organization() -> Result {
    let fixture = Fixture::new().await?;
    let canonical = Uuid::new_v4().to_string();
    let receiver =
        receiver().with_ting_organizations(&json!({"items":[{"id":canonical,"handle":"tos"}]}))?;
    let mut allowed = reference("000");
    allowed["org_id"] = json!(canonical);
    allowed["for"] = json!("alice");
    let mut wrong = allowed.clone();
    wrong["org_id"] = json!(Uuid::new_v4());
    let raw = serde_json::to_vec(&json!({"tings":[allowed,wrong]}))?;
    let items = fixture.client.hydrate_ting_batch(&raw, &receiver).await?;
    assert!(matches!(&items[0], HydratedTingItem::Message { .. }));
    assert!(matches!(
        &items[1],
        HydratedTingItem::Skipped(TingItem::Rejected {
            reason: TingRejection::WrongOrganization,
            ..
        })
    ));
    let seen = fixture.requests.lock().unwrap();
    assert_eq!(
        seen.len(),
        3,
        "fresh identity/discovery and only the mapped message GET"
    );
    assert!(seen.iter().all(|request| request.method == "GET"));
    Ok(())
}

#[tokio::test]
async fn mismatched_hook_binding_and_retired_sockets_cannot_dispatch_network_delivery() -> Result {
    let fixture = Fixture::new().await?;
    let mut wrong = receiver();
    wrong.actor.actor_type = ActorType::Silicon;
    let result = fixture
        .client
        .hydrate_ting_batch(
            &serde_json::to_vec(&json!({"tings":[reference("000")]}))?,
            &wrong,
        )
        .await;
    assert!(matches!(result, Err(Error::Configuration(_))));
    assert_eq!(
        fixture.requests.lock().unwrap().len(),
        2,
        "identity mismatch never fetches a message"
    );
    for result in [
        fixture.client.prewarm_shared().await,
        fixture
            .client
            .connect(&["alice".into()], "old-device")
            .await,
        fixture
            .client
            .connect_with_generation(&["alice".into()], "old-device", Some(1))
            .await,
    ] {
        assert!(
            matches!(result,Err(Error::Configuration(message)) if message.contains("Ting") && message.contains("register_delivery"))
        );
    }
    assert_eq!(
        fixture.requests.lock().unwrap().len(),
        2,
        "retired sockets perform no network request"
    );
    Ok(())
}
