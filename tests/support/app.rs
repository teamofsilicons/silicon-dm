//! A DM service wired to a fresh database and the fake Accounts, driven in-process.
#![allow(
    dead_code,
    reason = "each integration test binary uses a different subset"
)]

use std::{num::NonZeroU32, sync::Arc, time::Duration};

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode},
};
use secrecy::SecretString;
use serde_json::{Value, json};
use silicon_dm::{
    application::{
        accounts::AccountRuntime, speech::SpeechService, state::AppState, sync_hub::SyncHub,
    },
    config::{
        AccountsSettings, DataKey, DatabaseSettings, ProofIssuers, ProviderSettings,
        RealtimeSettings, RuntimeEnvironment, ServerSettings, Settings, SpeechSettings,
        TingSettings, WorkerSettings,
    },
    infrastructure::{
        accounts::AccountsAdapter, deepgram::DeepgramClient, giphy::GiphyClient,
        postgres::PostgresStore,
    },
};
use tower::ServiceExt as _;

use super::{
    BoxError, TestDatabase,
    accounts::{APP_ID, APP_SECRET, FakeAccounts, WEBHOOK_SECRET},
    deepgram::{DEEPGRAM_TEST_KEY, FakeDeepgram},
};

pub fn settings(
    database_url: &str,
    accounts: &FakeAccounts,
    proof_issuers: &str,
) -> Result<Settings, BoxError> {
    let issuer = accounts.issuer();
    Ok(Settings {
        environment: RuntimeEnvironment::Test,
        server: ServerSettings {
            bind_addr: "127.0.0.1:0".parse()?,
            public_base_url: "http://127.0.0.1:4121/api/v1".parse()?,
            request_timeout: Duration::from_secs(15),
            max_body_bytes: 134_217_728,
            shutdown_timeout: Duration::from_secs(5),
        },
        database: DatabaseSettings {
            url: SecretString::from(database_url.to_owned()),
            max_connections: NonZeroU32::new(8).ok_or("pool size")?,
            min_connections: 1,
            acquire_timeout: Duration::from_secs(10),
            statement_timeout: Duration::from_secs(30),
        },
        accounts: AccountsSettings {
            api_url: issuer.parse()?,
            issuer,
            app_id: APP_ID.to_owned(),
            app_secret: SecretString::from(APP_SECRET.to_owned()),
            webhook_secret: SecretString::from(WEBHOOK_SECRET.to_owned()),
            request_timeout: Duration::from_secs(5),
            proof_issuers: ProofIssuers::parse(proof_issuers)?,
        },
        data_key: DataKey::from_bytes([42; 32]),
        ting: TingSettings {
            base_url: None,
            app_id: "ting".to_owned(),
            request_timeout: Duration::from_secs(5),
        },
        providers: ProviderSettings {
            giphy_api_base_url: format!("{}/giphy", accounts.issuer()).parse()?,
            giphy_api_key: SecretString::from("giphy-test".to_owned()),
            request_timeout: Duration::from_secs(5),
            trending_cache_ttl: Duration::from_secs(300),
        },
        realtime: RealtimeSettings {
            heartbeat_interval: Duration::from_secs(30),
            heartbeat_timeout: Duration::from_secs(120),
            outbound_capacity: "256".parse()?,
            activity_ttl: Duration::from_secs(10),
        },
        worker: WorkerSettings {
            batch_size: "10".parse()?,
            poll_interval: Duration::from_millis(50),
            lease_duration: Duration::from_secs(5),
            max_attempts: 20,
            max_retry_delay: Duration::from_secs(5),
        },
        reporting: None,
        reports_per_hour: 10,
        speech: SpeechSettings::defaults()?,
        log_filter: "warn".to_owned(),
    })
}

/// A migrated database, the fake Accounts and the in-process router.
pub struct TestApp {
    pub database: TestDatabase,
    pub accounts: FakeAccounts,
    pub state: AppState,
    pub router: Router,
}

