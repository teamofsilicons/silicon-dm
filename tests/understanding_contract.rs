//! Integration coverage for IAM discovery, HTTP contracts and Ting delivery ownership.

mod support;
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD};
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
        if !["alice", "bob", "cos:tos"].contains(&id) || organization_id.as_str() != "tos" {
            return Err(AppError::Unauthorized);
        }
        Ok(AuthContext {
            actor: ActorRef {
                id: id.parse().map_err(|_| AppError::Unauthorized)?,
                actor_type: if id == "cos:tos" {
                    ActorType::Silicon
                } else {
                    ActorType::Carbon
                },
            },

            session_id: None,
            organization_id: organization_id.clone(),
            org_role: (id == "alice").then(|| "org_admin".into()),
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
        if ids.iter().any(|id| id.as_str() == "blocked") {
            return Err(AppError::Forbidden);
        }
        Ok(ids
            .iter()
            .map(|id| ActorRef {
                id: id.clone(),
                actor_type: if id.as_str() == "cos:tos" {
                    ActorType::Silicon
                } else {
                    ActorType::Carbon
                },
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
        ting: TingSettings {
            base_url: "http://127.0.0.1:9087".parse()?,
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
        testing_runtime_revision: None,
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
    reason = "one fixture covers transport retirement, HTTP actor scope and contract lifecycle"
)]
async fn retired_sockets_leave_http_actor_scope_and_contract_lifecycle_intact() -> Result {
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
    let http = reqwest::Client::new();
    for path in ["ws", "ws/shared"] {
        let response = http
            .get(format!("{base}/api/v1/{path}"))
            .bearer_auth("token-alice")
            .header("X-Org-ID", "tos")
            .send()
            .await?;
        assert_eq!(response.status(), 410);
        let body: Value = response.json().await?;
        assert_eq!(body["data"]["error"]["code"], "delivery_moved_to_ting");
    }
    assert!(client.prewarm_shared().await.is_err());
    assert!(
        client
            .connect(&["alice".into()], "retired-client")
            .await
            .is_err()
    );
    for actor in ["alice", "bob"] {
        let scoped =
            silicon_dm_client::Client::new(&base)?.with_auth(format!("token-{actor}"), "tos");
        let page = scoped.sync_reset().await?;
        assert!(page.events.is_empty());
        let other = if actor == "alice" { "bob" } else { "alice" };
        let wrong =
            silicon_dm_client::Client::new(&base)?.with_auth(format!("token-{other}"), "tos");
        assert!(
            wrong
                .sync(&silicon_dm_client::models::SyncRequest {
                    cursor: Some(page.cursor),
                    ..Default::default()
                })
                .await
                .is_err(),
            "sync cursor must remain actor-bound"
        );
    }
    assert_eq!(
        http.get(format!("{base}/api/v1/sync"))
            .header("X-Org-ID", "tos")
            .send()
            .await?
            .status(),
        401
    );
    daemon_reports_ting_delivery_ownership(&base).await?;
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

async fn daemon_reports_ting_delivery_ownership(base: &str) -> Result {
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
            config.profiles.insert(session_key(name,None),Profile{name:name.into(),base_url:url,tokens,webhook_url:None,device_id:format!("runtime-{name}"),expires_at:now()+3600,refresh_started_at:None,testing_environment_id:None,enabled:true});
        }
        Ok(())
    })?;
    let relay = runtime.client()?;
    let task = tokio::spawn(async move { runtime.run().await });
    tokio::time::timeout(Duration::from_secs(12), async {
        loop {
            if let Ok(status) = relay.status().await {
                let profiles = status["profiles"].as_object().ok_or("profiles")?;
                if profiles.len() == 2 && profiles.values().all(|p| p["state"] == "outgoing_only") {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
    })
    .await??;
    let status = relay.status().await?;
    assert_eq!(
        status["incoming_delivery"]["code"],
        "delivery_moved_to_ting"
    );
    assert_eq!(status["incoming_delivery"]["forwarding"], false);
    relay.stop().await?;
    tokio::time::timeout(Duration::from_secs(3), task).await???;
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
    let stale = owner
        .put_draft(&group.id, &draft.content, 0)
        .await
        .err()
        .ok_or("expected operation to fail")?;
    let silicon_dm_client::Error::Api {
        status, code, body, ..
    } = stale
    else {
        return Err("expected a structured draft conflict".into());
    };
    assert_eq!(status, 409);
    assert_eq!(code, "draft_conflict");
    assert_eq!(body["version"], draft.version);
    assert_eq!(body["conversation_id"], group.id);
    assert_eq!(owner.draft(&group.id).await?.version, draft.version);
    owner.delete_draft(&group.id).await?;
    group_address_http_roundtrip(&owner, &group.id, "group-id-http-first").await?;
    group_address_http_roundtrip(&owner, &group.id, "group-id-http-second").await?;
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

async fn group_address_http_roundtrip(
    client: &silicon_dm_client::Client,
    id: &str,
    key: &str,
) -> Result {
    let request = serde_json::from_value(json!({"message":"Address roundtrip",
        "metadata":{"conversation_id":"leave-this-alone"}}))?;
    let message = client.send_message(id, &request, key).await?;
    assert_eq!(message.conversation_id, id);
    assert_eq!(client.send_message(id, &request, key).await?.id, message.id);
    let hydrated = client.message(id, &message.id).await?;
    assert_eq!(hydrated.conversation_id, id);
    assert_eq!(hydrated.content.text.as_deref(), Some("Address roundtrip"));
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

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "exercise recipient addressing and fixed message snapshots across their full lifecycle"
)]
async fn recipient_addressed_messages_use_stable_local_codes_and_server_owned_replies() -> Result {
    use silicon_dm_client::{Client, MessageCreate, ReceiptStatus};
    let container = Postgres::default().with_tag("16-alpine").start().await?;
    let url = format!(
        "postgres://postgres:postgres@{}:{}/postgres",
        container.get_host().await?,
        container.get_host_port_ipv4(5432).await?
    );
    let app = state(settings(url, "http://localhost:9999")?).await?;
    let pool = app.store.pool().clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let server =
        tokio::spawn(
            async move { axum::serve(listener, silicon_dm::api::build_router(app)).await },
        );
    let alice = Client::new(&base)?.with_auth("token-alice", "tos");
    let bob = Client::new(&base)?.with_auth("token-bob", "tos");
    let text: MessageCreate = serde_json::from_value(json!({"message":"hey"}))?;
    let first = alice.send_message("bob", &text, "first-direct").await?;
    assert_eq!(first.id, "000");
    assert_eq!(first.conversation_id, "alice::bob");
    assert_eq!(first.content.recipient_id.as_deref(), Some("bob"));
    for (client, recipient) in [(&alice, "alice"), (&bob, "bob")] {
        let page = client
            .sync(&silicon_dm_client::models::SyncRequest::default())
            .await?;
        let event = page
            .events
            .iter()
            .find(|event| event.message_id == "000")
            .ok_or("creation reference missing")?;
        assert!(matches!(
            event.kind,
            silicon_dm_client::models::SyncEventKind::Message
        ));
        assert_eq!(event.conversation_id, "alice::bob");
        let hydrated = client
            .message(&event.conversation_id, &event.message_id)
            .await?;
        assert_eq!(hydrated.id, "000");
        assert_eq!(hydrated.content.recipient_id.as_deref(), Some("bob"));
        let handoff: (String, String) =
            sqlx::query_as("SELECT event,target_id FROM ting_handoffs WHERE delivery_id=$1")
                .bind(event.event_id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(handoff, ("message.created".into(), recipient.into()));
    }

    assert_eq!(
        alice.send_message("bob", &text, "first-direct").await?.id,
        first.id
    );
    let reply: MessageCreate = serde_json::from_value(
        json!({"message":"what's up","reply":{"message-id":"000","content":{"message":"forged"}}}),
    )?;
    let second = bob.send_message("alice", &reply, "reply-direct").await?;
    assert_eq!(second.id, "001");
    assert_eq!(second.conversation_id, first.conversation_id);
    let quote = second.reply.as_ref().ok_or("missing quote")?;
    assert_eq!(quote.sender.id, "alice");
    assert_eq!(
        quote.content.as_ref().and_then(|c| c.message.as_deref()),
        Some("hey")
    );
    let attachments: MessageCreate = serde_json::from_value(
        json!({"attachments":["https://files.example/voice.ogg","https://files.example/note.pdf"],"voice_transcript":"hello"}),
    )?;
    let third = alice
        .send_message("bob", &attachments, "attachments-direct")
        .await?;
    assert_eq!(third.id, "002");
    assert_eq!(third.content.text.as_deref(), Some(""));
    assert_eq!(third.content.attachments.len(), 2);
    assert_eq!(third.content.voice_transcript.as_deref(), Some("hello"));
    assert!(
        alice
            .send_message("bob", &MessageCreate::default(), "empty-direct")
            .await
            .is_err()
    );
    assert!(alice.message("bob", "../000").await.is_err());
    assert!(
        alice
            .send_message("blocked", &text, "denied-direct")
            .await
            .is_err()
    );
    let legacy: (Uuid, Uuid) =
        sqlx::query_as("SELECT conversation_id,id FROM messages WHERE text_content='hey'")
            .fetch_one(&pool)
            .await?;
    assert_eq!(alice.message(legacy.0, legacy.1).await?.id, "000");
    let edited = alice
        .edit_message(
            "bob",
            "000",
            &serde_json::from_value(json!({"message":"heyy"}))?,
            "edit-direct",
        )
        .await?;
    assert_eq!(edited.id, "000");
    assert_eq!(edited.history.len(), 1);
    assert_eq!(edited.history[0]["message"], "hey");
    assert!(edited.updated_at.is_some());
    let before_receipt = alice.sync_reset().await?;
    let read = bob
        .record_receipt("alice", "000", ReceiptStatus::Read, "test-device")
        .await?;
    assert!(read.read_at.is_some());
    let updates = alice
        .sync(&silicon_dm_client::models::SyncRequest {
            cursor: Some(before_receipt.cursor),
            ..Default::default()
        })
        .await?;
    let reference = updates
        .events
        .iter()
        .find(|event| event.message_id == "000")
        .ok_or("receipt reference missing")?;
    assert!(matches!(
        reference.kind,
        silicon_dm_client::models::SyncEventKind::MessageStatus
    ));
    let event: String = sqlx::query_scalar("SELECT event FROM ting_handoffs WHERE delivery_id=$1")
        .bind(reference.event_id)
        .fetch_one(&pool)
        .await?;
    assert_eq!(event, "message.read");
    let snapshot = alice
        .message(&reference.conversation_id, &reference.message_id)
        .await?;
    assert_eq!(snapshot.id, "000");
    assert_eq!(snapshot.content.recipient_id.as_deref(), Some("bob"));
    assert_eq!(snapshot.content.text.as_deref(), Some("heyy"));
    assert!(snapshot.read_at.is_some());
    let serialized = serde_json::to_value(&snapshot)?;
    assert!(serialized.get("delivery_id").is_none());
    assert!(serialized.get("sequence").is_none());
    let deleted = alice.delete_message("bob", "000", "delete-direct").await?;
    let tombstone = serde_json::to_value(&deleted)?;
    assert_eq!(tombstone["message-id"], "000");
    assert!(tombstone["message"].is_null());
    assert_eq!(tombstone["attachments"], json!([]));
    assert!(
        alice
            .message("bob", "001")
            .await?
            .reply
            .ok_or("quote")?
            .content
            .is_none()
    );
    let mut tx = pool.begin().await?;
    sqlx::query("ALTER TABLE conversations DISABLE TRIGGER conversations_enforce_update_policy")
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE conversations SET next_message_sequence=46656 WHERE id=$1")
        .bind(legacy.0)
        .execute(&mut *tx)
        .await?;
    sqlx::query("ALTER TABLE conversations ENABLE TRIGGER conversations_enforce_update_policy")
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    let last = alice.send_message("bob", &text, "last-three-digit").await?;
    let next = alice.send_message("bob", &text, "first-four-digit").await?;
    assert_eq!(last.id, "zzz");
    assert_eq!(next.id, "1000");
    let (left, right) = tokio::join!(
        alice.send_message("bob", &text, "concurrent-left"),
        bob.send_message("alice", &text, "concurrent-right")
    );
    assert_ne!(left?.id, right?.id);
    let other = alice
        .send_message("cos:tos", &text, "other-conversation")
        .await?;
    assert_eq!(other.conversation_id, "alice::cos:tos");
    assert_eq!(other.id, "000");
    assert!(bob.message(&other.conversation_id, "000").await.is_err());
    let cross_reply: MessageCreate =
        serde_json::from_value(json!({"message":"invalid reply","reply":{"message-id":legacy.1}}))?;
    assert!(
        alice
            .send_message("cos:tos", &cross_reply, "cross-chat-reply")
            .await
            .is_err()
    );
    assert_eq!(
        alice.message("cos:tos", "000").await?.conversation_id,
        other.conversation_id
    );
    let http = reqwest::Client::new();
    let raw: Value = http
        .get(format!(
            "{base}/api/v1/conversations/alice::bob/messages/002"
        ))
        .bearer_auth("token-alice")
        .header("X-Org-ID", "tos")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let data = raw["data"].as_object().ok_or("message data")?;
    let mut expected = vec![
        "message-id",
        "conversation_id",
        "recipient_id",
        "sender",
        "message",
        "attachments",
        "voice_transcript",
        "reply",
        "bundle",
        "history",
        "created_at",
        "updated_at",
        "deleted_at",
        "delivered_at",
        "read_at",
    ];
    expected.sort_unstable();
    assert_eq!(
        data.keys().map(String::as_str).collect::<Vec<_>>(),
        expected
    );
    assert_eq!(
        data["attachments"],
        json!([
            "https://files.example/voice.ogg",
            "https://files.example/note.pdf"
        ])
    );
    let response = http
        .post(format!("{base}/api/v1/messages"))
        .bearer_auth("token-bob")
        .header("X-Org-ID", "tos")
        .header("Idempotency-Key", "body-recipient")
        .json(
            &json!({"type":"message.create","data":{"recipient_id":"alice","message":"from body"}}),
        )
        .send()
        .await?
        .error_for_status()?;
    let direct: Value = response.json().await?;
    assert_eq!(direct["data"]["conversation_id"], "alice::bob");
    server.abort();
    Ok(())
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one transaction fixture exercises the full message and bundle contract"
)]
async fn bundle_commands_and_history_are_atomic_and_retry_safe() -> Result {
    use silicon_dm_client::{Activity, BundleCreate, Client, Error, MessageCreate};

    let database = support::TestDatabase::start().await?;
    let app = state(settings(database.url.clone(), "http://localhost:9999")?).await?;
    let pool = app.store.pool().clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let server =
        tokio::spawn(
            async move { axum::serve(listener, silicon_dm::api::build_router(app)).await },
        );
    let client = Client::new(&base)?.with_auth("token-cos:tos", "tos");
    let content: MessageCreate = serde_json::from_value(json!({"message":"original"}))?;
    let first = client
        .send_message("alice", &content, "history-first")
        .await?;
    assert!(first.history.is_empty());
    let changed: MessageCreate = serde_json::from_value(json!({"message":"edited"}))?;
    let edit = client
        .edit_message("alice", &first.id, &changed, "history-edit")
        .await?;
    assert_eq!(edit.history.len(), 1);
    assert_eq!(edit.history[0]["message"], "original");
    assert_eq!(
        client
            .edit_message("alice", &first.id, &changed, "history-edit")
            .await?
            .history
            .len(),
        1
    );
    let again: MessageCreate = serde_json::from_value(json!({"message":"second edit"}))?;
    assert_eq!(
        client
            .edit_message("alice", &first.id, &again, "history-edit-two")
            .await?
            .history[1]["message"],
        "edited"
    );
    let bundle = BundleCreate {
        message_ids: vec![first.id.clone()],
        display_message: serde_json::from_value(json!({"message":"summary"}))?,
    };
    let accepted = client
        .create_bundle(&first.conversation_id, &bundle, "bundle-http-001")
        .await?;
    assert_eq!(accepted.id, "001");
    let accepted_json = serde_json::to_value(&accepted)?;
    assert_eq!(
        accepted_json["display_message"]["bundle"],
        json!({"id":"001","role":"display"})
    );
    assert!(accepted_json["display_message"].get("version").is_none());
    let retried = client
        .create_bundle(&first.conversation_id, &bundle, "bundle-http-001")
        .await?;
    assert_eq!(retried.id, accepted.id);
    assert_eq!(retried.display_message.id, accepted.display_message.id);
    let expanded = client.bundle(&first.conversation_id, "001").await?;
    assert_eq!(
        expanded.original_messages[0]
            .bundle
            .as_ref()
            .ok_or("bundle")?
            .role,
        "member"
    );
    assert_eq!(expanded.original_messages[0].history.len(), 2);
    let mut conflict = bundle.clone();
    conflict.display_message = serde_json::from_value(json!({"message":"different"}))?;
    assert!(matches!(
        client
            .create_bundle(&first.conversation_id, &conflict, "bundle-http-001")
            .await,
        Err(Error::Api { status: 409, code, .. }) if code == "conflict"
    ));
    assert!(matches!(
        client
            .create_bundle(&first.conversation_id, &bundle, "bundle-already-member")
            .await,
        Err(Error::Api { status: 409, code, .. }) if code == "conflict"
    ));
    conflict.message_ids = vec!["zzz".into()];
    let missing = client
        .create_bundle(&first.conversation_id, &conflict, "bundle-missing-002")
        .await;
    assert!(
        matches!(&missing, Err(Error::Api { status: 422, code, .. }) if code == "validation_error"),
        "missing bundle member must be rejected: {missing:?}"
    );
    let recipient = Client::new(&base)?.with_auth("token-alice", "tos");
    assert_eq!(
        recipient.bundle(&first.conversation_id, "001").await?.id,
        accepted.id
    );
    assert!(matches!(
        recipient
            .create_bundle(&first.conversation_id, &bundle, "bundle-carbon-denied")
            .await,
        Err(Error::Api { status: 403, .. })
    ));
    let outsider = Client::new(&base)?.with_auth("token-bob", "tos");
    let denied = outsider.bundle(&first.conversation_id, "001").await;
    assert!(
        matches!(&denied, Err(Error::Api { status: 404, .. })),
        "outsider bundle access must be rejected: {denied:?}"
    );
    let counts: (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM message_bundles),(SELECT count(*) FROM messages)",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(counts, (1, 2));
    let lease = client
        .renew_presence("bundle-test", Some(Activity::Typing))
        .await?;
    assert_eq!(lease.presence.actor_id, "cos:tos");
    assert_eq!(lease.presence.availability, "online");
    assert!(matches!(lease.presence.activity, Some(Activity::Typing)));
    let presence = client.presence("cos:tos").await?;
    assert_eq!(presence.actor_id, "cos:tos");
    assert!(matches!(presence.activity, Some(Activity::Typing)));
    client.close_presence("bundle-test").await?;
    assert_eq!(client.presence("cos:tos").await?.availability, "offline");
    let deleted = client
        .delete_message("alice", &first.id, "history-delete")
        .await?;
    assert!(deleted.deleted_at.is_some());
    assert!(deleted.history.is_empty());
    let stored: Value = sqlx::query_scalar(
        "SELECT history FROM message_history WHERE message_id=(SELECT id FROM messages WHERE sequence=1)",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(stored.as_array().ok_or("history")?.len(), 2);
    assert!(
        client
            .edit_message("alice", &first.id, &content, "after-delete")
            .await
            .is_err()
    );
    let page = client
        .conversations(&silicon_dm_client::PageRequest::default())
        .await?;
    assert!(matches!(
        page.items[0].last_message_status,
        Some(silicon_dm_client::MessageStatus::Sent)
    ));
    server.abort();
    Ok(())
}

struct UnusedTingPublisher;
#[async_trait]
impl silicon_dm::infrastructure::ting::TingPublisher for UnusedTingPublisher {
    async fn publish(
        &self,
        _: &silicon_dm::infrastructure::postgres::TingDeliveryClaim,
    ) -> std::result::Result<
        silicon_dm::infrastructure::ting::TingAcceptance,
        silicon_dm::infrastructure::ting::TingFailure,
    > {
        panic!("stale runtime must be rejected before any Ting publication")
    }
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one retained sandbox covers generation, epoch and recovery across lifecycle transitions"
)]
async fn managed_generation_is_independent_of_credential_and_lifecycle_revisions() -> Result {
    let fixture = support::TestDatabase::start().await?;
    let iam = MockServer::start().await;
    let mut config = settings(fixture.url.clone(), &iam.uri())?;
    let admin = PostgresStore::connect(&config.database).await?;
    sqlx::query("CREATE DATABASE managed_generation_sandbox")
        .execute(admin.pool())
        .await?;
    let mut testing_db = config.database.clone();
    testing_db.url = SecretString::from(format!(
        "{}/managed_generation_sandbox",
        fixture.url.rsplit_once('/').ok_or("url")?.0
    ));
    let testing = TestingSettings {
        database: testing_db,
        encryption_key: SecretString::from(STANDARD.encode([9u8; 32])),
        honeycomb_service_token: Some(SecretString::from(
            "coordinator-service-token-32-characters",
        )),
        honeycomb_base_url: None,
    };
    config.testing = Some(testing.clone());
    let app = state(config).await?;
    let registry = app.testing.as_ref().ok_or("registry")?;
    let peer = Arc::new(
        silicon_dm::testing::TestingRegistry::new(app.store.clone(), &testing, &app.settings.iam)
            .await?,
    );
    let id = Uuid::new_v4();
    let old_secret = format!("ask_{}", "a".repeat(43));
    let new_secret = format!("ask_{}", "b".repeat(43));
    mock_context(&iam, id, &old_secret, 1, None).await;
    let mut op: silicon_dm::testing::honeycomb::Operation = serde_json::from_value(json!({
        "operation_id":Uuid::new_v4(),"environment_id":id,"org_id":"tos","app_id":"tos>dm",
        "environment_revision":1,"generation":1,"key_version":1,"action":"prepare",
        "testing_key":"K".repeat(32),"snapshot":{},"reason":"fixture","retired_apps":[]
    }))?;
    registry.honeycomb_operation(&op, "tos>dm").await?;
    let original = peer.state_for_key(&app, &old_secret).await?;
    assert_eq!(original.testing_generation, Some(1));
    let old_revision = original.testing_runtime_revision.ok_or("epoch")?;
    let stale_worker = silicon_dm::worker::TingDeliveryWorker::new(
        original.store.clone(),
        Arc::new(UnusedTingPublisher),
        original.ting_delivery_context(),
        "stale-peer-worker".into(),
        app.settings.worker.clone(),
    )
    .with_testing_registry(peer.clone())
    .with_runtime_revision(old_revision);
    iam.reset().await;
    mock_context(&iam, id, &new_secret, 2, None).await;
    let rotated = registry.state_for_key(&app, &new_secret).await?;
    assert_eq!(
        rotated.testing_generation,
        Some(1),
        "app credential rotation cannot clean the shared generation"
    );
    let mut revision = rotated.testing_runtime_revision.ok_or("epoch")?;
    assert!(revision > old_revision);
    assert!(matches!(
        peer.request_runtime_fence(id, 1, old_revision).await,
        Err(AppError::Unauthorized)
    ));
    assert!(matches!(
        stale_worker.process_once().await,
        Err(AppError::Unauthorized)
    ));
    let refreshed = peer.state_for_key(&app, &new_secret).await?;
    assert_eq!(refreshed.testing_generation, Some(1));
    assert_eq!(refreshed.testing_runtime_revision, Some(revision));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let origin = format!("http://{}", listener.local_addr()?);
    let router = silicon_dm::api::build_router(app.clone());
    let server = tokio::spawn(async move { axum::serve(listener, router).await });
    let response = reqwest::Client::new()
        .get(format!("{origin}/api/v1/iam"))
        .header("X-Testing-Environment-Key", &new_secret)
        .header("X-Testing-Environment-Generation", "1")
        .send()
        .await?;
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await?;
    assert_eq!(body["data"]["testing_generation"], 1);
    assert!(
        body["data"]["testing_environment"]
            .get("runtime_revision")
            .is_none()
    );
    for action in ["refresh-import", "rotate-key", "disable", "restore"] {
        op.operation_id = Uuid::new_v4();
        op.environment_revision += 1;
        op.action = action.into();
        if action == "rotate-key" {
            op.key_version += 1;
            op.testing_key = "N".repeat(32);
        }
        registry.honeycomb_operation(&op, "tos>dm").await?;
        assert!(matches!(
            registry.request_runtime_fence(id, 1, revision).await,
            Err(AppError::Unauthorized)
        ));
        if action == "disable" {
            assert!(registry.state_for_key(&app, &new_secret).await.is_err());
        } else {
            let current = registry.state_for_key(&app, &new_secret).await?;
            assert_eq!(
                current.testing_generation,
                Some(1),
                "{action} retains generation"
            );
            assert!(current.testing_runtime_revision.ok_or("epoch")? > revision);
            revision = current.testing_runtime_revision.ok_or("epoch")?;
        }
    }
    let actor = ActorRef {
        actor_type: ActorType::Carbon,
        id: "alice".parse()?,
    };
    let retained = registry.state_for_key(&app, &new_secret).await?;
    retained
        .store
        .refresh_directory(&"tos".parse()?, &[actor])
        .await?;
    // Model the incorrect metadata left by backend0.10.0; supported discovery repairs it.
    sqlx::query("UPDATE dm.testing_environments SET version=9 WHERE environment_id=$1")
        .bind(id)
        .execute(app.store.pool())
        .await?;
    let repaired = registry.state_for_key(&app, &new_secret).await?;
    assert_eq!(repaired.testing_generation, Some(1));
    assert!(repaired.testing_runtime_revision.ok_or("epoch")? > revision);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM actor_snapshots")
        .fetch_one(repaired.store.pool())
        .await?;
    assert_eq!(count, 1, "metadata repair preserves sandbox data");
    op.operation_id = Uuid::new_v4();
    op.environment_revision += 1;
    op.action = "clean".into();
    op.generation = 2;
    registry.honeycomb_operation(&op, "tos>dm").await?;
    assert!(registry.ensure_active(id, 1).await.is_err());
    let cleaned = registry.state_for_key(&app, &new_secret).await?;
    assert_eq!(cleaned.testing_generation, Some(2));
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM actor_snapshots")
        .fetch_one(cleaned.store.pool())
        .await?;
    assert_eq!(count, 0);
    server.abort();
    sandbox_worker_retries_shared_readiness(&app, registry, &iam, &new_secret, op).await?;
    Ok(())
}

#[derive(Default)]
struct RecoveredTingPublisher(std::sync::Mutex<Vec<String>>);

#[async_trait]
impl silicon_dm::infrastructure::ting::TingPublisher for RecoveredTingPublisher {
    async fn publish(
        &self,
        claim: &silicon_dm::infrastructure::postgres::TingDeliveryClaim,
    ) -> std::result::Result<
        silicon_dm::infrastructure::ting::TingAcceptance,
        silicon_dm::infrastructure::ting::TingFailure,
    > {
        self.0
            .lock()
            .map_err(|_| silicon_dm::infrastructure::ting::TingFailure::Protocol)?
            .push(claim.request_body.clone());
        Ok(silicon_dm::infrastructure::ting::TingAcceptance {
            id: format!("recovered-{}", claim.delivery_id),
            created_at: time::OffsetDateTime::now_utc(),
            silent: false,
        })
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "follow the same sandbox worker through idle, outages, recovery and revocation"
)]
async fn sandbox_worker_retries_shared_readiness(
    app: &AppState,
    registry: &Arc<silicon_dm::testing::TestingRegistry>,
    iam: &MockServer,
    secret: &str,
    mut op: silicon_dm::testing::honeycomb::Operation,
) -> Result {
    use silicon_dm::{
        application::commands::{CreateConversationCommand, SendMessageCommand},
        domain::MessageCreate,
        infrastructure::postgres::TingDeliveryContext,
        worker::TingDeliveryWorker,
    };
    // Stop the automatically constructed worker through a supported lifecycle operation.
    // The retained test credentials remain usable by this independently opened runtime.
    op.operation_id = Uuid::new_v4();
    op.environment_revision += 1;
    op.action = "refresh-import".into();
    registry.honeycomb_operation(&op, "tos>dm").await?;
    let revision: i64 = sqlx::query_scalar(
        "SELECT runtime_revision FROM dm.testing_environments WHERE environment_id=$1",
    )
    .bind(op.environment_id)
    .fetch_one(app.store.pool())
    .await?;
    let store = PostgresStore::connect_schema(
        &app.settings.testing.as_ref().ok_or("testing")?.database,
        &format!("dm_test_{}", op.environment_id.simple()),
    )
    .await?;
    let mut settings = app.settings.worker.clone();
    settings.poll_interval = Duration::from_millis(10);
    settings.max_retry_delay = Duration::from_secs(1);
    let publisher = Arc::new(RecoveredTingPublisher::default());
    let worker = Arc::new(
        TingDeliveryWorker::new(
            store.clone(),
            publisher.clone(),
            TingDeliveryContext {
                app_id: "tos>dm".into(),
                testing_environment_id: Some(op.environment_id),
                testing_generation: Some(op.generation),
            },
            "sandbox-recovery".into(),
            settings,
        )
        .with_testing_registry(registry.clone())
        .with_runtime_revision(revision),
    );
    iam.reset().await;
    for _ in 0..3 {
        assert_eq!(worker.process_once().await?, 0);
    }
    worker.maintain_once().await?;
    assert!(
        iam.received_requests().await.ok_or("requests")?.is_empty(),
        "idle and maintenance must not query IAM"
    );
    let alice = ActorRef {
        actor_type: ActorType::Carbon,
        id: "alice".parse()?,
    };
    let bob = ActorRef {
        actor_type: ActorType::Carbon,
        id: "bob".parse()?,
    };
    let organization: OrganizationId = "tos".parse()?;
    let chat = store
        .create_conversation(CreateConversationCommand {
            organization_id: organization.clone(),
            creator: alice.clone(),
            participants: vec![alice.clone(), bob],
            idempotency_key: "readiness-chat".parse()?,
        })
        .await?;
    store
        .send_message(SendMessageCommand {
            organization_id: organization,
            conversation_id: chat.id,
            sender: alice,
            content: MessageCreate {
                text: Some("retained readiness retry".into()),
                ..MessageCreate::default()
            },
            idempotency_key: "readiness-message".parse()?,
        })
        .await?;
    Mock::given(path("/api/v1/application/testing-context"))
        .respond_with(
            ResponseTemplate::new(503)
                .set_body_json(json!({"error":{"code":"unavailable","message":"fixture"}})),
        )
        .mount(iam)
        .await;
    let cancel = tokio_util::sync::CancellationToken::new();
    let running = {
        let worker = worker.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move { worker.run(cancel).await })
    };
    wait_readiness_attempts(&store, 1, false).await?;
    assert!(
        !running.is_finished(),
        "503 must not kill the cached worker"
    );
    assert!(publisher.0.lock().map_err(|_| "publisher")?.is_empty());
    iam.reset().await;
    Mock::given(path("/api/v1/application/testing-context"))
        .respond_with(
            ResponseTemplate::new(429)
                .set_body_json(json!({"error":{"code":"rate_limited","message":"fixture"}})),
        )
        .mount(iam)
        .await;
    sqlx::query("UPDATE ting_handoffs SET next_attempt_at=clock_timestamp()")
        .execute(store.pool())
        .await?;
    wait_readiness_attempts(&store, 2, false)
        .await
        .map_err(|e| format!("429 recovery: {e}"))?;
    assert!(
        !running.is_finished(),
        "429 must not kill the cached worker"
    );
    assert!(publisher.0.lock().map_err(|_| "publisher")?.is_empty());
    let original: Vec<String> =
        sqlx::query_scalar("SELECT request_body FROM ting_handoffs ORDER BY request_body")
            .fetch_all(store.pool())
            .await?;
    iam.reset().await;
    mock_context(iam, op.environment_id, secret, 2, None).await;
    sqlx::query("UPDATE ting_handoffs SET next_attempt_at=clock_timestamp()")
        .execute(store.pool())
        .await?;
    wait_readiness_attempts(&store, 2, true).await?;
    let mut delivered_bodies = publisher.0.lock().map_err(|_| "publisher")?.clone();
    delivered_bodies.sort();
    assert_eq!(
        delivered_bodies, original,
        "recovery must publish the unchanged body and idempotency key"
    );
    assert!(!running.is_finished());
    op.operation_id = Uuid::new_v4();
    op.environment_revision += 1;
    registry.honeycomb_operation(&op, "tos>dm").await?;
    assert!(
        matches!(
            tokio::time::timeout(Duration::from_secs(2), running).await??,
            Err(AppError::Unauthorized)
        ),
        "epoch revocation still stops the old worker"
    );
    cancel.cancel();
    Ok(())
}

async fn wait_readiness_attempts(store: &PostgresStore, attempt: i64, accepted: bool) -> Result {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let count: i64 = sqlx::query_scalar("SELECT count(*) FROM ting_handoffs WHERE attempt_count >= $1 AND (accepted_at IS NOT NULL)=$2 AND ($2 OR last_error_code='ting_authority_unavailable')")
                .bind(attempt).bind(accepted).fetch_one(store.pool()).await?;
            if count == 2 { return Ok::<(), sqlx::Error>(()); }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await??;
    Ok(())
}
