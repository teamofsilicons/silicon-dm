//! Stateless Silicon DM client. Callers own credentials, retry policy and durable cursors.
//! No IAM application secrets, local files or backend-internal operations are required.
/// Optional Silicon-to-Carbon CLI message safety policy.
#[cfg(feature = "runtime")]
pub mod message_length;
pub mod models;
pub mod relay;
#[cfg(feature = "runtime")]
pub mod runtime;
pub mod ting;
pub use models::*;
pub use silicon_dm_protocol::Envelope;

use reqwest::{Client as HttpClient, Method, RequestBuilder};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
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
    /// Acquire a reset anchor, refresh canonical history, then resume that cursor.
    pub fn sync_reset_required(&self) -> bool {
        matches!(self, Self::Api { status: 409, code, .. } if code == "sync_reset_required")
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
    telemetry_enabled: bool,
    telemetry_source: &'static str,
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
            telemetry_enabled: true,
            telemetry_source: "sdk",
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
        if !((key.len() == 32 && key.bytes().all(|b| b.is_ascii_alphanumeric()))
            || (key.len() == 47
                && key.starts_with("ask_")
                && key[4..]
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))))
        {
            return Err(Error::Configuration(
                "provide an IAM test app_secret (ask_...) or a legacy 32-character DM test key"
                    .into(),
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
    /// Disable diagnostic collection for requests and connections from this client.
    pub fn with_telemetry(mut self, enabled: bool) -> Self {
        self.telemetry_enabled = enabled;
        self
    }
    /// Select a known diagnostic source without accepting arbitrary identifying text.
    pub fn with_source(mut self, source: &'static str) -> Self {
        self.telemetry_source = match source {
            "cli" | "daemon" | "web" => source,
            _ => "sdk",
        };
        self
    }
    /// Submit a bounded diagnostic event; callers choose whether to wait or drop on failure.
    /// No table key, message content, token or callback URL is included.
    pub async fn telemetry(&self, event: &str, success: bool, duration_ms: u64) -> Result<()> {
        if !self.telemetry_enabled {
            return Ok(());
        }
        self.empty(self.request(Method::POST, "telemetry")?
            .timeout(Duration::from_millis(500))
            .json(&json!({"source":self.telemetry_source,"event":event,"success":success,"duration_ms":duration_ms}))).await
    }
    fn endpoint(&self, path: &str) -> Result<Url> {
        self.base
            .join(path)
            .map_err(|e| Error::Configuration(e.to_string()))
    }
    fn request(&self, method: Method, path: &str) -> Result<RequestBuilder> {
        let mut request = self
            .http
            .request(method, self.endpoint(path)?)
            .header("X-DM-Contract-Version", "3")
            .header(
                "X-DM-Telemetry",
                if self.telemetry_enabled { "on" } else { "off" },
            )
            .header("X-DM-Source", self.telemetry_source);
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
    async fn send(&self, builder: RequestBuilder) -> Result<reqwest::Response> {
        let mut request = builder.build()?;
        if let Some(body) = request.body().and_then(reqwest::Body::as_bytes) {
            let kind =
                silicon_dm_protocol::http_type(request.method().as_str(), request.url().path());
            // Preserve already-serialized JSON bytes, avoiding a second parsed message allocation.
            let mut encoded = format!("{{\"type\":\"{kind}\",\"data\":").into_bytes();
            encoded.extend_from_slice(body);
            encoded.push(b'}');
            *request.body_mut() = Some(encoded.into());
        }
        Ok(self.http.execute(request).await?)
    }
    async fn json<T: DeserializeOwned>(&self, request: RequestBuilder) -> Result<T> {
        let response = checked(self.send(request).await?).await?;
        Ok(serde_json::from_slice::<Envelope<T>>(&response.bytes().await?)?.data)
    }
    async fn empty(&self, request: RequestBuilder) -> Result<()> {
        checked(self.send(request).await?).await?;
        Ok(())
    }
    /// Discover supported contracts and the local deprecation lifecycle.
    pub async fn contracts(&self) -> Result<Value> {
        self.json(self.request(Method::GET, "contracts")?).await
    }
    /// Durably submit a bug report. Reuse the idempotency key when retrying.
    /// Production sends Postmark notifications; test environments simulate them.
    pub async fn report(&self, message: &str, pr: Option<&str>, key: &str) -> Result<Value> {
        self.json(
            self.request(Method::POST, "reports")?
                .header("Idempotency-Key", key)
                .json(
                    &json!({"message":message,"pr":pr,"client_version":env!("CARGO_PKG_VERSION")}),
                ),
        )
        .await
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
    /// Explicitly enrolls this recipient's DM grant in Ting. Reuse the key on retry.
    pub async fn register_delivery(&self, key: &str) -> Result<DeliveryRegistration> {
        self.json(
            self.request(Method::POST, "delivery/registration")?
                .header("Idempotency-Key", key)
                .json(&json!({})),
        )
        .await
    }
    /// Fetches authoritative event references; Ting sequence numbers are not cursors.
    pub async fn sync(&self, request: &SyncRequest) -> Result<SyncPage> {
        if request.reset && request.cursor.is_some() {
            return Err(Error::Configuration(
                "sync reset cannot be combined with a cursor".into(),
            ));
        }
        if request
            .limit
            .is_some_and(|limit| !(1..=100).contains(&limit))
        {
            return Err(Error::Configuration(
                "sync limit must be between 1 and 100".into(),
            ));
        }
        self.json(self.request(Method::GET, "sync")?.query(request))
            .await
    }
    /// Anchors recovery at the current head. Refresh conversation/message snapshots
    /// after this call, then resume the returned cursor so arrivals are not skipped.
    pub async fn sync_reset(&self) -> Result<SyncPage> {
        self.sync(&SyncRequest {
            reset: true,
            ..SyncRequest::default()
        })
        .await
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
    /// Lists groups accessible to this token; group IDs are conversation IDs.
    pub async fn groups(&self, page: &PageRequest) -> Result<Page<Conversation>> {
        let mut result = self.conversations(page).await?;
        result
            .items
            .retain(|conversation| conversation.group.is_some());
        Ok(result)
    }
    pub async fn group(&self, id: impl std::fmt::Display) -> Result<Conversation> {
        let id = id.to_string();
        validate_conversation_id(&id)?;
        self.json(self.request(Method::GET, &format!("groups/{id}"))?)
            .await
    }
    pub async fn create_group(&self, input: &GroupCreate, key: &str) -> Result<Conversation> {
        self.json(
            self.request(Method::POST, "groups")?
                .header("Idempotency-Key", key)
                .json(input),
        )
        .await
    }
    pub async fn update_group(
        &self,
        id: impl std::fmt::Display,
        settings: &GroupSettings,
        version: i64,
        key: &str,
    ) -> Result<GroupDetails> {
        let id = id.to_string();
        validate_conversation_id(&id)?;
        self.json(
            self.request(Method::PATCH, &format!("groups/{id}"))?
                .header("If-Match", version)
                .header("Idempotency-Key", key)
                .json(settings),
        )
        .await
    }
    pub async fn invite_group_members(
        &self,
        id: impl std::fmt::Display,
        members: &[String],
        key: &str,
    ) -> Result<GroupDetails> {
        let id = id.to_string();
        validate_conversation_id(&id)?;
        self.json(
            self.request(Method::POST, &format!("groups/{id}/members"))?
                .header("Idempotency-Key", key)
                .json(&json!({"member_ids":members})),
        )
        .await
    }
    /// Removes explicit invitations; independent public/tag access remains effective.
    pub async fn remove_group_members(
        &self,
        id: impl std::fmt::Display,
        members: &[String],
        key: &str,
    ) -> Result<GroupDetails> {
        let id = id.to_string();
        validate_conversation_id(&id)?;
        self.json(
            self.request(Method::DELETE, &format!("groups/{id}/members"))?
                .header("Idempotency-Key", key)
                .json(&json!({"member_ids":members})),
        )
        .await
    }
    pub async fn messages(
        &self,
        conversation: impl std::fmt::Display,
        page: &PageRequest,
        include_bundled: bool,
    ) -> Result<Page<Message>> {
        let conversation = conversation.to_string();
        validate_conversation_id(&conversation)?;
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
        conversation: impl std::fmt::Display,
        message: &MessageCreate,
        key: &str,
    ) -> Result<Message> {
        let conversation = conversation.to_string();
        validate_conversation_id(&conversation)?;
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
    pub async fn message(
        &self,
        conversation: impl std::fmt::Display,
        message: impl std::fmt::Display,
    ) -> Result<Message> {
        let message = message.to_string();
        if Uuid::parse_str(&message).is_err()
            && silicon_dm_protocol::message_sequence(&message).is_none()
        {
            return Err(Error::Configuration(
                "expected a conversation-local message code or legacy UUID".into(),
            ));
        }
        let conversation = conversation.to_string();
        validate_conversation_id(&conversation)?;
        self.json(self.request(
            Method::GET,
            &format!("conversations/{conversation}/messages/{message}"),
        )?)
        .await
    }
    pub async fn edit_message(
        &self,
        conversation: impl std::fmt::Display,
        message: impl std::fmt::Display,
        content: &MessageCreate,
        key: &str,
    ) -> Result<Message> {
        let message = message.to_string();
        if Uuid::parse_str(&message).is_err()
            && silicon_dm_protocol::message_sequence(&message).is_none()
        {
            return Err(Error::Configuration(
                "expected a conversation-local message code or legacy UUID".into(),
            ));
        }
        let conversation = conversation.to_string();
        validate_conversation_id(&conversation)?;
        self.json(
            self.request(
                Method::PATCH,
                &format!("conversations/{conversation}/messages/{message}"),
            )?
            .header("Idempotency-Key", key)
            .json(content),
        )
        .await
    }
    pub async fn delete_message(
        &self,
        conversation: impl std::fmt::Display,
        message: impl std::fmt::Display,
        key: &str,
    ) -> Result<Message> {
        let message = message.to_string();
        if Uuid::parse_str(&message).is_err()
            && silicon_dm_protocol::message_sequence(&message).is_none()
        {
            return Err(Error::Configuration(
                "expected a conversation-local message code or legacy UUID".into(),
            ));
        }
        let conversation = conversation.to_string();
        validate_conversation_id(&conversation)?;
        self.json(
            self.request(
                Method::DELETE,
                &format!("conversations/{conversation}/messages/{message}"),
            )?
            .header("Idempotency-Key", key),
        )
        .await
    }
    pub async fn record_receipt(
        &self,
        conversation: impl std::fmt::Display,
        message: impl std::fmt::Display,
        status: ReceiptStatus,
        device: &str,
    ) -> Result<Message> {
        let message = message.to_string();
        if Uuid::parse_str(&message).is_err()
            && silicon_dm_protocol::message_sequence(&message).is_none()
        {
            return Err(Error::Configuration(
                "expected a conversation-local message code or legacy UUID".into(),
            ));
        }
        let conversation = conversation.to_string();
        validate_conversation_id(&conversation)?;
        self.json(
            self.request(
                Method::POST,
                &format!("conversations/{conversation}/messages/{message}/receipts"),
            )?
            .json(&json!({"status":status,"device_id":device})),
        )
        .await
    }
    pub async fn draft(&self, conversation: impl std::fmt::Display) -> Result<Draft> {
        let conversation = conversation.to_string();
        validate_conversation_id(&conversation)?;
        self.json(self.request(Method::GET, &format!("conversations/{conversation}/draft"))?)
            .await
    }
    pub async fn put_draft(
        &self,
        conversation: impl std::fmt::Display,
        draft: &DraftInput,
        version: i64,
    ) -> Result<Draft> {
        let conversation = conversation.to_string();
        validate_conversation_id(&conversation)?;
        self.json(
            self.request(Method::PUT, &format!("conversations/{conversation}/draft"))?
                .header("If-Match", version)
                .json(draft),
        )
        .await
    }
    pub async fn delete_draft(&self, conversation: impl std::fmt::Display) -> Result<()> {
        let conversation = conversation.to_string();
        validate_conversation_id(&conversation)?;
        self.empty(self.request(
            Method::DELETE,
            &format!("conversations/{conversation}/draft"),
        )?)
        .await
    }
    pub async fn create_bundle(
        &self,
        conversation: impl std::fmt::Display,
        bundle: &BundleCreate,
        key: &str,
    ) -> Result<Bundle> {
        let conversation = conversation.to_string();
        validate_conversation_id(&conversation)?;
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
    pub async fn bundle(
        &self,
        conversation: impl std::fmt::Display,
        bundle: &str,
    ) -> Result<Bundle> {
        let conversation = conversation.to_string();
        validate_conversation_id(&conversation)?;
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
    pub async fn renew_presence(
        &self,
        device_id: &str,
        activity: Option<Activity>,
    ) -> Result<PresenceLease> {
        let path = presence_device_path(device_id)?;
        self.json(
            self.request(Method::PUT, &path)?
                .json(&json!({"activity":activity})),
        )
        .await
    }
    pub async fn close_presence(&self, device_id: &str) -> Result<()> {
        self.empty(self.request(Method::DELETE, &presence_device_path(device_id)?)?)
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
    /// DM delivery sockets are retired. Register delivery, then attach the generic
    /// Ting consumer to a local webhook and use HTTP sync/message APIs here.
    pub async fn prewarm_shared(&self) -> Result<Socket> {
        Err(retired_delivery_socket())
    }

    /// Kept for source compatibility; Ting now owns incoming delivery connections.
    pub async fn connect(&self, _actors: &[String], _device_id: &str) -> Result<Socket> {
        Err(retired_delivery_socket())
    }

    /// Kept for source compatibility. Discover generation through `iam()` instead.
    pub async fn connect_with_generation(
        &self,
        _actors: &[String],
        _device_id: &str,
        _testing_generation: Option<i64>,
    ) -> Result<Socket> {
        Err(retired_delivery_socket())
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
fn response_is_draft_conflict(body: &Value) -> bool {
    body["version"].is_i64()
        && body["conversation_id"].is_string()
        && (body["member_id"].is_string() || body["actor_id"].is_string())
}
async fn checked(response: reqwest::Response) -> Result<reqwest::Response> {
    if response.status().is_success() {
        return Ok(response);
    }
    let status = response.status().as_u16();
    let status_message = response
        .status()
        .canonical_reason()
        .unwrap_or("HTTP request failed");
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
    let bytes = response.bytes().await?;
    let body = match serde_json::from_slice::<Value>(&bytes) {
        Ok(mut value) if value.get("type").is_some() && value.get("data").is_some() => {
            value["data"].take()
        }
        Ok(value) => value,
        Err(_) => json!({"raw": String::from_utf8_lossy(&bytes)}),
    };
    let draft_conflict = status == 409 && response_is_draft_conflict(&body);
    let code = body
        .pointer("/error/code")
        .and_then(Value::as_str)
        .unwrap_or(if draft_conflict {
            "draft_conflict"
        } else {
            "http_error"
        })
        .to_owned();
    let message = body
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or(if draft_conflict {
            "The draft changed since the supplied version. Your save was not applied. Resolve against the returned draft and retry with its current version."
        } else {
            status_message
        })
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

fn validate_conversation_id(value: &str) -> Result<()> {
    if !value.is_empty()
        && value.len() <= 515
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_-.:@+".contains(&c))
    {
        Ok(())
    } else {
        Err(Error::Configuration(
            "expected an account ID, direct conversation address, UUID or group address".into(),
        ))
    }
}

fn retired_delivery_socket() -> Error {
    Error::Configuration("DM incoming WebSocket delivery moved to Ting. Use register_delivery(), attach your generic Ting consumer webhook, and use iam()/sync() plus HTTP message and presence methods.".into())
}

fn presence_device_path(device: &str) -> Result<String> {
    if device.is_empty()
        || device.len() > 255
        || device.chars().any(char::is_control)
        || matches!(device, "." | "..")
    {
        return Err(Error::Configuration(
            "device_id must contain 1 to 255 bytes without controls and cannot be a URL dot segment".into(),
        ));
    }
    // URL path-segment encoding (form encoding would turn spaces into literal '+').
    let encoded = url::form_urlencoded::byte_serialize(device.as_bytes())
        .collect::<String>()
        .replace('+', "%20");
    Ok(format!("presence/devices/{encoded}"))
}
