//! The typed client against a local fixture: credentials, contract header, custodian and
//! allow-list routes, sync with long-poll, errors with hints, and Ting reference hydration.
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
    Activity, Client, SyncRequest,
    ting::{HydratedTingItem, TingItem, TingReceiverContext, TingRejection},
};

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
    base: String,
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
        let base = format!("http://{}", listener.local_addr()?);
        let requests = Arc::new(Mutex::new(Vec::new()));
        let router = Router::new()
            .route("/{*path}", any(handler))
            .with_state(requests.clone());
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        Ok(Self {
            base,
            requests,
            task,
        })
    }
    fn client(&self) -> Result<Client> {
        Ok(Client::new(&self.base)?
            .with_token("access-jwt")
            .with_telemetry(false))
    }
    fn seen(&self) -> Vec<Seen> {
        self.requests.lock().map(|r| r.clone()).unwrap_or_default()
    }
}

fn account(kind: &str, id: &str, uuid: &str) -> Value {
    json!({"type":kind,"id":id,"uuid":uuid})
}

fn delivery() -> Value {
    json!({"transport":"sync","ting_enabled":false,"sync_path":"/api/v1/sync","sync_long_poll_seconds":30,"dm_websocket_supported":false})
}

async fn handler(State(requests): State<Requests>, request: Request) -> Response {
    let method = request.method().to_string();
    let path = request.uri().path().to_owned();
    let query = request.uri().query().map(str::to_owned);
    let headers = request.headers().clone();
    let body = to_bytes(request.into_body(), 1024 * 1024)
        .await
        .unwrap_or_default();
    if let Ok(mut seen) = requests.lock() {
        seen.push(Seen {
            method: method.clone(),
            path: path.clone(),
            query: query.clone(),
            headers,
            body: serde_json::from_slice(&body).unwrap_or(Value::Null),
        });
    }
    let value = match path.as_str() {
        "/api/v1/accounts" => {
            json!({"app_id":"dm","accounts_url":"https://accounts.teamofsilicons.com",
            "api_base_url":"https://api.dm.teamofsilicons.com/api/v1","contract_version":4,
            "sign_in":{"device_flow":true},"proof_scopes":["dm.messages.read"],"delivery":delivery()})
        }
        "/api/v1/me" => {
            json!({"account":account("silicon","si:cos","K1E"),"custodian":account("carbon","c:ada","Ada"),
            "custodian_of":[],"scopes":[],"via_app":null,"delivery":delivery()})
        }
        "/api/v1/allow-list" => {
            json!({"silicon":account("silicon","si:cos","K1E"),"items":[{"account":account("carbon","c:bob","B0b"),
                "added_by":account("carbon","c:ada","Ada"),"added_at":"2026-10-10T10:00:00Z"}]})
        }
        "/api/v1/conversations" => json!({"items":[
            {"id":"c:ada::si:cos","participants":[account("carbon","c:ada","Ada"),account("silicon","si:cos","K1E")],
             "visible_to_custodians":[account("carbon","c:ada","Ada")],"last_message":null,"last_message_status":null,
             "created_at":"2026-10-10T10:00:00Z","updated_at":"2026-10-10T10:00:00Z"},
            {"id":"g:design","participants":[],"group":{"name":"Design","description":"","owners":[account("carbon","c:ada","Ada")]},
             "last_message":null,"created_at":"2026-10-10T10:00:00Z","updated_at":"2026-10-10T10:00:00Z"}],"next_cursor":null}),
        "/api/v1/groups/g:design/owners" => json!({"name":"Design","description":"","version":3,
            "owners":[account("carbon","c:ada","Ada"),account("carbon","c:bob","B0b")],"members":[],"pending_invitations":[]}),
        "/api/v1/groups/g:design/leave" | "/api/v1/groups/g:design/decline" => {
            return StatusCode::NO_CONTENT.into_response();
        }
        "/api/v1/invitations" => {
            json!({"items":[{"conversation_id":"g:design","name":"Design","description":"",
            "invitee":account("silicon","si:cos","K1E"),"owners":[account("carbon","c:ada","Ada")]}]})
        }
        "/api/v1/sync" => {
            if query
                .as_deref()
                .is_some_and(|q| q.contains("cursor=expired"))
            {
                return failure(
                    StatusCode::CONFLICT,
                    "sync_reset_required",
                    Some("Get a reset cursor."),
                );
            }
            json!({"events":[],"cursor":"durable-resume-cursor","has_more":false,"upper_sequence":7})
        }
        path if path.starts_with("/api/v1/presence/devices/") => {
            if method == "DELETE" {
                return StatusCode::NO_CONTENT.into_response();
            }
            json!({"presence":{"member_id":"si:cos","member_uuid":"K1E","availability":"online","activity":"typing","last_seen_at":null},
                "lease_expires_at":"2026-10-10T10:02:00Z","activity_expires_at":"2026-10-10T10:00:10Z"})
        }
        "/api/v1/messages" => {
            return failure(
                StatusCode::FORBIDDEN,
                "contact_not_allowed",
                Some("Ask si:other's custodian to allow you."),
            );
        }
        "/api/v1/conversations/c:ada::si:cos/messages/000" => {
            json!({"message-id":"000","conversation_id":"c:ada::si:cos",
            "sender":account("carbon","c:ada","Ada"),"recipient_id":"si:cos","message":null,"attachments":[],
            "created_at":"2026-10-10T10:00:00Z","deleted_at":"2026-10-10T10:01:00Z"})
        }
        "/api/v1/conversations/c:ada::si:cos/messages/001" => {
            return failure(StatusCode::NOT_FOUND, "not_found", None);
        }
        "/api/v1/conversations/c:ada::si:cos/messages/002" => {
            return failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "dependency_unavailable",
                None,
            );
        }
        _ => return failure(StatusCode::NOT_FOUND, "not_found", None),
    };
    Json(json!({"type":"fixture","data":value})).into_response()
}

