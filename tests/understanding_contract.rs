//! Integration coverage for IAM discovery, contract retirement and shared profiles.
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use futures::{SinkExt as _, StreamExt as _};
use secrecy::{ExposeSecret as _, SecretString};
use serde_json::{Value, json};
use silicon_dm::{
    AppError, AppResult,
    application::{
        auth::{ApplicationSession, AuthContext, PresentedCredential},
        ports::{AuthenticationRequest, IdentityProvider},
        state::AppState,
    },
    config::*,
    domain::{ActorId, ActorRef, ActorType, OrganizationId},
    infrastructure::{giphy::GiphyClient, postgres::PostgresStore},
    realtime::RealtimeHub,
};
use std::{collections::BTreeSet, sync::Arc, time::Duration};
use testcontainers::{ImageExt as _, runners::AsyncRunner as _};
use testcontainers_modules::postgres::Postgres;
use uuid::Uuid;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, path},
};
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
struct Identity;
#[async_trait]
impl IdentityProvider for Identity {
    async fn authenticate(&self, request: AuthenticationRequest<'_>) -> AppResult<AuthContext> {
        let AuthenticationRequest::Bearer {
            token,
            organization_id,
        } = request;
        let id = token
            .expose_secret()
            .strip_prefix("token-")
            .ok_or(AppError::Unauthorized)?;
        if !["alice", "bob"].contains(&id) || organization_id.as_str() != "tos" {
            return Err(AppError::Unauthorized);
        }
        Ok(AuthContext {
            actor: ActorRef {
                id: id.parse().map_err(|_| AppError::Unauthorized)?,
                actor_type: ActorType::Carbon,
            },
            principal_id: Uuid::from_u128(if id == "alice" { 1 } else { 2 }),
            session_id: None,
            organization_id: organization_id.clone(),
            org_role: (id == "alice").then(|| "org_admin".into()),
            tag_ids: None,
            represented_actor_ids: BTreeSet::new(),
            capabilities: BTreeSet::new(),
            credential: PresentedCredential::Bearer(token.clone()),
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
                id: id.clone(),
                actor_type: ActorType::Carbon,
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
}
fn settings(url: String, iam: &str) -> Result<Settings> {
    let db = DatabaseSettings {
        url: SecretString::from(url),
        max_connections: 8.try_into()?,
        min_connections: 1,
        acquire_timeout: Duration::from_secs(10),
        statement_timeout: Duration::from_secs(10),
    };
    Ok(Settings {
        environment: RuntimeEnvironment::Test,
        server: ServerSettings {
            bind_addr: "127.0.0.1:0".parse()?,
            public_base_url: "http://localhost:8080/api/v1".parse()?,
            request_timeout: Duration::from_secs(15),
            max_body_bytes: 1024 * 1024,
            shutdown_timeout: Duration::from_secs(5),
        },
        database: db,
        iam: IamSettings {
            base_url: iam.parse()?,
            app_id: "tos>dm".into(),
            app_secret: SecretString::from("production-secret"),
            webhook_secret: SecretString::from("x".repeat(32)),
            webhook_key_version: 1,
            request_timeout: Duration::from_secs(5),
        },
        testing: None,
        providers: ProviderSettings {
            giphy_api_base_url: "https://api.giphy.com".parse()?,
            giphy_api_key: SecretString::from("test"),
            request_timeout: Duration::from_secs(5),
            trending_cache_ttl: Duration::from_secs(60),
        },
        realtime: RealtimeSettings {
            heartbeat_interval: Duration::from_secs(30),
            heartbeat_timeout: Duration::from_secs(120),
            outbound_capacity: 64.try_into()?,
            activity_ttl: Duration::from_secs(30),
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
            home: std::env::temp_dir().join("dm-test-telemetry"),
        },
        reporting: None,
        log_filter: "error".into(),
    })
}
async fn state(settings: Settings) -> Result<AppState> {
    let store = PostgresStore::connect(&settings.database).await?;
    store.migrate().await?;
    let testing = if let Some(test) = &settings.testing {
        Some(Arc::new(
            silicon_dm::testing::TestingRegistry::new(store.clone(), test, &settings.iam).await?,
        ))
    } else {
        None
    };
    Ok(AppState {
        instance_id: "test".into(),
        telemetry: silicon_dm::telemetry::Recorder::default(),
        gifs: Arc::new(GiphyClient::new(&settings.providers)?),
        settings: Arc::new(settings),
        store,
        identity: Arc::new(Identity),
        testing,
        testing_environment: None,
        testing_generation: None,
        realtime: RealtimeHub::default(),
    })
}
async fn mock_context(
    iam: &MockServer,
    id: Uuid,
    secret: &str,
    version: i64,
    cleaned: Option<&str>,
) {
    Mock::given(path("/api/v1/application/testing-context"))
        .and(header("x-testing-application",format!("Basic {}",STANDARD.encode(format!("tos>dm:{secret}")))))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"environment_id":id,"application":{"app_id":"tos>dm","base_url":"https://backend.dm.example","app_scope":{"iam":[],"external":[]},"webhook_scope":[],"testing_idle_days":15},"environment":{"environment_id":id,"org_id":"tos","name":format!("Sandbox {version}"),"version":version,"key_generation":1,"cleaned_at":cleaned,"created_at":"2026-09-01T00:00:00Z","creator_type":"carbon","creator_id":"alice"},"webhook_key_digest":"00".repeat(32)}))).mount(iam).await;
}
#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one fixture covers discovery and generation reset"
)]
async fn app_secret_discovery_is_isolated_revalidated_and_resets_generation() -> Result {
    let container = Postgres::default().with_tag("16-alpine").start().await?;
    let url = format!(
        "postgres://postgres:postgres@{}:{}/postgres",
        container.get_host().await?,
        container.get_host_port_ipv4(5432).await?
    );
    let iam = MockServer::start().await;
    let mut config = settings(url.clone(), &iam.uri())?;
    let admin = PostgresStore::connect(&config.database).await?;
    sqlx::query("CREATE DATABASE sandbox")
        .execute(admin.pool())
        .await?;
    let mut testing_db = config.database.clone();
    testing_db.url =
        SecretString::from(format!("{}/sandbox", url.rsplit_once('/').ok_or("url")?.0));
    config.testing = Some(TestingSettings {
        honeycomb_service_token: None,
        honeycomb_base_url: None,
        database: testing_db,
        encryption_key: SecretString::from(STANDARD.encode([7u8; 32])),
    });
    let app = state(config).await?;
    let registry = app.testing.as_ref().ok_or("registry")?;
    let id = Uuid::new_v4();
    let secret = format!("ask_{}", "a".repeat(43));
    mock_context(&iam, id, &secret, 1, None).await;
    let selected = registry.state_for_key(&app, &secret).await?;
    assert_eq!(selected.testing_environment, Some(id));
    assert_ne!(
        selected.store.pool().connect_options().get_database(),
        app.store.pool().connect_options().get_database()
    );
    let org: OrganizationId = "tos".parse()?;
    let actor = ActorRef {
        id: "alice".parse()?,
        actor_type: ActorType::Carbon,
    };
    selected.store.refresh_directory(&org, &[actor]).await?;
    sandbox_effects_are_isolated(&app, selected.clone()).await?;
    let selected_again = registry.state_for_key(&app, &secret).await?;
    assert_eq!(
        selected.testing_generation,
        selected_again.testing_generation
    );
    let (one, two) = tokio::join!(
        registry.state_for_key(&app, &secret),
        registry.state_for_key(&app, &secret)
    );
    assert_eq!(one?.testing_generation, two?.testing_generation);
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM dm.testing_environments")
        .fetch_one(app.store.pool())
        .await?;
    assert_eq!(rows, 1);
    let production_actors: i64 = sqlx::query_scalar("SELECT count(*) FROM actor_snapshots")
        .fetch_one(app.store.pool())
        .await?;
    assert_eq!(production_actors, 0);
    assert!(
        registry
            .state_for_key(&app, &format!("ask_{}", "b".repeat(43)))
            .await
            .is_err()
    );
    iam.reset().await;
    mock_context(&iam, id, &secret, 2, None).await;
    let renamed = registry.state_for_key(&app, &secret).await?;
    assert_eq!(renamed.testing_generation, selected.testing_generation);
    assert_eq!(registry.selected_metadata(id).await?.name, "Sandbox 2");
    iam.reset().await;
    mock_context(&iam, id, &secret, 3, Some("2026-09-13T00:00:00Z")).await;
    let reset = registry.state_for_key(&app, &secret).await?;
    assert!(reset.testing_generation > selected.testing_generation);
    let actors: i64 = sqlx::query_scalar("SELECT count(*) FROM actor_snapshots")
        .fetch_one(reset.store.pool())
        .await?;
    assert_eq!(actors, 0);
    assert!(
        registry
            .ensure_active(id, selected.testing_generation.ok_or("generation")?)
            .await
            .is_err()
    );
    iam.reset().await;
    assert!(registry.state_for_key(&app, &secret).await.is_err());
    // No root authority can be inferred from this application's secret.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server =
        tokio::spawn(
            async move { axum::serve(listener, silicon_dm::api::build_router(app)).await },
        );
    let response = reqwest::Client::new()
        .post(format!(
            "http://{address}/api/v1/testing-environments/{id}/clean"
        ))
        .header("X-Testing-Environment-Key", secret)
        .header("Idempotency-Key", "attempt-clean")
        .send()
        .await?;
    assert_eq!(response.status(), 403);
    server.abort();
    Ok(())
}
#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one integration fixture covers shared transport and its lifecycle contract"
)]
async fn shared_socket_authenticates_each_profile_and_contracts_retire_after_idle() -> Result {
    let container = Postgres::default().with_tag("16-alpine").start().await?;
    let url = format!(
        "postgres://postgres:postgres@{}:{}/postgres",
        container.get_host().await?,
        container.get_host_port_ipv4(5432).await?
    );
    let app = state(settings(url, "http://localhost:9999")?).await?;
    let org: OrganizationId = "tos".parse()?;
    app.store
        .refresh_directory(
            &org,
            &[
                ActorRef {
                    id: "alice".parse()?,
                    actor_type: ActorType::Carbon,
                },
                ActorRef {
                    id: "bob".parse()?,
                    actor_type: ActorType::Carbon,
                },
            ],
        )
        .await?;
    let pool = app.store.pool().clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let server =
        tokio::spawn(
            async move { axum::serve(listener, silicon_dm::api::build_router(app)).await },
        );
    let client = silicon_dm_client::Client::new(&base)?;
    let mut socket = client.prewarm_shared().await?;
    let first = socket.next().await.ok_or("prewarm missing")??;
    assert!(first.to_text()?.contains("prewarmed"));
    for (id, actor, token) in [
        ("a", "alice", "token-alice"),
        ("b", "bob", "token-bob"),
        ("invalid", "bob", "token-alice"),
    ] {
        socket.send(tokio_tungstenite::tungstenite::Message::Text(json!({"type":"subscribe","data":{"subscription_id":id,"actor_id":actor,"token":token,"organization_id":"tos","device_id":format!("device-{id}"),"testing_key":null,"testing_generation":null}}).to_string().into())).await?;
    }
    let mut ready = BTreeSet::new();
    let mut rejected = false;
    tokio::time::timeout(Duration::from_secs(10), async {
        while ready.len() < 2 || !rejected {
            let frame = socket.next().await.ok_or("socket ended")??;
            if !frame.is_text() {
                continue;
            }
            let value: Value = serde_json::from_str(frame.to_text()?)?;
            if value["type"] == "ping" {
                socket
                    .send(tokio_tungstenite::tungstenite::Message::Text(
                        json!({"type":"pong","data":{"ping_id":value["data"]["ping_id"]}})
                            .to_string()
                            .into(),
                    ))
                    .await?;
                continue;
            }
            let id = value["data"]["subscription_id"].as_str().ok_or("id")?;
            if value["data"]["frame"]["type"] == "ready" {
                ready.insert(id.to_owned());
                let expected = if id == "a" { "alice" } else { "bob" };
                assert_eq!(value["data"]["frame"]["data"]["actors"], json!([expected]));
            }
            if id == "invalid" {
                assert_eq!(
                    value["data"]["frame"]["data"]["code"],
                    "subscription_rejected"
                );
                rejected = true;
            }
        }
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })
    .await??;
    socket.close(None).await?;
    daemon_uses_one_connection(&base, &pool).await?;
    let http = reqwest::Client::new();
    assert_eq!(
        http.get(format!("{base}/api/v1/iam"))
            .header("X-DM-Contract-Version", "999")
            .send()
            .await?
            .status(),
        406
    );
    sqlx::query("UPDATE contract_versions SET status='deprecated',deprecated_at=clock_timestamp()-INTERVAL '8 days',last_request_at=clock_timestamp()-INTERVAL '8 days' WHERE family='http'").execute(&pool).await?;
    assert_eq!(
        http.get(format!("{base}/api/v1/iam"))
            .send()
            .await?
            .status(),
        410
    );
    let contracts = client.contracts().await?;
    assert!(
        contracts["contracts"]
            .as_array()
            .ok_or("contracts")?
            .iter()
            .any(|row| row["family"] == "http" && row["status"] == "sunset")
    );
    server.abort();
    Ok(())
}

