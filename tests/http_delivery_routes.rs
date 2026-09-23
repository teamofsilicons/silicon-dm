//! Authenticated HTTP replacement routes against an owned local PostgreSQL database.

mod support;

use std::{
    collections::BTreeSet,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use secrecy::{ExposeSecret as _, SecretString};
use serde_json::{Value, json};
use silicon_dm::{
    AppError, AppResult,
    application::{
        auth::{ApplicationSession, AuthContext, PresentedCredential},
        commands::{CreateConversationCommand, SendMessageCommand},
        ports::{AuthenticationRequest, IdentityProvider},
        state::AppState,
    },
    config::{
        DatabaseSettings, IamSettings, ProviderSettings, RealtimeSettings, RuntimeEnvironment,
        ServerSettings, Settings, TelemetrySettings, TingSettings, WorkerSettings,
    },
    domain::{ActorId, ActorRef, ActorType, MessageCreate},
    infrastructure::{giphy::GiphyClient, postgres::PostgresStore},
    realtime::RealtimeHub,
};
use uuid::Uuid;

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

struct Identity(AtomicUsize);

#[async_trait]
impl IdentityProvider for Identity {
    async fn authenticate(&self, request: AuthenticationRequest<'_>) -> AppResult<AuthContext> {
        let AuthenticationRequest::Bearer {
            token,
            organization_id,
        } = request;
        let actor = token.expose_secret();
        if !["c:alice", "c:bob", "unavailable"].contains(&actor) {
            return Err(AppError::Unauthorized);
        }
        Ok(AuthContext {
            organization_id: organization_id.clone(),
            actor: ActorRef {
                actor_type: ActorType::Carbon,
                id: actor.parse().map_err(|_| AppError::Unauthorized)?,
            },
            session_id: None,
            org_role: None,
            tag_ids: None,
            represented_actor_ids: BTreeSet::new(),
            capabilities: BTreeSet::new(),
            credential: PresentedCredential::Bearer(token.clone()),
            credential_expires_at: time::OffsetDateTime::now_utc() + time::Duration::hours(1),
        })
    }
    async fn login(&self, _: &SecretString, _: &str) -> AppResult<ApplicationSession> {
        Err(AppError::Unauthorized)
    }
    async fn refresh(&self, _: &SecretString, _: &str) -> AppResult<ApplicationSession> {
        Err(AppError::Unauthorized)
    }
    async fn logout(&self, _: &SecretString, _: &str) -> AppResult<()> {
        Err(AppError::Unauthorized)
    }
    fn verify_webhook(
        &self,
        _: &http::HeaderMap,
        _: &[u8],
    ) -> AppResult<silicon_iam_client::models::WebhookEvent> {
        Err(AppError::Unauthorized)
    }
    async fn authorize_participants(
        &self,
        _: &AuthContext,
        ids: &[ActorId],
    ) -> AppResult<Vec<ActorRef>> {
        Ok(ids
            .iter()
            .map(|id| ActorRef {
                actor_type: ActorType::Carbon,
                id: id.clone(),
            })
            .collect())
    }
    async fn authorize_presence(&self, context: &AuthContext, id: &ActorId) -> AppResult<ActorRef> {
        if context.actor.id == *id {
            Ok(context.actor.clone())
        } else {
            Err(AppError::Forbidden)
        }
    }
    async fn register_ting_delivery(
        &self,
        context: &AuthContext,
        _: &TingSettings,
        _: &str,
    ) -> AppResult<Value> {
        self.0.fetch_add(1, Ordering::SeqCst);
        if context.actor.id.as_str() == "unavailable" {
            return Err(AppError::DependencyUnavailable { dependency: "ting" });
        }
        Ok(
            json!({"id":format!("sub_{}", context.actor.id),"app_id":"dm","for":context.actor.id,"active":true}),
        )
    }
}

fn settings(database: DatabaseSettings) -> Result<Settings> {
    Ok(Settings {
        environment: RuntimeEnvironment::Test,
        server: ServerSettings {
            bind_addr: "127.0.0.1:0".parse()?,
            public_base_url: "http://127.0.0.1:8080/api/v1".parse()?,
            request_timeout: Duration::from_secs(10),
            max_body_bytes: 1024 * 1024,
            shutdown_timeout: Duration::from_secs(1),
        },
        database,
        iam: IamSettings {
            base_url: "http://127.0.0.1:8081".parse()?,
            app_id: "dm".into(),
            app_secret: SecretString::from("cursor-signing-secret"),
            webhook_secret: SecretString::from("w".repeat(32)),
            webhook_key_version: 1,
            request_timeout: Duration::from_secs(5),
        },
        ting: TingSettings {
            base_url: "http://127.0.0.1:8082".parse()?,
            request_timeout: Duration::from_secs(5),
        },
        testing: None,
        providers: ProviderSettings {
            giphy_api_base_url: "http://127.0.0.1:8083".parse()?,
            giphy_api_key: SecretString::from("test"),
            request_timeout: Duration::from_secs(1),
            trending_cache_ttl: Duration::from_secs(60),
        },
        realtime: RealtimeSettings {
            heartbeat_interval: Duration::from_secs(30),
            heartbeat_timeout: Duration::from_secs(120),
            outbound_capacity: 64.try_into()?,
            activity_ttl: Duration::from_secs(10),
        },
        worker: WorkerSettings {
            batch_size: 100.try_into()?,
            poll_interval: Duration::from_millis(100),
            lease_duration: Duration::from_secs(30),
            max_attempts: 20,
            max_retry_delay: Duration::from_secs(30),
        },
        telemetry: TelemetrySettings {
            enabled: false,
            table_key: None,
            home: std::env::temp_dir().join("dm-http-route-tests"),
        },
        reporting: None,
        log_filter: "error".into(),
    })
}

async fn check_delivery_discovery_and_retirement(client: &reqwest::Client, base: &str) -> Result {
    for path in ["/api/v1/ws", "/api/v1/ws/shared"] {
        for protocol in ["2", "5", "999"] {
            let response = client
                .get(format!("{base}{path}"))
                .header("Connection", "Upgrade")
                .header("Upgrade", "websocket")
                .header("Sec-WebSocket-Version", "13")
                .header("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ==")
                .header("X-DM-Protocol-Version", protocol)
                .header("X-Testing-Environment-Key", "retired-client-key")
                .send()
                .await?;
            assert_eq!(response.status(), 410);
            assert!(!response.headers().contains_key("x-dm-protocol-version"));
            let body: Value = response.json().await?;
            assert_eq!(body["type"], "error");
            assert_eq!(body["data"]["error"]["code"], "delivery_moved_to_ting");
            assert_eq!(body["data"]["delivery"]["dm_websocket_supported"], false);
        }
    }
    let iam: Value = client
        .get(format!("{base}/api/v1/iam"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let response = client
        .get(format!("{base}/api/v1/contracts"))
        .send()
        .await?;
    assert!(!response.headers().contains_key("x-dm-protocol-version"));
    assert_eq!(response.headers()["x-dm-contract-version"], "3");
    let contracts: Value = response.error_for_status()?.json().await?;
    assert_eq!(contracts["data"]["delivery"], iam["data"]["delivery"]);
    assert_eq!(contracts["data"]["compatibility"], json!([{"http":3}]));
    assert_eq!(
        iam["data"]["delivery"]["browser_origin"],
        "https://ting.teamofsilicons.com"
    );
    assert_eq!(iam["data"]["delivery"]["sync_path"], "/api/v1/sync");
    for row in contracts["data"]["contracts"]
        .as_array()
        .ok_or("contracts")?
    {
        if row["family"] == "websocket" || row["family"] == "shared" {
            assert_eq!(row["status"], "sunset");
        }
    }
    let rejected = client
        .get(format!("{base}/api/v1/contracts"))
        .header("X-DM-Contract-Version", "999")
        .send()
        .await?;
    assert_eq!(rejected.status(), 406);
    let rejected: Value = rejected.json().await?;
    assert_eq!(rejected["data"]["compatible"], json!({"http":[3]}));
    Ok(())
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "authenticated HTTP flow covers cursor recovery, presence, and explicit enrollment"
)]
async fn http_routes_scope_cursors_and_leases_and_deduplicate_enrollment() -> Result {
    let fixture = support::TestDatabase::start().await?;
    let database = DatabaseSettings {
        url: SecretString::from(fixture.url.clone()),
        max_connections: 4.try_into()?,
        min_connections: 1,
        acquire_timeout: Duration::from_secs(10),
        statement_timeout: Duration::from_secs(30),
    };
    let store = PostgresStore::connect(&database).await?;
    store.migrate().await?;
    let settings = settings(database)?;
    let identity = Arc::new(Identity(AtomicUsize::new(0)));
    let state = AppState {
        instance_id: "http-tests".into(),
        gifs: Arc::new(GiphyClient::new(&settings.providers)?),
        settings: Arc::new(settings),
        telemetry: silicon_dm::telemetry::Recorder::default(),
        store: store.clone(),
        identity: identity.clone(),
        testing: None,
        testing_environment: None,
        testing_generation: None,
        testing_runtime_revision: None,
        realtime: RealtimeHub::default(),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let server =
        tokio::spawn(
            async move { axum::serve(listener, silicon_dm::api::build_router(state)).await },
        );
    let client = reqwest::Client::new();
    check_delivery_discovery_and_retirement(&client, &base).await?;
    let org = format!("http_{}", Uuid::new_v4().simple());
    let request = |method, path: &str, actor: &str| {
        client
            .request(method, format!("{base}{path}"))
            .bearer_auth(actor)
            .header("X-Org-ID", &org)
    };
    assert_eq!(
        client
            .get(format!("{base}/api/v1/sync"))
            .header("X-Org-ID", &org)
            .send()
            .await?
            .status(),
        401
    );
    let initial: Value = request(reqwest::Method::GET, "/api/v1/sync?reset=true", "c:bob")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(initial["type"], "sync");
    assert_eq!(initial["data"]["events"], json!([]));
    let cursor = initial["data"]["cursor"].as_str().ok_or("cursor")?;
    assert_eq!(
        request(reqwest::Method::GET, "/api/v1/sync", "c:alice")
            .query(&[("cursor", cursor)])
            .send()
            .await?
            .status(),
        409
    );
    assert_eq!(
        client
            .get(format!("{base}/api/v1/sync"))
            .bearer_auth("c:bob")
            .header("X-Org-ID", "elsewhere")
            .query(&[("cursor", cursor)])
            .send()
            .await?
            .status(),
        409
    );
    assert_eq!(
        request(reqwest::Method::GET, "/api/v1/sync?limit=0", "c:bob")
            .send()
            .await?
            .status(),
        422
    );
    assert_eq!(
        request(
            reqwest::Method::GET,
            "/api/v1/sync?reset=true&cursor=invalid",
            "c:bob"
        )
        .send()
        .await?
        .status(),
        422
    );
    let invalid = format!("{cursor}x");
    assert_eq!(
        request(reqwest::Method::GET, "/api/v1/sync", "c:bob")
            .query(&[("cursor", invalid)])
            .send()
            .await?
            .status(),
        422
    );

    let alice = ActorRef {
        actor_type: ActorType::Carbon,
        id: "c:alice".parse()?,
    };
    let bob = ActorRef {
        actor_type: ActorType::Carbon,
        id: "c:bob".parse()?,
    };
    let conversation = store
        .create_conversation(CreateConversationCommand {
            organization_id: org.parse()?,
            creator: alice.clone(),
            participants: vec![alice.clone(), bob],
            idempotency_key: Uuid::new_v4().to_string().parse()?,
        })
        .await?;
    store
        .send_message(SendMessageCommand {
            organization_id: org.parse()?,
            conversation_id: conversation.id,
            sender: alice,
            content: MessageCreate {
                text: Some("HTTP retrieval remains authoritative".into()),
                ..MessageCreate::default()
            },
            idempotency_key: Uuid::new_v4().to_string().parse()?,
        })
        .await?;
    let page: Value = request(reqwest::Method::GET, "/api/v1/sync", "c:bob")
        .query(&[("cursor", cursor)])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(page["data"]["events"][0]["message_id"], "000");
    assert_eq!(
        page["data"]["events"][0]["conversation_id"],
        "c:alice::c:bob"
    );
    assert_eq!(page["data"]["has_more"], false);
    assert!(!page.to_string().contains("HTTP retrieval remains"));
    let lease: Value = request(
        reqwest::Method::PUT,
        "/api/v1/presence/devices/laptop",
        "c:bob",
    )
    .json(&json!({"type":"renew_presence","data":{"activity":"typing"}}))
    .send()
    .await?
    .error_for_status()?
    .json()
    .await?;
    assert_eq!(lease["data"]["presence"]["availability"], "online");
    assert_eq!(lease["data"]["presence"]["activity"], "typing");
    assert_eq!(
        request(
            reqwest::Method::PUT,
            "/api/v1/presence/devices/laptop",
            "c:bob"
        )
        .json(&json!({"type":"renew_presence","data":{"member_id":"c:alice"}}))
        .send()
        .await?
        .status(),
        422
    );
    assert_eq!(
        request(
            reqwest::Method::DELETE,
            "/api/v1/presence/devices/laptop",
            "c:alice"
        )
        .send()
        .await?
        .status(),
        204
    );
    let still_online: Value = request(reqwest::Method::GET, "/api/v1/presence/c:bob", "c:bob")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(still_online["data"]["availability"], "online");
    assert_eq!(
        request(
            reqwest::Method::DELETE,
            "/api/v1/presence/devices/laptop",
            "c:bob"
        )
        .send()
        .await?
        .status(),
        204
    );

    let key = Uuid::new_v4().to_string();
    for _ in 0..2 {
        let reply: Value = request(
            reqwest::Method::POST,
            "/api/v1/delivery/registration",
            "c:bob",
        )
        .header("Idempotency-Key", &key)
        .json(&json!({"type":"delivery_registration","data":{}}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
        assert_eq!(reply["data"]["for"], "c:bob");
    }
    assert_eq!(identity.0.load(Ordering::SeqCst), 1);
    request(
        reqwest::Method::POST,
        "/api/v1/delivery/registration",
        "c:alice",
    )
    .header("Idempotency-Key", &key)
    .json(&json!({"type":"delivery_registration","data":{}}))
    .send()
    .await?
    .error_for_status()?;
    assert_eq!(identity.0.load(Ordering::SeqCst), 2);
    assert_eq!(
        request(
            reqwest::Method::POST,
            "/api/v1/delivery/registration",
            "c:bob"
        )
        .header("Idempotency-Key", Uuid::new_v4().to_string())
        .json(&json!({"type":"delivery_registration","data":{"for":"c:alice"}}))
        .send()
        .await?
        .status(),
        422
    );
    assert_eq!(identity.0.load(Ordering::SeqCst), 2);
    let uncertain = Uuid::new_v4().to_string();
    for status in [503, 409] {
        assert_eq!(
            request(
                reqwest::Method::POST,
                "/api/v1/delivery/registration",
                "unavailable"
            )
            .header("Idempotency-Key", &uncertain)
            .json(&json!({"type":"delivery_registration","data":{}}))
            .send()
            .await?
            .status(),
            status
        );
    }
    assert_eq!(identity.0.load(Ordering::SeqCst), 3);
    server.abort();
    store.pool().close().await;
    Ok(())
}