fn failure(status: StatusCode, code: &str, hint: Option<&str>) -> Response {
    let mut error = json!({"code":code,"message":"fixture error"});
    if let Some(hint) = hint {
        error["hint"] = json!(hint);
    }
    (status, Json(json!({"type":"error","data":{"error":error}}))).into_response()
}

#[tokio::test]
async fn requests_carry_the_bearer_or_proof_and_contract_4_and_discovery_carries_none() -> Result {
    let fixture = Fixture::new().await?;
    let discovery = fixture.client()?.accounts().await?;
    assert_eq!(discovery.app_id, "dm");
    assert_eq!(discovery.contract_version, 4);
    let me = fixture.client()?.me().await?;
    assert_eq!(me.account.uuid.as_deref(), Some("K1E"));
    assert_eq!(me.custodian.and_then(|c| c.uuid).as_deref(), Some("Ada"));
    Client::new(&fixture.base)?
        .with_proof("sap_fixture-proof")
        .me()
        .await?;
    let seen = fixture.seen();
    assert!(
        seen[0].headers.get("authorization").is_none(),
        "discovery sends no credential"
    );
    assert_eq!(seen[1].headers["authorization"], "Bearer access-jwt");
    assert_eq!(seen[2].headers["authorization"], "Proof sap_fixture-proof");
    for request in &seen {
        assert_eq!(request.headers["x-dm-contract-version"], "4");
        assert!(request.headers.get("x-org-id").is_none());
    }
    Ok(())
}

#[tokio::test]
async fn custodian_allow_list_group_and_invitation_routes_use_their_contract() -> Result {
    let fixture = Fixture::new().await?;
    let client = fixture.client()?;
    let conversations = client
        .conversations_for(&Default::default(), Some("si:cos"))
        .await?;
    assert_eq!(conversations.items[0].visible_to_custodians[0].id, "c:ada");
    let groups = client.groups(&Default::default()).await?;
    assert_eq!(groups.items.len(), 1);
    let list = client.allow(&["c:bob".into()], Some("si:cos")).await?;
    assert_eq!(list.items[0].account.uuid.as_deref(), Some("B0b"));
    assert_eq!(
        list.items[0].added_by.as_ref().map(|a| a.id.as_str()),
        Some("c:ada")
    );
    let owners = client
        .add_group_owners("g:design", &["c:bob".into()], "owner-key")
        .await?;
    assert_eq!(owners.owners.len(), 2);
    assert_eq!(owners.version, Some(3));
    client.leave_group("g:design").await?;
    client
        .decline_invitation("g:design", Some("si:cos"))
        .await?;
    let invitations = client.invitations(None).await?;
    assert_eq!(invitations.items[0].invitee.id, "si:cos");
    let seen = fixture.seen();
    assert_eq!(seen[0].query.as_deref(), Some("for=si%3Acos"));
    assert_eq!(
        (seen[2].method.as_str(), seen[2].query.as_deref()),
        ("POST", Some("for=si%3Acos"))
    );
    assert_eq!(
        seen[2].body,
        json!({"type":"allow_list_add","data":{"account_ids":["c:bob"]}})
    );
    assert_eq!(seen[3].headers["idempotency-key"], "owner-key");
    assert_eq!(seen[3].body["type"], "add_group_owners");
    assert_eq!(seen[4].path, "/api/v1/groups/g:design/leave");
    assert_eq!(seen[5].query.as_deref(), Some("for=si%3Acos"));
    Ok(())
}