#[tokio::test]
async fn reports_retry_postmark_and_deduplicate_without_sending_test_email() -> Result {
    let container = Postgres::default().with_tag("16-alpine").start().await?;
    let url = format!(
        "postgres://postgres:postgres@{}:{}/postgres",
        container.get_host().await?,
        container.get_host_port_ipv4(5432).await?
    );
    let postmark = MockServer::start().await;
    let mut config = settings(url, "http://localhost:9999")?;
    config.reporting = Some(ReportingSettings {
        endpoint: format!("{}/email", postmark.uri()).parse()?,
        token: SecretString::from("mock-postmark"),
    });
    let app = state(config).await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let route_state = app.clone();
    let server = tokio::spawn(async move {
        axum::serve(listener, silicon_dm::api::build_router(route_state)).await
    });
    let client = silicon_dm_client::Client::new(&base)?.with_auth("token-alice", "tos");
    let first = client
        .report("Fixture report only", None, "report-idempotency")
        .await?;
    assert_eq!(first["notification"], "queued");
    assert_eq!(
        first["id"],
        client
            .report("Fixture report only", None, "report-idempotency")
            .await?["id"]
    );
    assert!(
        client
            .report("Different report", None, "report-idempotency")
            .await
            .is_err()
    );
    Mock::given(path("/email"))
        .and(header("X-Postmark-Server-Token", "mock-postmark"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&postmark)
        .await;
    silicon_dm::reporting::deliver_one(&app).await?;
    let attempts: i32 = sqlx::query_scalar("SELECT attempts FROM bug_reports")
        .fetch_one(app.store.pool())
        .await?;
    assert_eq!(attempts, 1);
    postmark.reset().await;
    Mock::given(path("/email"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"ErrorCode":0,"MessageID":"mock-accepted"})),
        )
        .expect(1)
        .mount(&postmark)
        .await;
    sqlx::query("UPDATE bug_reports SET next_attempt_at=now()")
        .execute(app.store.pool())
        .await?;
    silicon_dm::reporting::deliver_one(&app).await?;
    assert_eq!(
        client
            .report("Fixture report only", None, "report-idempotency")
            .await?["notification"],
        "sent"
    );
    let requests = postmark.received_requests().await.ok_or("requests")?;
    let body: Value = serde_json::from_slice(&requests[0].body)?;
    assert_eq!(
        body["To"],
        "saketdev12@gmail.com,shubhastro2@gmails.com,bugs@teamofsilicons.com"
    );
    assert_eq!(body["TrackOpens"], false);
    let mut sandbox = app.clone();
    sandbox.testing_environment = Some(Uuid::new_v4());
    sqlx::query("UPDATE bug_reports SET status='queued',next_attempt_at=now()")
        .execute(app.store.pool())
        .await?;
    silicon_dm::reporting::deliver_one(&sandbox).await?;
    // A sandbox-scoped worker never invokes even a configured production transport.
    assert_eq!(
        postmark.received_requests().await.ok_or("requests")?.len(),
        1
    );
    server.abort();
    Ok(())
}