impl TestApp {
    pub async fn start() -> Result<Self, BoxError> {
        Self::start_with(
            "dm.conversations.read=interface,dm.messages.read=interface,dm.messages.write=interface",
        )
        .await
    }

    pub async fn start_with(proof_issuers: &str) -> Result<Self, BoxError> {
        Self::start_on(TestDatabase::start().await?, proof_issuers).await
    }

    /// The service on an existing database (migrated to the latest schema first).
    pub async fn start_on(database: TestDatabase, proof_issuers: &str) -> Result<Self, BoxError> {
        Self::start_configured(database, proof_issuers, |_| {}).await
    }

    /// The service with speech through `deepgram` (a fake), adjusted by `configure`.
    pub async fn start_with_speech(
        deepgram: &FakeDeepgram,
        configure: impl FnOnce(&mut SpeechSettings),
    ) -> Result<Self, BoxError> {
        let issuers = "dm.conversations.read=interface,dm.messages.read=interface,dm.messages.write=interface";
        let deepgram_url: url::Url = deepgram.base_url.parse()?;
        let agent_url: url::Url = deepgram.agent_url.parse()?;
        Self::start_configured(TestDatabase::start().await?, issuers, move |settings| {
            settings.speech.deepgram_api_key =
                Some(SecretString::from(DEEPGRAM_TEST_KEY.to_owned()));
            settings.speech.deepgram_url = deepgram_url;
            settings.speech.agent_url = agent_url;
            configure(&mut settings.speech);
        })
        .await
    }

    /// The service with Ting configured at `ting` (`DM_TING_URL`).
    pub async fn start_with_ting(ting: url::Url) -> Result<Self, BoxError> {
        let issuers = "dm.conversations.read=interface,dm.messages.read=interface,dm.messages.write=interface";
        Self::start_configured(TestDatabase::start().await?, issuers, move |settings| {
            settings.ting.base_url = Some(ting);
        })
        .await
    }

    pub async fn start_configured(
        database: TestDatabase,
        proof_issuers: &str,
        configure: impl FnOnce(&mut Settings),
    ) -> Result<Self, BoxError> {
        let accounts = FakeAccounts::start().await;
        let mut settings = settings(&database.url, &accounts, proof_issuers)?;
        configure(&mut settings);
        let store = PostgresStore::connect(&settings.database).await?;
        store.migrate().await?;
        let speech = SpeechService::new(DeepgramClient::new(&settings.speech)?.map(|client| {
            Arc::new(client) as Arc<dyn silicon_dm::application::ports::SpeechProvider>
        }));
        let state = AppState {
            instance_id: Arc::from("test-instance"),
            identity: Arc::new(AccountsAdapter::new(&settings.accounts)?),
            gifs: Arc::new(GiphyClient::new(&settings.providers)?),
            settings: Arc::new(settings),
            store,
            accounts: AccountRuntime::default(),
            sync: SyncHub::default(),
            speech,
        };
        let router = silicon_dm::api::build_router(state.clone());
        Ok(Self {
            database,
            accounts,
            state,
            router,
        })
    }

    pub async fn close(self) -> Result<(), BoxError> {
        self.state.store.pool().close().await;
        self.database.close().await
    }