#[tokio::test]
async fn sync_long_polls_and_errors_keep_code_message_and_hint() -> Result {
    let fixture = Fixture::new().await?;
    let client = fixture.client()?;
    let page = client
        .sync(&SyncRequest {
            cursor: Some("c".into()),
            limit: Some(10),
            reset: false,
            wait: Some(30),
        })
        .await?;
    assert_eq!(page.cursor, "durable-resume-cursor");
    assert_eq!(
        fixture.seen()[0].query.as_deref(),
        Some("cursor=c&limit=10&wait=30")
    );
    let reset = client
        .sync(&SyncRequest {
            cursor: Some("expired".into()),
            ..SyncRequest::default()
        })
        .await
        .err()
        .ok_or("expected a reset")?;
    assert!(reset.sync_reset_required());
    assert_eq!(reset.hint(), Some("Get a reset cursor."));
    assert!(
        client
            .sync(&SyncRequest {
                wait: Some(31),
                ..SyncRequest::default()
            })
            .await
            .is_err()
    );
    let refused = client
        .send_to_recipient(
            &silicon_dm_client::MessageCreate {
                recipient_id: Some("si:other".into()),
                text: Some("hello".into()),
                ..Default::default()
            },
            "send-key",
        )
        .await
        .err()
        .ok_or("expected a refusal")?;
    assert_eq!(
        (refused.code(), refused.status()),
        ("contact_not_allowed", Some(403))
    );
    assert!(refused.hint().is_some_and(|h| h.contains("custodian")));
    let lease = client
        .renew_presence("device 1", Some(Activity::Typing))
        .await?;
    assert_eq!(lease.presence.member_uuid.as_deref(), Some("K1E"));
    client.close_presence("device 1").await?;
    assert!(
        fixture
            .seen()
            .iter()
            .any(|s| s.path == "/api/v1/presence/devices/device%201")
    );
    Ok(())
}

fn reference(message: &str, key: &str) -> Value {
    json!({"id":format!("ting_{message}"),"type":"dm.sync.changed","key":key,"metadata":{},
        "data":{"schema_version":2,"event":"message.created","conversation_id":"c:ada::si:cos","message_id":message,
            "delivery_id":key,"delivery_sequence":1}})
}

#[tokio::test]
async fn ting_references_hydrate_through_http_without_acknowledging_anything() -> Result {
    let fixture = Fixture::new().await?;
    let client = fixture.client()?;
    let me = client.me().await?;
    let receiver = TingReceiverContext::for_account("dm", &me).ok_or("uuid")?;
    let keys = [
        "00000000-0000-4000-8000-000000000001",
        "00000000-0000-4000-8000-000000000002",
        "00000000-0000-4000-8000-000000000003",
    ];
    let mut legacy = reference("000", keys[0]);
    legacy["data"]["org_id"] = json!("tos");
    let batch = json!({"tings":[reference("000", keys[0]),reference("001", keys[1]),reference("002", keys[2]),
        {"id":"other","type":"remind.sync.changed"},legacy]});
    let items = client
        .hydrate_ting_batch(&serde_json::to_vec(&batch)?, &receiver)
        .await?;
    assert!(
        matches!(&items[0], HydratedTingItem::Message { message, .. } if message.deleted_at.is_some())
    );
    assert!(matches!(items[1], HydratedTingItem::Inaccessible { .. }));
    assert!(matches!(&items[2], HydratedTingItem::Failed { error, .. } if error.retryable()));
    assert!(matches!(
        items[3],
        HydratedTingItem::Skipped(TingItem::Unrelated { index: 3 })
    ));
    assert!(matches!(
        items[4],
        HydratedTingItem::Skipped(TingItem::Rejected {
            reason: TingRejection::LegacyReference,
            ..
        })
    ));
    // Only reads: nothing was marked delivered or read.
    assert!(fixture.seen().iter().all(|s| s.method == "GET"));
    Ok(())
}