async fn daemon_uses_one_connection(base: &str, pool: &sqlx::PgPool) -> Result {
    use silicon_dm_client::runtime::{
        LocalRuntime,
        store::{Profile, now, session_key},
    };
    let directory = tempfile::tempdir()?;
    let runtime = LocalRuntime::new(directory.path())?;
    let port = std::net::TcpListener::bind("127.0.0.1:0")?
        .local_addr()?
        .port();
    runtime.store().update(|config| {
        config.relay_port=port;config.auto_update=false;config.telemetry_enabled=false;
        for (name,url) in [("alice",base.to_owned()),("bob",format!("{base}/api/v1/"))] {
            let tokens=serde_json::from_value(json!({"access_token":format!("token-{name}"),"refresh_token":"refresh-fixture","token_type":"Bearer","scope":"messaging","expires_in":3600,"organization_id":"tos","actor":{"type":"carbon","id":name}}))?;
            config.profiles.insert(session_key(name,None),Profile{name:name.into(),base_url:url,tokens,webhook_url:None,device_id:format!("runtime-{name}"),expires_at:now()+3600,testing_environment_id:None,enabled:true});
        }
        Ok(())
    })?;
    let before: i64 =
        sqlx::query_scalar("SELECT requests FROM contract_versions WHERE family='shared'")
            .fetch_one(pool)
            .await?;
    let relay = runtime.client()?;
    let task = tokio::spawn(async move { runtime.run().await });
    tokio::time::timeout(Duration::from_secs(12), async {
        loop {
            if let Ok(status) = relay.status().await {
                let profiles = status["profiles"].as_object().ok_or("profiles")?;
                if profiles.len() == 2 && profiles.values().all(|p| p["state"] == "connected") {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
    })
    .await??;
    let after: i64 =
        sqlx::query_scalar("SELECT requests FROM contract_versions WHERE family='shared'")
            .fetch_one(pool)
            .await?;
    assert_eq!(
        after - before,
        1,
        "origin and /api/v1 aliases share a physical connection"
    );
    task.abort();
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "sandbox effects and opt-out share a single generation fence"
)]
async fn sandbox_effects_are_isolated(production: &AppState, mut sandbox: AppState) -> Result {
    sandbox.identity = Arc::new(Identity);
    Arc::make_mut(&mut sandbox.settings).telemetry.enabled = true;
    let pool = sandbox.store.pool().clone();
    sqlx::query("INSERT INTO iam_membership_projections(membership_id,principal_id,iam_organization_id,organization_id,actor_kind,actor_id,iam_version,authorization_epoch,status) VALUES($1,$2,$3,'tos','carbon','alice',1,1,'active')")
        .bind(Uuid::new_v4()).bind(Uuid::new_v4()).bind(Uuid::nil()).execute(&pool).await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let server =
        tokio::spawn(
            async move { axum::serve(listener, silicon_dm::api::build_router(sandbox)).await },
        );
    let client = silicon_dm_client::Client::new(&base)?.with_auth("token-alice", "tos");
    assert_eq!(
        client
            .report("Sandbox fixture", None, "sandbox-report-fixture")
            .await?["notification"],
        "simulated"
    );
    let input = silicon_dm_client::models::GroupCreate {
        settings: silicon_dm_client::models::GroupSettings {
            name: "Secret sandbox group".into(),
            description: "Private description".into(),
            is_public: false,
            tag_ids: vec![],
        },
        member_ids: vec![],
    };
    let group = client.create_group(&input, "sandbox-group-fixture").await?;
    assert!(group.group.is_some());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM groups")
            .fetch_one(production.store.pool())
            .await?,
        0
    );
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let event: Option<Value> = sqlx::query_scalar(
                "SELECT event FROM telemetry_events WHERE event->>'event'='group.created'",
            )
            .fetch_optional(&pool)
            .await?;
            if let Some(event) = event {
                assert_eq!(event["context"]["group_id"], group.id);
                assert!(!event.to_string().contains("Secret sandbox"));
                assert!(!event.to_string().contains("Private description"));
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
    })
    .await??;
    client
        .with_source("cli")
        .telemetry("command", true, 12)
        .await?;
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let count: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM telemetry_events WHERE event->>'source'='cli' AND EXISTS(SELECT 1 FROM telemetry_events WHERE event->>'event'='http.completed' AND event->'context'->>'route' LIKE '%/groups')",
            )
            .fetch_one(&pool)
            .await?;
            if count > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
    })
    .await??;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM bug_reports")
        .fetch_one(production.store.pool())
        .await?;
    assert_eq!(count, 0);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM telemetry_events")
        .fetch_one(production.store.pool())
        .await?;
    assert_eq!(count, 0);
    let client = silicon_dm_client::Client::new(&base)?
        .with_auth("token-alice", "tos")
        .with_telemetry(false);
    let before: i64 = sqlx::query_scalar("SELECT count(*) FROM telemetry_events")
        .fetch_one(&pool)
        .await?;
    client.telemetry("command", false, 12).await?;
    client.iam().await?;
    let mut input = input;
    input.settings.name = "Opt out group".into();
    client.create_group(&input, "sandbox-group-opt-out").await?;
    let after: i64 = sqlx::query_scalar("SELECT count(*) FROM telemetry_events")
        .fetch_one(&pool)
        .await?;
    assert_eq!(
        after, before,
        "opt-out excludes client and server request events"
    );
    server.abort();
    Ok(())
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "exercise the public SDK and HTTP group lifecycle end to end"
)]
async fn groups_work_through_sdk_and_http_with_admin_and_retry_guards() -> Result {
    use silicon_dm_client::{
        Client,
        models::{GroupCreate, GroupSettings, MessageCreate, PageRequest},
    };
    let container = Postgres::default().with_tag("16-alpine").start().await?;
    let url = format!(
        "postgres://postgres:postgres@{}:{}/postgres",
        container.get_host().await?,
        container.get_host_port_ipv4(5432).await?
    );
    let app = state(settings(url, "http://localhost:9999")?).await?;
    let org: OrganizationId = "tos".parse()?;
    for id in ["alice", "bob"] {
        let actor = ActorRef {
            id: id.parse()?,
            actor_type: ActorType::Carbon,
        };
        app.store.refresh_directory(&org, &[actor]).await?;
        sqlx::query("INSERT INTO iam_membership_projections(membership_id,principal_id,iam_organization_id,organization_id,actor_kind,actor_id,iam_version,authorization_epoch,status) VALUES($1,$2,$3,'tos','carbon',$4,1,1,'active')")
            .bind(Uuid::new_v4()).bind(Uuid::new_v4()).bind(Uuid::nil()).bind(id).execute(app.store.pool()).await?;
    }
    let group_pool = app.store.pool().clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let server =
        tokio::spawn(
            async move { axum::serve(listener, silicon_dm::api::build_router(app)).await },
        );
    let owner = Client::new(&base)?.with_auth("token-alice", "tos");
    let reader = Client::new(&base)?.with_auth("token-bob", "tos");
    let input = GroupCreate {
        settings: GroupSettings {
            name: "Delivery team".into(),
            description: "Full history".into(),
            is_public: false,
            tag_ids: vec![],
        },
        member_ids: vec![],
    };
    assert!(
        reader
            .create_group(&input, "reader-cannot-create")
            .await
            .is_err()
    );
    let group = owner.create_group(&input, "owner-creates-group").await?;
    assert_eq!(group.id, "g:tos:delivery-team");
    let duplicate = owner
        .create_group(&input, "duplicate-name-different-key")
        .await;
    assert!(matches!(
        duplicate,
        Err(silicon_dm_client::Error::Api { status: 409, .. })
    ));
    assert_eq!(
        owner.create_group(&input, "owner-creates-group").await?.id,
        group.id
    );
    let collision = |name: &str| GroupCreate {
        settings: GroupSettings {
            name: name.into(),
            ..input.settings.clone()
        },
        member_ids: vec![],
    };
    let one = collision("  Product   Design!  ");
    let two = collision("product-design");
    let (one, two) = tokio::join!(
        owner.create_group(&one, "slug-collision-one"),
        owner.create_group(&two, "slug-collision-two")
    );
    assert!(one.is_ok() ^ two.is_ok());
    let winner = one
        .as_ref()
        .ok()
        .or_else(|| two.as_ref().ok())
        .ok_or("no collision winner")?;
    assert_eq!(winner.id, "g:tos:product-design");
    let loser = one
        .err()
        .or_else(|| two.err())
        .ok_or("no collision loser")?;
    assert!(matches!(
        loser,
        silicon_dm_client::Error::Api { status: 409, .. }
    ));
    assert!(matches!(
        owner.group("g:another-org:delivery-team").await,
        Err(silicon_dm_client::Error::Api { status: 404, .. })
    ));
    assert!(owner.group("../../auth/logout").await.is_err());
    assert!(matches!(
        owner
            .create_group(&collision("!!!"), "empty-slug-rejected")
            .await,
        Err(silicon_dm_client::Error::Api { status: 422, .. })
    ));
    assert_eq!(
        group.group.as_ref().ok_or("group")?.settings.description,
        "Full history"
    );
    assert!(reader.group(&group.id).await.is_err());
    let message = owner
        .send_message(
            &group.id,
            &MessageCreate {
                text: Some("Existing history".into()),
                ..MessageCreate::default()
            },
            "prior-group-message",
        )
        .await?;
    assert_eq!(message.conversation_id, group.id);
    let draft = owner
        .put_draft(
            &group.id,
            &serde_json::from_value(
                json!({"message":"Private draft", "metadata":{"conversation_id":"unchanged"}}),
            )?,
            0,
        )
        .await?;
    assert_eq!(draft.conversation_id, group.id);
    assert_eq!(draft.content.metadata["conversation_id"], "unchanged");
    owner.delete_draft(&group.id).await?;
    group_address_socket_roundtrip(&owner, &group.id, false).await?;
    group_address_socket_roundtrip(&owner, &group.id, true).await?;
    owner
        .invite_group_members(&group.id, &["bob".into()], "invite-bob-to-group")
        .await?;
    assert_eq!(
        reader
            .messages(&group.id, &PageRequest::default(), false)
            .await?
            .items
            .iter()
            .filter(|item| item.id == message.id)
            .count(),
        1
    );
    assert_eq!(reader.groups(&PageRequest::default()).await?.items.len(), 1);
    assert!(
        reader
            .invite_group_members(&group.id, &["alice".into()], "reader-cannot-invite")
            .await
            .is_err()
    );
    assert!(
        reader
            .remove_group_members(&group.id, &["alice".into()], "reader-cannot-remove")
            .await
            .is_err()
    );
    let current = owner.group(&group.id).await?.group.ok_or("group")?;
    let renamed = GroupSettings {
        name: "Renamed".into(),
        ..input.settings.clone()
    };
    let updated = owner
        .update_group(&group.id, &renamed, current.version, "rename-group-once")
        .await?;
    assert_eq!(updated.settings.name, "Renamed");
    assert_eq!(owner.group(&group.id).await?.id, "g:tos:delivery-team");
    assert_eq!(message.conversation_id, group.id);
    let legacy: Uuid = sqlx::query_scalar("SELECT conversation_id FROM groups WHERE public_id=$1")
        .bind(&group.id)
        .fetch_one(&group_pool)
        .await?;
    assert_eq!(owner.group(legacy).await?.id, group.id);
    assert_eq!(
        owner
            .messages(legacy, &PageRequest::default(), false)
            .await?
            .items[0]
            .conversation_id,
        group.id
    );
    assert!(
        owner
            .update_group(
                &group.id,
                &input.settings,
                current.version,
                "reject-stale-version"
            )
            .await
            .is_err()
    );
    assert_eq!(
        owner
            .update_group(&group.id, &renamed, current.version, "rename-group-once")
            .await?
            .version,
        updated.version
    );
    let removed = owner
        .remove_group_members(&group.id, &["bob".into()], "remove-bob-once")
        .await?;
    assert_eq!(
        owner
            .remove_group_members(&group.id, &["bob".into()], "remove-bob-once")
            .await?
            .version,
        removed.version
    );
    assert!(
        reader
            .messages(&group.id, &PageRequest::default(), false)
            .await
            .is_err()
    );
    assert!(reader.draft(&group.id).await.is_err());
    assert!(
        reader
            .groups(&PageRequest::default())
            .await?
            .items
            .is_empty()
    );
    let tag = Uuid::new_v4();
    sqlx::query("UPDATE iam_membership_projections SET tag_ids=$1 WHERE actor_id='bob'")
        .bind(vec![tag])
        .execute(&group_pool)
        .await?;
    owner
        .update_group(
            &group.id,
            &GroupSettings {
                tag_ids: vec![tag],
                ..renamed
            },
            removed.version,
            "tag-policy-for-narrow-token",
        )
        .await?;
    // The cached IAM projection can grant membership while this specific token
    // discloses no tags. Both literal and encoded paths must still deny it.
    assert!(
        reader
            .messages(&group.id, &PageRequest::default(), false)
            .await
            .is_err()
    );
    let raw_id = &group.id;
    let encoded_id = format!("%{:02X}{}", raw_id.as_bytes()[0], &raw_id[1..]);
    let encoded = reqwest::Client::new()
        .get(format!("{base}/api/v1/conversations/{encoded_id}/messages"))
        .bearer_auth("token-bob")
        .header("X-Org-ID", "tos")
        .send()
        .await?;
    assert_eq!(encoded.status(), 404);
    let bad = reqwest::Client::new()
        .post(format!("{base}/api/v1/groups"))
        .bearer_auth("token-alice")
        .header("X-Org-ID", "tos")
        .header("Idempotency-Key", "wrong-wire-discriminator")
        .json(&json!({"type":"create_conversation","data":input}))
        .send()
        .await?;
    assert_eq!(bad.status(), 422);
    server.abort();
    Ok(())
}

