//! Stateless Silicon DM client. Callers own credentials, retry policy and durable cursors.
//! No IAM application secrets, local files or backend-internal operations are required.
pub mod models;
pub mod relay;
#[cfg(feature = "runtime")]
pub mod runtime;
pub use models::*;

use reqwest::{Client as HttpClient, Method, RequestBuilder};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, tungstenite::client::IntoClientRequest};
use url::Url;
use uuid::Uuid;

pub type Result<T> = std::result::Result<T, Error>;
pub type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// HTTP failures preserve the response body, including current-draft conflict data.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid client configuration: {0}")]
    Configuration(String),
    #[error("DM transport failed: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("DM returned HTTP {status}: {code}: {message}")]
    Api {
        status: u16,
        code: String,
        message: String,
        body: Box<Value>,
        request_id: Option<String>,
        retry_after: Option<String>,
    },
    #[error("invalid DM response: {0}")]
    Decode(#[from] serde_json::Error),
    #[error("WebSocket connection failed: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),
}
impl Error {
    /// Retrying a mutation requires reusing its original idempotency key.
    pub fn retryable(&self) -> bool {
        matches!(self, Self::Transport(_) | Self::WebSocket(_))
            || matches!(
                self,
                Self::Api {
                    status: 408 | 429 | 500..=599,
                    ..
                }
            )
    }
    pub fn unauthorized(&self) -> bool {
        matches!(self, Self::Api { status: 401, .. })
    }
}