    /// Calls the API as `token` (or anonymously). JSON bodies are wrapped in the
    /// `{type, data}` envelope the route expects; mutations get an idempotency key unless
    /// `extra` names one. Returns the status and the response's `data` (or the raw body).
    pub async fn call(
        &self,
        method: Method,
        path: &str,
        token: Option<&str>,
        body: Option<Value>,
        extra: &[(&str, String)],
    ) -> Result<(StatusCode, Value), BoxError> {
        let mut request = Request::builder()
            .method(method.clone())
            .uri(path)
            .header("x-dm-telemetry", "off");
        if let Some(token) = token {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        let mutation = !matches!(method, Method::GET | Method::HEAD);
        if mutation
            && !extra
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case("idempotency-key"))
        {
            request = request.header(
                "idempotency-key",
                format!("test-{}", uuid::Uuid::new_v4().simple()),
            );
        }
        for (name, value) in extra {
            request = request.header(*name, value);
        }
        let request = match body {
            Some(data) => {
                let kind = silicon_dm_protocol::http_type(method.as_str(), path);
                request
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"type": kind, "data": data}).to_string()))?
            }
            None => request.body(Body::empty())?,
        };
        let response = self.router.clone().oneshot(request).await?;
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await?;
        let value: Value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| json!({"raw": String::from_utf8_lossy(&bytes)}))
        };
        Ok((status, value.get("data").cloned().unwrap_or(value)))
    }

    pub async fn get(&self, path: &str, token: &str) -> Result<(StatusCode, Value), BoxError> {
        self.call(Method::GET, path, Some(token), None, &[]).await
    }

    pub async fn post(
        &self,
        path: &str,
        token: &str,
        body: Value,
    ) -> Result<(StatusCode, Value), BoxError> {
        self.call(Method::POST, path, Some(token), Some(body), &[])
            .await
    }

    /// Sends a request as built and returns the status, headers and raw body.
    pub async fn raw(
        &self,
        request: Request<Body>,
    ) -> Result<(StatusCode, axum::http::HeaderMap, axum::body::Bytes), BoxError> {
        let response = self.router.clone().oneshot(request).await?;
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = to_bytes(response.into_body(), usize::MAX).await?;
        Ok((status, headers, bytes))
    }

    /// `GET path` as `token` with extra headers, answering raw bytes (audio).
    pub async fn get_raw(
        &self,
        path: &str,
        token: &str,
        headers: &[(&str, &str)],
    ) -> Result<(StatusCode, axum::http::HeaderMap, axum::body::Bytes), BoxError> {
        let mut request = Request::builder()
            .method(Method::GET)
            .uri(path)
            .header("authorization", format!("Bearer {token}"));
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        self.raw(request.body(Body::empty())?).await
    }

    /// `POST path` as `token` with an exact envelope (any `type`), answering raw bytes.
    pub async fn post_envelope(
        &self,
        path: &str,
        token: &str,
        envelope: &Value,
    ) -> Result<(StatusCode, axum::http::HeaderMap, axum::body::Bytes), BoxError> {
        let request = Request::builder()
            .method(Method::POST)
            .uri(path)
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .header(
                "idempotency-key",
                format!("test-{}", uuid::Uuid::new_v4().simple()),
            )
            .body(Body::from(envelope.to_string()))?;
        self.raw(request).await
    }

    /// Delivers a signed webhook (raw body) and returns the status.
    pub async fn deliver(
        &self,
        headers: &[(&'static str, String)],
        body: &str,
    ) -> Result<StatusCode, BoxError> {
        let mut request = Request::builder().method(Method::POST).uri("/webhook/");
        for (name, value) in headers {
            request = request.header(*name, value);
        }
        let response = self
            .router
            .clone()
            .oneshot(request.body(Body::from(body.to_owned()))?)
            .await?;
        Ok(response.status())
    }
}

/// Parses a JSON envelope's `data` (or the raw body as a string).
pub fn data_of(bytes: &[u8]) -> Value {
    let value: Value = serde_json::from_slice(bytes)
        .unwrap_or_else(|_| json!({"raw": String::from_utf8_lossy(bytes)}));
    value.get("data").cloned().unwrap_or(value)
}

/// Fails with the body when the status is not the expected one.
pub fn expect_status(result: &(StatusCode, Value), expected: StatusCode) -> Result<(), BoxError> {
    if result.0 == expected {
        Ok(())
    } else {
        Err(format!("expected {expected}, got {}: {}", result.0, result.1).into())
    }
}