async fn group_address_socket_roundtrip(
    client: &silicon_dm_client::Client,
    id: &str,
    shared: bool,
) -> Result {
    use tokio_tungstenite::tungstenite::Message as SocketMessage;
    let mut socket = if shared {
        client.prewarm_shared().await?
    } else {
        client.connect(&["alice".into()], "group-id-device").await?
    };
    let frame = socket.next().await.ok_or("opening frame missing")??;
    assert!(frame.is_text());
    if shared {
        socket.send(SocketMessage::Text(json!({"type":"subscribe","data":{"subscription_id":"group","actor_id":"alice","token":"token-alice","organization_id":"tos","device_id":"group-id-device","testing_key":null,"testing_generation":null}}).to_string().into())).await?;
    }
    let command = json!({"type":"new_message","data":{"actor_id":"alice","org_id":"tos","conversation_id":id,"idempotency_key":format!("group-id-socket-{shared}"),"message":"Address roundtrip","metadata":{"conversation_id":"leave-this-alone"}}});
    if shared {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let frame = socket.next().await.ok_or("shared closed before ready")??;
                let value: Value = serde_json::from_str(frame.to_text()?)?;
                if value["data"]["frame"]["type"] == "ready" {
                    break;
                }
            }
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
        })
        .await??;
    }
    let command = if shared {
        json!({"type":"channel","data":{"subscription_id":"group","frame":command}})
    } else {
        command
    };
    socket
        .send(SocketMessage::Text(command.to_string().into()))
        .await?;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let frame = socket
                .next()
                .await
                .ok_or("socket closed before acceptance")??;
            let value: Value = serde_json::from_str(frame.to_text()?)?;
            let value = if shared {
                &value["data"]["frame"]
            } else {
                &value
            };
            if value["type"] == "error" {
                return Err(format!("socket rejected: {value}").into());
            }
            if value["type"] == "message_accepted" {
                assert_eq!(value["data"]["conversation_id"], id);
                assert_eq!(
                    value["data"]["metadata"]["conversation_id"],
                    "leave-this-alone"
                );
                let _: silicon_dm_client::models::ServerFrame =
                    serde_json::from_value(value.clone())?;
                break;
            }
        }
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
    })
    .await??;
    socket.close(None).await?;
    Ok(())
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one sandbox exercises durable lifecycle ordering and replay"
)]
async fn honeycomb_lifecycle_is_authenticated_fenced_and_retry_safe() -> Result {
    let container = Postgres::default().with_tag("16-alpine").start().await?;
    let url = format!(
        "postgres://postgres:postgres@{}:{}/postgres",
        container.get_host().await?,
        container.get_host_port_ipv4(5432).await?
    );
    let iam = MockServer::start().await;
    let honeycomb = MockServer::start().await;
    let mut config = settings(url.clone(), &iam.uri())?;
    let admin = PostgresStore::connect(&config.database).await?;
    sqlx::query("CREATE DATABASE sandbox")
        .execute(admin.pool())
        .await?;
    let mut testing_db = config.database.clone();
    testing_db.url =
        SecretString::from(format!("{}/sandbox", url.rsplit_once('/').ok_or("url")?.0));
    let token = "honeycomb-only-service-token-32-characters";
    config.testing = Some(TestingSettings {
        database: testing_db.clone(),
        encryption_key: SecretString::from(STANDARD.encode([7u8; 32])),
        honeycomb_service_token: Some(SecretString::from(token)),
        honeycomb_base_url: Some(honeycomb.uri().parse()?),
    });
    let app = state(config).await?;
    let registry = app.testing.as_ref().ok_or("registry")?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let router = silicon_dm::api::build_router(app.clone());
    let server = tokio::spawn(async move { axum::serve(listener, router).await });
    let id = Uuid::new_v4();
    let secret = format!("ask_{}", "h".repeat(43));
    mock_context(&iam, id, &secret, 1, None).await;
    assert!(
        registry.state_for_key(&app, &secret).await.is_err(),
        "runtime discovery cannot bypass preparation"
    );
    let mut op = json!({"operation_id":Uuid::new_v4(),"environment_id":id,"org_id":"tos","app_id":"tos>dm","environment_revision":1,"generation":1,"key_version":1,"action":"prepare","testing_key":"K".repeat(32),"name":"Shared DM","description":"coordinated sandbox","snapshot":{},"reason":"requested","retired_apps":[]});
    let http = reqwest::Client::new();
    let endpoint = |body: &Value| {
        format!(
            "{base}/internal/honeycomb/organizations/tos/testing-environments/{id}/operations/{}",
            body["operation_id"].as_str().unwrap_or_default()
        )
    };
    assert_eq!(
        http.put(endpoint(&op))
            .bearer_auth(&secret)
            .json(&op)
            .send()
            .await?
            .status(),
        401
    );
    let prepared = http
        .put(endpoint(&op))
        .bearer_auth(token)
        .json(&op)
        .send()
        .await?;
    assert!(prepared.status().is_success(), "{}", prepared.text().await?);
    let receipt: Value = http
        .get(endpoint(&op))
        .bearer_auth(token)
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(receipt["state"], "completed"); // Internal receipts must not have a DM envelope.
    assert!(!receipt.to_string().contains(&"K".repeat(32)));
    let mut changed = op.clone();
    changed["name"] = json!("changed replay");
    assert_eq!(
        http.put(endpoint(&op))
            .bearer_auth(token)
            .json(&changed)
            .send()
            .await?
            .status(),
        409
    );
    let selected = registry.state_for_key(&app, &secret).await?;
    let old_generation = selected.testing_generation.ok_or("generation")?;
    let actor = ActorRef {
        id: "alice".parse()?,
        actor_type: ActorType::Carbon,
    };
    selected
        .store
        .refresh_directory(&"tos".parse()?, std::slice::from_ref(&actor))
        .await?;
    // A real in-flight request prevents clean from overtaking its write.
    let fence = registry.request_fence(id, old_generation).await?;
    op["operation_id"] = json!(Uuid::new_v4());
    op["action"] = json!("clean");
    op["environment_revision"] = json!(2);
    op["generation"] = json!(2);
    let pending = http.put(endpoint(&op)).bearer_auth(token).json(&op).send();
    tokio::pin!(pending);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut pending)
            .await
            .is_err()
    );
    drop(fence);
    let response = pending.await?;
    assert!(response.status().is_success(), "{}", response.text().await?);
    assert!(registry.ensure_active(id, old_generation).await.is_err());
    // Activity survives a failed report and is scoped to the current generation.
    let generation = registry
        .state_for_key(&app, &secret)
        .await?
        .testing_generation
        .ok_or("generation")?;
    registry.touch(id, generation).await?;
    registry.report_honeycomb_activity().await?; // 404: retain for retry.
    Mock::given(path(format!(
        "/api/v1/environments/{id}/apps/tos%3Edm/activity"
    )))
    .and(header("x-testing-environment-key", "K".repeat(32)))
    .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
    .expect(1)
    .mount(&honeycomb)
    .await;
    registry.report_honeycomb_activity().await?;
    registry.report_honeycomb_activity().await?; // acknowledged activity is not repeated.
    let clean = op.clone();
    let selected = registry.state_for_key(&app, &secret).await?;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM actor_snapshots")
        .fetch_one(selected.store.pool())
        .await?;
    assert_eq!(count, 0);
    selected
        .store
        .refresh_directory(&"tos".parse()?, std::slice::from_ref(&actor))
        .await?;
    assert!(
        http.put(endpoint(&clean))
            .bearer_auth(token)
            .json(&clean)
            .send()
            .await?
            .status()
            .is_success()
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM actor_snapshots")
        .fetch_one(selected.store.pool())
        .await?;
    assert_eq!(
        count, 1,
        "completed clean retry must not erase subsequent data"
    );
    let response = http
        .post(format!("{base}/api/v1/conversations"))
        .header("X-Testing-Environment-Key", &secret)
        .json(&json!({}))
        .send()
        .await?;
    assert_eq!(
        response.status(),
        409,
        "unfenced mutations must fail before processing"
    );
    let response = http
        .post(format!("{base}/api/v1/conversations"))
        .header("X-Testing-Environment-Key", &secret)
        .header("X-Testing-Environment-Generation", old_generation)
        .json(&json!({}))
        .send()
        .await?;
    assert_eq!(response.status(), 409);
    op["operation_id"] = json!(Uuid::new_v4());
    op["action"] = json!("disable");
    op["environment_revision"] = json!(3);
    assert!(
        http.put(endpoint(&op))
            .bearer_auth(token)
            .json(&op)
            .send()
            .await?
            .status()
            .is_success()
    );
    assert!(registry.state_for_key(&app, &secret).await.is_err());
    // No runtime IAM sessions are available while lifecycle control remains usable.
    iam.reset().await;
    op["operation_id"] = json!(Uuid::new_v4());
    op["action"] = json!("clean");
    op["environment_revision"] = json!(4);
    op["generation"] = json!(3);
    assert!(
        http.put(endpoint(&op))
            .bearer_auth(token)
            .json(&op)
            .send()
            .await?
            .status()
            .is_success()
    );
    mock_context(&iam, id, &secret, 3, Some("2026-09-16T00:00:00Z")).await;
    assert!(
        registry.state_for_key(&app, &secret).await.is_err(),
        "clean must preserve disabled state"
    );
    op["operation_id"] = json!(Uuid::new_v4());
    op["action"] = json!("restore");
    op["environment_revision"] = json!(5);
    assert!(
        http.put(endpoint(&op))
            .bearer_auth(token)
            .json(&op)
            .send()
            .await?
            .status()
            .is_success()
    );
    let restored = registry.state_for_key(&app, &secret).await?;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM actor_snapshots")
        .fetch_one(restored.store.pool())
        .await?;
    assert_eq!(count, 0, "restore must not undo a clean");
    // Fresh IAM readiness gates already-open request generations too.
    iam.reset().await;
    assert!(
        registry
            .request_fence(id, restored.testing_generation.ok_or("generation")?)
            .await
            .is_err()
    );
    op["operation_id"] = json!(Uuid::new_v4());
    op["action"] = json!("rotate-key");
    op["environment_revision"] = json!(6);
    op["key_version"] = json!(2);
    op["testing_key"] = json!("N".repeat(32));
    let test_admin = PostgresStore::connect(&testing_db).await?;
    let schema = format!("dm_test_{}", id.simple());
    let checksum: Vec<u8> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT checksum FROM {schema}.__dm_migrations WHERE version=1"
    )))
    .fetch_one(test_admin.pool())
    .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE {schema}.__dm_migrations SET checksum=''::bytea WHERE version=1"
    )))
    .execute(test_admin.pool())
    .await?;
    let failed = http
        .put(endpoint(&op))
        .bearer_auth(token)
        .json(&op)
        .send()
        .await?;
    assert_eq!(failed.status(), 500);
    let receipt: Value = http
        .get(endpoint(&op))
        .bearer_auth(token)
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(receipt["state"], "failed");
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE {schema}.__dm_migrations SET checksum=$1 WHERE version=1"
    )))
    .bind(checksum)
    .execute(test_admin.pool())
    .await?;
    assert!(
        http.put(endpoint(&op))
            .bearer_auth(token)
            .json(&op)
            .send()
            .await?
            .status()
            .is_success()
    );
    op["operation_id"] = json!(Uuid::new_v4());
    op["action"] = json!("purge");
    op["environment_revision"] = json!(7);
    assert!(
        http.put(endpoint(&op))
            .bearer_auth(token)
            .json(&op)
            .send()
            .await?
            .status()
            .is_success()
    );
    assert!(
        http.put(endpoint(&op))
            .bearer_auth(token)
            .json(&op)
            .send()
            .await?
            .status()
            .is_success()
    );
    mock_context(&iam, id, &secret, 3, None).await;
    assert!(
        registry.state_for_key(&app, &secret).await.is_err(),
        "purged tombstone blocks rediscovery"
    );
    let test_admin = PostgresStore::connect(&testing_db).await?;
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_namespace WHERE nspname=$1)")
            .bind(format!("dm_test_{}", id.simple()))
            .fetch_one(test_admin.pool())
            .await?;
    assert!(!exists);
    assert_eq!(
        http.put(endpoint(&clean))
            .bearer_auth(token)
            .json(&clean)
            .send()
            .await?
            .status(),
        200,
        "old receipt is replayable without an effect"
    );
    server.abort();
    Ok(())
}