/// Immutable connection configuration. Debug intentionally omits bearer/test secrets.
#[derive(Clone)]
pub struct Client {
    http: HttpClient,
    base: Url,
    token: Option<String>,
    organization: Option<String>,
    test_key: Option<String>,
    websocket_limit: usize,
    testing_generation: Option<i64>,
}
impl Client {
    /// Accepts an origin or an `/api/v1` base. HTTP is limited to loopback hosts.
    pub fn new(base: impl AsRef<str>) -> Result<Self> {
        let mut base =
            Url::parse(base.as_ref()).map_err(|e| Error::Configuration(e.to_string()))?;
        validate_endpoint(&base)?;
        if base.query().is_some() || base.fragment().is_some() {
            return Err(Error::Configuration(
                "base URL must not have a query or fragment".into(),
            ));
        }
        let path = base.path().trim_end_matches('/');
        let path = if path.is_empty() {
            "/api/v1/".to_owned()
        } else {
            format!("{path}/")
        };
        base.set_path(&path);
        let http = HttpClient::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(45))
            .user_agent(concat!("silicon-dm-client/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            http,
            base,
            token: None,
            organization: None,
            test_key: None,
            websocket_limit: 128 * 1024 * 1024,
            testing_generation: None,
        })
    }
    pub fn with_auth(mut self, token: impl Into<String>, organization: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self.organization = Some(organization.into());
        self
    }
    /// Mandatory IAM sandbox selection remains the backend's responsibility.
    pub fn with_test_key(mut self, key: impl Into<String>) -> Result<Self> {
        let key = key.into();
        if key.len() != 32 || !key.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err(Error::Configuration(
                "testing environment key must be 32 alphanumeric characters".into(),
            ));
        }
        self.test_key = Some(key);
        Ok(self)
    }
    pub fn without_test(mut self) -> Self {
        self.test_key = None;
        self.testing_generation = None;
        self
    }
    /// Binds requests to a previously observed sandbox generation. A cleaned or
    /// rotated environment rejects stale requests rather than applying them anew.
    pub fn with_testing_generation(mut self, generation: i64) -> Result<Self> {
        if generation < 1 {
            return Err(Error::Configuration(
                "testing generation must be positive".into(),
            ));
        }
        self.testing_generation = Some(generation);
        Ok(self)
    }
    /// Sets a bounded encoded WebSocket message/frame limit, up to 3 GiB.
    /// The default 128 MiB accommodates DM's default API body ceiling.
    pub fn with_websocket_limit(mut self, max_bytes: usize) -> Result<Self> {
        if max_bytes == 0 || max_bytes as u64 > 3 * 1024 * 1024 * 1024 {
            return Err(Error::Configuration(
                "WebSocket limit must be between 1 byte and 3 GiB".into(),
            ));
        }
        self.websocket_limit = max_bytes;
        Ok(self)
    }
    fn endpoint(&self, path: &str) -> Result<Url> {
        self.base
            .join(path)
            .map_err(|e| Error::Configuration(e.to_string()))
    }
    fn request(&self, method: Method, path: &str) -> Result<RequestBuilder> {
        let mut request = self.http.request(method, self.endpoint(path)?);
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }
        if let Some(org) = &self.organization {
            request = request.header("X-Org-ID", org);
        }
        if let Some(key) = &self.test_key {
            request = request.header("X-Testing-Environment-Key", key);
        }
        if let Some(generation) = self.testing_generation {
            request = request.header("X-Testing-Environment-Generation", generation);
        }
        Ok(request)
    }
    async fn json<T: DeserializeOwned>(&self, request: RequestBuilder) -> Result<T> {
        let response = checked(request.send().await?).await?;
        Ok(serde_json::from_slice(&response.bytes().await?)?)
    }
    async fn empty(&self, request: RequestBuilder) -> Result<()> {
        checked(request.send().await?).await?;
        Ok(())
    }
    pub async fn login(&self, slt: &str, key: &str) -> Result<Tokens> {
        self.json(
            self.request(Method::POST, "auth/login")?
                .header("Idempotency-Key", key)
                .json(&json!({"slt": slt})),
        )
        .await
    }
    pub async fn refresh(&self, refresh_token: &str, key: &str) -> Result<Tokens> {
        self.json(
            self.request(Method::POST, "auth/refresh")?
                .header("Idempotency-Key", key)
                .json(&json!({"refresh_token": refresh_token})),
        )
        .await
    }
    pub async fn logout(&self, token: &str, key: &str) -> Result<()> {
        self.empty(
            self.request(Method::POST, "auth/logout")?
                .header("Idempotency-Key", key)
                .json(&json!({"token":token})),
        )
        .await
    }
    /// Public IAM application information; does not require login.
    pub async fn iam(&self) -> Result<IamInfo> {
        self.json(self.request(Method::GET, "iam")?).await
    }
    pub async fn me(&self) -> Result<Identity> {
        self.json(self.request(Method::GET, "auth/me")?).await
    }
    pub async fn conversations(&self, page: &PageRequest) -> Result<Page<Conversation>> {
        self.json(self.request(Method::GET, "conversations")?.query(page))
            .await
    }
    pub async fn create_conversation(
        &self,
        participants: &[String],
        key: &str,
    ) -> Result<Conversation> {
        self.json(
            self.request(Method::POST, "conversations")?
                .header("Idempotency-Key", key)
                .json(&json!({"participant_ids":participants})),
        )
        .await
    }
    pub async fn messages(
        &self,
        conversation: Uuid,
        page: &PageRequest,
        include_bundled: bool,
    ) -> Result<Page<Message>> {
        self.json(
            self.request(
                Method::GET,
                &format!("conversations/{conversation}/messages"),
            )?
            .query(page)
            .query(&[("include_bundled_members", include_bundled)]),
        )
        .await
    }
    pub async fn send_message(
        &self,
        conversation: Uuid,
        message: &MessageCreate,
        key: &str,
    ) -> Result<Message> {
        self.json(
            self.request(
                Method::POST,
                &format!("conversations/{conversation}/messages"),
            )?
            .header("Idempotency-Key", key)
            .json(message),
        )
        .await
    }
    pub async fn message(&self, conversation: Uuid, message: Uuid) -> Result<Message> {
        self.json(self.request(
            Method::GET,
            &format!("conversations/{conversation}/messages/{message}"),
        )?)
        .await
    }
    pub async fn edit_message(
        &self,
        conversation: Uuid,
        message: Uuid,
        content: &MessageCreate,
        version: i64,
        key: &str,
    ) -> Result<Message> {
        self.json(
            self.request(
                Method::PATCH,
                &format!("conversations/{conversation}/messages/{message}"),
            )?
            .header("If-Match", version)
            .header("Idempotency-Key", key)
            .json(content),
        )
        .await
    }
    pub async fn delete_message(
        &self,
        conversation: Uuid,
        message: Uuid,
        version: i64,
        key: &str,
    ) -> Result<Message> {
        self.json(
            self.request(
                Method::DELETE,
                &format!("conversations/{conversation}/messages/{message}"),
            )?
            .header("If-Match", version)
            .header("Idempotency-Key", key),
        )
        .await
    }
    pub async fn record_receipt(
        &self,
        conversation: Uuid,
        message: Uuid,
        status: ReceiptStatus,
        device: &str,
    ) -> Result<Message> {
        self.json(
            self.request(
                Method::POST,
                &format!("conversations/{conversation}/messages/{message}/receipts"),
            )?
            .json(&json!({"status":status,"device_id":device})),
        )
        .await
    }
    pub async fn draft(&self, conversation: Uuid) -> Result<Draft> {
        self.json(self.request(Method::GET, &format!("conversations/{conversation}/draft"))?)
            .await
    }
    pub async fn put_draft(
        &self,
        conversation: Uuid,
        draft: &DraftInput,
        version: i64,
    ) -> Result<Draft> {
        self.json(
            self.request(Method::PUT, &format!("conversations/{conversation}/draft"))?
                .header("If-Match", version)
                .json(draft),
        )
        .await
    }
    pub async fn delete_draft(&self, conversation: Uuid) -> Result<()> {
        self.empty(self.request(
            Method::DELETE,
            &format!("conversations/{conversation}/draft"),
        )?)
        .await
    }
    pub async fn create_bundle(
        &self,
        conversation: Uuid,
        bundle: &BundleCreate,
        key: &str,
    ) -> Result<Bundle> {
        self.json(
            self.request(
                Method::POST,
                &format!("conversations/{conversation}/bundles"),
            )?
            .header("Idempotency-Key", key)
            .json(bundle),
        )
        .await
    }
    pub async fn bundle(&self, conversation: Uuid, bundle: Uuid) -> Result<Bundle> {
        self.json(self.request(
            Method::GET,
            &format!("conversations/{conversation}/bundles/{bundle}"),
        )?)
        .await
    }
    pub async fn presence(&self, actor: &str) -> Result<Presence> {
        let encoded: String = url::form_urlencoded::byte_serialize(actor.as_bytes()).collect();
        self.json(self.request(Method::GET, &format!("presence/{encoded}"))?)
            .await
    }
    pub async fn gifs(&self, kind: GifList<'_>) -> Result<Page<Gif>> {
        let req = match kind {
            GifList::Trending => self.request(Method::GET, "gifs/trending")?,
            GifList::Recent => self.request(Method::GET, "gifs/recent")?,
            GifList::Search(q) => self.request(Method::GET, "gifs/search")?.query(&[("q", q)]),
        };
        self.json(req).await
    }
    pub async fn create_test_environment(
        &self,
        input: &TestEnvironmentCreate,
        key: &str,
    ) -> Result<TestEnvironment> {
        self.json(
            self.request(Method::POST, "testing-environments")?
                .header("Idempotency-Key", key)
                .json(input),
        )
        .await
    }
    pub async fn test_environments(&self, include_deleted: bool) -> Result<Page<TestEnvironment>> {
        self.json(
            self.request(Method::GET, "testing-environments")?
                .query(&[("include_deleted", include_deleted)]),
        )
        .await
    }
    pub async fn test_environment(&self, id: Uuid) -> Result<TestEnvironment> {
        self.json(self.request(Method::GET, &format!("testing-environments/{id}"))?)
            .await
    }
    pub async fn update_test_environment(
        &self,
        id: Uuid,
        input: &TestEnvironmentUpdate,
        key: &str,
    ) -> Result<TestEnvironment> {
        self.json(
            self.request(Method::PATCH, &format!("testing-environments/{id}"))?
                .header("Idempotency-Key", key)
                .json(input),
        )
        .await
    }
    pub async fn test_environment_key(&self, id: Uuid) -> Result<TestEnvironmentKey> {
        self.json(self.request(Method::GET, &format!("testing-environments/{id}/key"))?)
            .await
    }
    pub async fn rotate_test_environment_key(
        &self,
        id: Uuid,
        key: &str,
    ) -> Result<TestEnvironmentKey> {
        self.json(
            self.request(
                Method::POST,
                &format!("testing-environments/{id}/rotate-key"),
            )?
            .header("Idempotency-Key", key),
        )
        .await
    }
    pub async fn restore_test_environment(&self, id: Uuid, key: &str) -> Result<TestEnvironment> {
        self.json(
            self.request(Method::POST, &format!("testing-environments/{id}/restore"))?
                .header("Idempotency-Key", key),
        )
        .await
    }
    pub async fn clean_test_environment(&self, id: Uuid, key: &str) -> Result<()> {
        self.empty(
            self.request(Method::POST, &format!("testing-environments/{id}/clean"))?
                .header("Idempotency-Key", key),
        )
        .await
    }
    pub async fn delete_test_environment(&self, id: Uuid, key: &str) -> Result<()> {
        self.empty(
            self.request(Method::DELETE, &format!("testing-environments/{id}"))?
                .header("Idempotency-Key", key),
        )
        .await
    }
    /// Connects without reading/writing a cursor or automatically acknowledging messages.
    pub async fn connect(&self, actors: &[String], device_id: &str) -> Result<Socket> {
        self.connect_with_generation(actors, device_id, None).await
    }
    /// Supply the last observed sandbox generation; changed generations reset local cursors.
    pub async fn connect_with_generation(
        &self,
        actors: &[String],
        device_id: &str,
        testing_generation: Option<i64>,
    ) -> Result<Socket> {
        let mut url = self.endpoint("ws")?;
        url.set_scheme(if self.base.scheme() == "https" {
            "wss"
        } else {
            "ws"
        })
        .map_err(|()| Error::Configuration("invalid WebSocket scheme".into()))?;
        {
            let mut query = url.query_pairs_mut();
            query
                .append_pair(
                    "org_id",
                    self.organization.as_deref().ok_or_else(|| {
                        Error::Configuration("organization is required for WebSocket".into())
                    })?,
                )
                .append_pair("device_id", device_id);
            for actor in actors {
                query.append_pair("actors", actor);
            }
        }
        if let Some(generation) = testing_generation {
            url.query_pairs_mut()
                .append_pair("testing_generation", &generation.to_string());
        }
        let mut req = url.as_str().into_client_request()?;
        let token = self
            .token
            .as_ref()
            .ok_or_else(|| Error::Configuration("bearer token is required for WebSocket".into()))?;
        req.headers_mut().insert(
            "Authorization",
            format!("Bearer {token}")
                .parse()
                .map_err(|_| Error::Configuration("invalid bearer header".into()))?,
        );
        if let Some(key) = &self.test_key {
            req.headers_mut().insert(
                "X-Testing-Environment-Key",
                key.parse()
                    .map_err(|_| Error::Configuration("invalid test header".into()))?,
            );
        }
        let configuration = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
            .max_message_size(Some(self.websocket_limit))
            .max_frame_size(Some(self.websocket_limit))
            .max_write_buffer_size(self.websocket_limit.saturating_add(128 * 1024 + 1));
        let (socket, _) =
            tokio_tungstenite::connect_async_with_config(req, Some(configuration), true).await?;
        Ok(socket)
    }
}
pub enum GifList<'a> {
    Trending,
    Search(&'a str),
    Recent,
}

pub fn validate_endpoint(url: &Url) -> Result<()> {
    let loopback = matches!(
        url.host_str(),
        Some("localhost" | "dm.localhost" | "127.0.0.1" | "[::1]" | "::1")
    );
    if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
        return Err(Error::Configuration(
            "use HTTPS, or HTTP on loopback for local development".into(),
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(Error::Configuration(
            "URL credentials are not supported".into(),
        ));
    }
    Ok(())
}
async fn checked(response: reqwest::Response) -> Result<reqwest::Response> {
    if response.status().is_success() {
        return Ok(response);
    }
    let status = response.status().as_u16();
    let request_id = response
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let retry_after = response
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let body = response.json::<Value>().await.unwrap_or(Value::Null);
    let code = body
        .pointer("/error/code")
        .and_then(Value::as_str)
        .unwrap_or("http_error")
        .to_owned();
    let message = body
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or("request failed; inspect response body")
        .to_owned();
    Err(Error::Api {
        status,
        code,
        message,
        body: Box::new(body),
        request_id,
        retry_after,
    })
}
/// Available package release information. A linked Rust library cannot replace itself:
/// applications must update their dependency and rebuild to load a newer library.
#[derive(Clone, Debug, Serialize)]
pub struct UpdateInfo {
    pub package: String,
    pub current_version: String,
    pub latest_version: String,
    pub rebuild_command: String,
}
pub async fn check_update() -> Result<UpdateInfo> {
    let body = checked(
        HttpClient::builder()
            .timeout(Duration::from_secs(8))
            .user_agent(concat!("silicon-dm-client/", env!("CARGO_PKG_VERSION")))
            .build()?
            .get("https://crates.io/api/v1/crates/silicon-dm-client")
            .send()
            .await?,
    )
    .await?
    .json::<Value>()
    .await?;
    let latest = body
        .pointer("/crate/max_stable_version")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Configuration("registry did not return a stable version".into()))?;
    Ok(UpdateInfo {
        package: "silicon-dm-client".into(),
        current_version: env!("CARGO_PKG_VERSION").into(),
        latest_version: latest.into(),
        rebuild_command: "cargo update -p silicon-dm-client && cargo build --release".into(),
    })
}
