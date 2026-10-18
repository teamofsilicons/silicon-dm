//! Stateless DM client for HTTP contract 4. Callers own credentials, retry policy and
//! durable cursors.
//!
//! A request carries either a DM access token issued by Silicon Accounts
//! ([`Client::with_token`], `Authorization: Bearer …`) or, for an app acting for one of its
//! users, a User verification proof ([`Client::with_proof`], `Authorization: Proof sap_…`).
//! Signing in (device flow, short-lived tokens, refresh, revoke) lives in [`accounts`]
//! (feature `accounts`); the CLI's durable relay and session storage live in `runtime`
//! (feature `runtime`).
// Errors keep the service's whole error answer (code, message, hint, body, request id,
// retry-after) so callers can branch and report without another request; boxing it would
// make every match on `Error::Api` clumsier for no measurable gain on this I/O-bound path.
#![allow(clippy::result_large_err)]
#[cfg(feature = "accounts")]
pub mod accounts;
/// Optional Silicon-to-Carbon CLI message safety policy.
#[cfg(feature = "runtime")]
pub mod message_length;
pub mod models;
pub mod relay;
#[cfg(feature = "runtime")]
pub mod runtime;
pub mod speech;
pub mod ting;
pub use models::*;
pub use silicon_dm_protocol::Envelope;

/// The Rust examples of DM's docs and of this README, compiled as doctests
/// (`cargo test --doc -p silicon-dm-client --all-features`), so no page shows code that stopped
/// building. Only rustdoc's doctest collection compiles this module (`cfg(doctest)`), so the
/// pages outside the crate are never needed to build or publish it.
#[cfg(doctest)]
#[allow(dead_code)]
mod docs_examples {
    #[doc = include_str!("../README.md")]
    struct Readme;
    #[doc = include_str!("../../../docs/start/building.md")]
    struct Building;
    #[doc = include_str!("../../../docs/start/groups.md")]
    struct Groups;
    #[cfg(feature = "accounts")]
    #[doc = include_str!("../../../docs/reference/rust-client.md")]
    struct RustClient;
    #[cfg(feature = "runtime")]
    #[doc = include_str!("../../../docs/reference/rust-runtime.md")]
    struct RustRuntime;
}

use reqwest::{Client as HttpClient, Method, RequestBuilder};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::time::Duration;
use url::Url;
use uuid::Uuid;

/// The HTTP contract this client speaks (`x-dm-contract-version`).
pub const CONTRACT_VERSION: u16 = 4;
/// DM's app id at Silicon Accounts (the `aud` of its access tokens).
pub const APP_ID: &str = "dm";
/// The production DM service.
pub const DEFAULT_API_URL: &str = "https://api.dm.teamofsilicons.com";

pub type Result<T> = std::result::Result<T, Error>;

/// HTTP failures keep the service's `{error:{code,message,hint?}}` and the response body,
/// including current-draft conflict data.
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
        /// What to do next, when the service says.
        hint: Option<String>,
        body: Box<Value>,
        request_id: Option<String>,
        retry_after: Option<String>,
    },
    #[error("invalid DM response: {0}")]
    Decode(#[from] serde_json::Error),
    /// The service answered successfully but not with what the route promises (for example
    /// JSON where speech audio was expected, or audio larger than any DM renders).
    #[error("unexpected DM response: {0}")]
    UnexpectedResponse(String),
}
impl Error {
    /// Retrying a mutation requires reusing its original idempotency key.
    pub fn retryable(&self) -> bool {
        matches!(self, Self::Transport(_))
            || matches!(
                self,
                Self::Api {
                    status: 408 | 429 | 500..=599,
                    ..
                }
            )
    }
    /// The credential was refused (expired, signed out, or not a DM credential).
    pub fn unauthorized(&self) -> bool {
        matches!(self, Self::Api { status: 401, .. })
    }
    /// Acquire a reset anchor, refresh canonical history, then resume that cursor.
    pub fn sync_reset_required(&self) -> bool {
        self.is_code("sync_reset_required")
    }
    /// The stable machine-readable code: the service's `error.code`, or `configuration_error`,
    /// `transport_error`, `decode_error` for failures before or after the service answered.
    pub fn code(&self) -> &str {
        match self {
            Self::Api { code, .. } => code,
            Self::Configuration(_) => "configuration_error",
            Self::Transport(_) => "transport_error",
            Self::Decode(_) => "decode_error",
            Self::UnexpectedResponse(_) => "unexpected_response",
        }
    }
    /// What went wrong, for people.
    pub fn message(&self) -> String {
        match self {
            Self::Api { message, .. } => message.clone(),
            other => other.to_string(),
        }
    }
    /// What to do next, when known.
    pub fn hint(&self) -> Option<&str> {
        match self {
            Self::Api { hint, .. } => hint.as_deref(),
            _ => None,
        }
    }
    /// The HTTP status, when the service answered.
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Api { status, .. } => Some(*status),
            _ => None,
        }
    }
    pub fn is_code(&self, code: &str) -> bool {
        matches!(self, Self::Api { code: actual, .. } if actual == code)
    }
}

#[derive(Clone)]
enum Credential {
    Bearer(String),
    Proof(String),
}

/// Immutable connection configuration. Debug intentionally omits credentials.
#[derive(Clone)]
pub struct Client {
    http: HttpClient,
    base: Url,
    credential: Option<Credential>,
}
impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("base", &self.base.as_str())
            .field(
                "credential",
                &match &self.credential {
                    Some(Credential::Bearer(_)) => "bearer",
                    Some(Credential::Proof(_)) => "proof",
                    None => "none",
                },
            )
            .finish_non_exhaustive()
    }
}
impl Client {
    /// Accepts an origin or an `/api/v1` base. HTTP is limited to loopback hosts.
    pub fn new(base: impl AsRef<str>) -> Result<Self> {
        Ok(Self {
            http: http_client()?,
            base: api_base(base.as_ref())?,
            credential: None,
        })
    }
    /// The normalized `/api/v1/` base of this client.
    pub fn base_url(&self) -> &Url {
        &self.base
    }
    /// Sends through a caller-owned connection pool. A long-lived process (the
    /// local relay) keeps one pool so every request and refresh reuses an open
    /// TLS connection instead of paying a new handshake.
    #[cfg_attr(not(feature = "runtime"), allow(dead_code))]
    pub(crate) fn with_http_client(mut self, http: HttpClient) -> Self {
        self.http = http;
        self
    }
    /// A cheap unauthenticated request that opens, or keeps open, a pooled
    /// connection to this backend's origin.
    #[cfg_attr(not(feature = "runtime"), allow(dead_code))]
    pub(crate) async fn warm(&self) -> Result<()> {
        let url = self
            .base
            .join("/live")
            .map_err(|e| Error::Configuration(e.to_string()))?;
        checked(
            self.http
                .get(url)
                .timeout(Duration::from_secs(10))
                .send()
                .await?,
        )
        .await?;
        Ok(())
    }
    /// Authenticates as the signed-in account with its DM access token from Silicon
    /// Accounts (`aud = dm`).
    pub fn with_token(mut self, access_token: impl Into<String>) -> Self {
        self.credential = Some(Credential::Bearer(access_token.into()));
        self
    }
    /// Acts for one of your app's users with a User verification proof (`sap_…`) issued by
    /// Silicon Accounts for receiving app `dm`. DM accepts it on the routes its scopes cover
    /// (`dm.conversations.read`, `dm.messages.read`, `dm.messages.write`,
    /// `dm.receipts.write`, `dm.drafts.write`) from the apps it allows for each scope.
    pub fn with_proof(mut self, proof_token: impl Into<String>) -> Self {
        self.credential = Some(Credential::Proof(proof_token.into()));
        self
    }
    /// Removes any credential (discovery and contract reads need none).
    pub fn without_credentials(mut self) -> Self {
        self.credential = None;
        self
    }
    /// Does nothing: DM records no telemetry. Every request says `X-DM-Telemetry: off`.
    #[deprecated(since = "0.16.0", note = "DM records no telemetry; this does nothing")]
    pub fn with_telemetry(self, _enabled: bool) -> Self {
        self
    }
    /// Does nothing: the source only labelled telemetry, which DM no longer records.
    #[deprecated(since = "0.16.0", note = "DM records no telemetry; this does nothing")]
    pub fn with_source(self, _source: &'static str) -> Self {
        self
    }
    /// Does nothing and sends nothing: DM records no telemetry.
    #[deprecated(since = "0.16.0", note = "DM records no telemetry; this sends nothing")]
    pub async fn telemetry(&self, _event: &str, _success: bool, _duration_ms: u64) -> Result<()> {
        Ok(())
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
            .header("X-DM-Contract-Version", CONTRACT_VERSION)
            // DM records no telemetry; this keeps a service that predates the removal from
            // recording this request.
            .header("X-DM-Telemetry", "off");
        match &self.credential {
            Some(Credential::Bearer(token)) => request = request.bearer_auth(token),
            Some(Credential::Proof(proof)) => {
                request = request.header(reqwest::header::AUTHORIZATION, format!("Proof {proof}"));
            }
            None => {}
        }
        Ok(request)
    }
    fn anonymous(&self, method: Method, path: &str) -> Result<RequestBuilder> {
        self.clone().without_credentials().request(method, path)
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
    /// Sends a request whose body is already final (an explicit envelope, or raw audio), so it
    /// is not wrapped in another envelope.
    async fn send_exact(&self, builder: RequestBuilder) -> Result<reqwest::Response> {
        Ok(self.http.execute(builder.build()?).await?)
    }
    /// A JSON body in an envelope with an explicit `type`, for routes whose type is not implied
    /// by method and path (`receipts.read` and `receipts.delivered` share one route).
    fn typed<T: serde::Serialize + ?Sized>(
        builder: RequestBuilder,
        kind: &str,
        data: &T,
    ) -> Result<RequestBuilder> {
        let body = serde_json::to_vec(&Envelope::new(kind, data))?;
        Ok(builder
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body))
    }
    async fn typed_json<T: DeserializeOwned, D: serde::Serialize + ?Sized>(
        &self,
        builder: RequestBuilder,
        kind: &str,
        data: &D,
    ) -> Result<T> {
        let response = checked(self.send_exact(Self::typed(builder, kind, data)?).await?).await?;
        Ok(serde_json::from_slice::<Envelope<T>>(&response.bytes().await?)?.data)
    }
    /// Reads a WAV answer with what DM says about it.
    async fn audio(&self, builder: RequestBuilder) -> Result<SpeechAudio> {
        let mut response = checked(self.send_exact(builder).await?).await?;
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned)
        };
        let content_type = header("content-type").unwrap_or_default();
        let (voice, duration, cache, etag) = (
            header("x-dm-voice"),
            header("x-dm-duration-ms").and_then(|value| value.parse::<u64>().ok()),
            header("x-dm-cache"),
            header("etag"),
        );
        if !content_type.starts_with("audio/") {
            return Err(Error::UnexpectedResponse(format!(
                "DM answered `{content_type}` where speech audio (audio/wav) was expected"
            )));
        }
        let mut wav = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            if wav.len() + chunk.len() > MAX_SPEECH_AUDIO_BYTES {
                return Err(Error::UnexpectedResponse(format!(
                    "the speech audio is larger than {} MiB, more than DM renders",
                    MAX_SPEECH_AUDIO_BYTES / (1024 * 1024)
                )));
            }
            wav.extend_from_slice(&chunk);
        }
        let duration_milliseconds =
            duration.or_else(|| speech::wav_info(&wav).map(|info| info.duration_milliseconds));
        Ok(SpeechAudio {
            wav,
            content_type,
            voice,
            duration_milliseconds,
            cache,
            etag,
        })
    }
}

/// DM caps one render at 30 MB of audio; anything far larger is not speech from DM.
const MAX_SPEECH_AUDIO_BYTES: usize = 64 * 1024 * 1024;
/// A render takes 2 to 6 seconds; DM gives a long script up to 180 seconds.
const SPEECH_TIMEOUT: Duration = Duration::from_secs(200);
/// Up to 100 MiB of audio goes up, then the transcription comes back.
const TRANSCRIBE_TIMEOUT: Duration = Duration::from_secs(600);

/// One path segment for an account id (`c:ada`, `si:scout`) or uuid. Ids use only characters
/// that are literal in a path, so they travel readable, as conversation addresses do.
fn account_segment(account: &str) -> Result<String> {
    let account = account.trim();
    if account.is_empty()
        || account.len() > 255
        || !account
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_-.:".contains(&c))
    {
        return Err(Error::Configuration(format!(
            "`{account}` is not an account: expected an id (c:ada, si:scout) or a uuid"
        )));
    }
    Ok(account.to_owned())
}

impl Client {
    /// The voices this service speaks with, and how an account's default voice is chosen.
    /// Needs no credential.
    pub async fn voices(&self) -> Result<VoiceCatalog> {
        self.json(self.anonymous(Method::GET, "voices")?).await
    }
    /// An account's voice: its stored choice or its default (`account`: `c:`/`si:` id or uuid).
    pub async fn voice(&self, account: &str) -> Result<AccountVoice> {
        self.json(self.request(
            Method::GET,
            &format!("voices/{}", account_segment(account)?),
        )?)
        .await
    }
    /// Chooses an account's voice; `voice` is a key from [`Client::voices`]. The account
    /// itself may, and so may the custodian of a Silicon; anyone else gets 403
    /// `voice_change_forbidden`, an unknown key 422 `voice_not_found`.
    pub async fn set_voice(&self, account: &str, voice: &str, key: &str) -> Result<AccountVoice> {
        self.typed_json(
            self.request(
                Method::PUT,
                &format!("voices/{}", account_segment(account)?),
            )?
            .header("Idempotency-Key", key),
            "voice.set",
            &json!({"voice": voice}),
        )
        .await
    }
    /// Returns an account to its default voice (who may: as for [`Client::set_voice`]).
    pub async fn reset_voice(&self, account: &str) -> Result<AccountVoice> {
        self.json(self.request(
            Method::DELETE,
            &format!("voices/{}", account_segment(account)?),
        )?)
        .await
    }
    /// Renders a script as WAV in a voice (`None`: the signed-in account's own). For previews
    /// and checking how a script sounds; it counts against the account's speech limits unless
    /// DM already has it cached.
    pub async fn render_speech(&self, script: &str, voice: Option<&str>) -> Result<SpeechAudio> {
        let mut data = json!({"script": script});
        if let Some(voice) = voice {
            data["voice"] = json!(voice);
        }
        self.audio(Self::typed(
            self.request(Method::POST, "speech")?
                .timeout(SPEECH_TIMEOUT),
            "speech.render",
            &data,
        )?)
        .await
    }
    /// A message's speech as WAV: a spoken message in its pinned voice, or any other message's
    /// text read in its sender's current voice. `for_silicon`: a custodian listening in one of
    /// its Silicons' conversations. 404 `nothing_to_speak` when there are no words, 410
    /// `message_deleted` for a tombstone.
    pub async fn message_speech(
        &self,
        conversation: impl std::fmt::Display,
        message: impl std::fmt::Display,
        for_silicon: Option<&str>,
    ) -> Result<SpeechAudio> {
        let message = message.to_string();
        validate_message_reference(&message)?;
        let conversation = conversation.to_string();
        validate_conversation_id(&conversation)?;
        self.audio(
            self.request(
                Method::GET,
                &format!("conversations/{conversation}/messages/{message}/speech"),
            )?
            .query(&for_query(for_silicon))
            .timeout(SPEECH_TIMEOUT),
        )
        .await
    }
    /// Transcribes audio. `content_type` is one of [`speech::TRANSCRIPTION_TYPES`]; `language`
    /// is a hint such as `en` (without one the language is detected). Over the service's size
    /// limit: 413 `audio_too_large`; another type: 415 `unsupported_audio_type`.
    pub async fn transcribe(
        &self,
        audio: Vec<u8>,
        content_type: &str,
        language: Option<&str>,
    ) -> Result<Transcription> {
        let mut builder = self
            .request(Method::POST, "speech/transcriptions")?
            .header(reqwest::header::CONTENT_TYPE, content_type)
            .timeout(TRANSCRIBE_TIMEOUT)
            .body(audio);
        if let Some(language) = language {
            builder = builder.query(&[("language", language)]);
        }
        let response = checked(self.send_exact(builder).await?).await?;
        Ok(serde_json::from_slice::<Envelope<Transcription>>(&response.bytes().await?)?.data)
    }
    /// Marks every message from others up to `up_to` (a message code, inclusive) read, which
    /// implies delivered. Repeating it changes nothing. A custodian's view cannot mark read
    /// (403 `read_only_view`).
    pub async fn read_up_to(
        &self,
        conversation: impl std::fmt::Display,
        up_to: &str,
    ) -> Result<ReceiptsUpdate> {
        self.receipts_up_to(conversation, up_to, ReceiptStatus::Read)
            .await
    }
    /// Marks every message from others up to `up_to` (inclusive) delivered or read.
    pub async fn receipts_up_to(
        &self,
        conversation: impl std::fmt::Display,
        up_to: &str,
        status: ReceiptStatus,
    ) -> Result<ReceiptsUpdate> {
        validate_message_reference(up_to)?;
        let conversation = conversation.to_string();
        validate_conversation_id(&conversation)?;
        let kind = match status {
            ReceiptStatus::Read => "receipts.read",
            ReceiptStatus::Delivered => "receipts.delivered",
        };
        self.typed_json(
            self.request(
                Method::POST,
                &format!("conversations/{conversation}/receipts"),
            )?,
            kind,
            &json!({"up_to": up_to}),
        )
        .await
    }
}
/// `?for=si:handle`: a custodian reading or managing one of its Silicons.
fn for_query(account: Option<&str>) -> Vec<(&'static str, &str)> {
    account
        .map(|account| vec![("for", account)])
        .unwrap_or_default()
}
impl Client {
    /// How to sign in to DM: its app id, the Silicon Accounts URL that issues its tokens,
    /// the accepted proof scopes and how updates are delivered. Needs no credential.
    pub async fn accounts(&self) -> Result<SignInDiscovery> {
        self.json(self.anonymous(Method::GET, "accounts")?).await
    }
    /// Discover supported contracts and the deprecation lifecycle.
    pub async fn contracts(&self) -> Result<Value> {
        self.json(self.anonymous(Method::GET, "contracts")?).await
    }
    /// The signed-in account, its custodian or the Silicons it looks after, and delivery.
    pub async fn me(&self) -> Result<Me> {
        self.json(self.request(Method::GET, "me")?).await
    }
    /// Whether updates reach the signed-in account through Ting, and how to sync without it.
    pub async fn delivery(&self) -> Result<Delivery> {
        self.json(self.request(Method::GET, "delivery")?).await
    }
    /// Durably submit a bug report. Reuse the idempotency key when retrying.
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
    /// A Silicon's allow-list: its own (`for_silicon = None`), or, for its custodian, the
    /// list of the Silicon named by `si:` id or uuid. Kept for older clients: since DM 0.17.1
    /// the list no longer limits who may start a conversation with the Silicon.
    pub async fn allow_list(&self, for_silicon: Option<&str>) -> Result<AllowList> {
        self.json(
            self.request(Method::GET, "allow-list")?
                .query(&for_query(for_silicon)),
        )
        .await
    }
    /// Put accounts (by `c:`/`si:` id or uuid) on the Silicon's allow-list. Kept for older
    /// clients: since DM 0.17.1 anyone may start a conversation with any Silicon and invite it
    /// to groups, so this changes nothing about who may reach it.
    pub async fn allow(
        &self,
        account_ids: &[String],
        for_silicon: Option<&str>,
    ) -> Result<AllowList> {
        self.json(
            self.request(Method::POST, "allow-list")?
                .query(&for_query(for_silicon))
                .json(&json!({"account_ids": account_ids})),
        )
        .await
    }
    /// Take accounts off the Silicon's allow-list. Kept for older clients: since DM 0.17.1 this
    /// changes nothing about who may reach the Silicon.
    pub async fn disallow(
        &self,
        account_ids: &[String],
        for_silicon: Option<&str>,
    ) -> Result<AllowList> {
        self.json(
            self.request(Method::DELETE, "allow-list")?
                .query(&for_query(for_silicon))
                .json(&json!({"account_ids": account_ids})),
        )
        .await
    }
    /// Event references since `cursor`. With `wait`, an empty page is held open up to that
    /// many seconds and returns as soon as something new arrives.
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
        if request.wait.is_some_and(|wait| wait > 30) {
            return Err(Error::Configuration(
                "sync wait must be between 0 and 30 seconds".into(),
            ));
        }
        let mut builder = self.request(Method::GET, "sync")?.query(request);
        if let Some(wait) = request.wait.filter(|wait| *wait > 0) {
            // The service answers within `wait` seconds; leave room for the network.
            builder = builder.timeout(Duration::from_secs(u64::from(wait) + 30));
        }
        self.json(builder).await
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
        self.conversations_for(page, None).await
    }
    /// Conversations of the signed-in account, or, for a custodian, those of one of its
    /// Silicons (`for_silicon`, read-only).
    pub async fn conversations_for(
        &self,
        page: &PageRequest,
        for_silicon: Option<&str>,
    ) -> Result<Page<Conversation>> {
        self.json(
            self.request(Method::GET, "conversations")?
                .query(page)
                .query(&for_query(for_silicon)),
        )
        .await
    }
    /// One conversation's summary: its participants, last message, unread facts and
    /// `draft_version`. A custodian reads one of its Silicon's with `for_silicon` (the unread
    /// facts and draft are then that Silicon's).
    pub async fn conversation(
        &self,
        conversation: impl std::fmt::Display,
        for_silicon: Option<&str>,
    ) -> Result<Conversation> {
        let conversation = conversation.to_string();
        validate_conversation_id(&conversation)?;
        self.json(
            self.request(Method::GET, &format!("conversations/{conversation}"))?
                .query(&for_query(for_silicon)),
        )
        .await
    }
    /// Starts (or finds) the one conversation of an exact set of accounts. You are included.
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
    /// Lists groups accessible to this credential; group IDs are conversation IDs.
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
    /// Creates a group; you become its first owner and `member_ids` are invited.
    pub async fn create_group(&self, input: &GroupCreate, key: &str) -> Result<Conversation> {
        self.json(
            self.request(Method::POST, "groups")?
                .header("Idempotency-Key", key)
                .json(input),
        )
        .await
    }
    /// Renames or describes a group (owners). `version` comes from [`Client::group`].
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
    /// Invites accounts (owners). Each invitation must be accepted by its invitee.
    pub async fn invite_group_members(
        &self,
        id: impl std::fmt::Display,
        members: &[String],
        key: &str,
    ) -> Result<GroupDetails> {
        self.group_people(Method::POST, id, "members", members, key)
            .await
    }
    /// Removes members or withdraws invitations (owners; a custodian may remove its own
    /// Silicons).
    pub async fn remove_group_members(
        &self,
        id: impl std::fmt::Display,
        members: &[String],
        key: &str,
    ) -> Result<GroupDetails> {
        self.group_people(Method::DELETE, id, "members", members, key)
            .await
    }
    /// Makes members co-equal owners (owners).
    pub async fn add_group_owners(
        &self,
        id: impl std::fmt::Display,
        members: &[String],
        key: &str,
    ) -> Result<GroupDetails> {
        self.group_people(Method::POST, id, "owners", members, key)
            .await
    }
    /// Stops members being owners (owners); a group always keeps one (409 `last_owner`).
    pub async fn remove_group_owners(
        &self,
        id: impl std::fmt::Display,
        members: &[String],
        key: &str,
    ) -> Result<GroupDetails> {
        self.group_people(Method::DELETE, id, "owners", members, key)
            .await
    }
    async fn group_people(
        &self,
        method: Method,
        id: impl std::fmt::Display,
        which: &str,
        members: &[String],
        key: &str,
    ) -> Result<GroupDetails> {
        let id = id.to_string();
        validate_conversation_id(&id)?;
        self.json(
            self.request(method, &format!("groups/{id}/{which}"))?
                .header("Idempotency-Key", key)
                .json(&json!({"member_ids":members})),
        )
        .await
    }
    /// Group invitations waiting for you, or, for a custodian, for one of its Silicons.
    pub async fn invitations(&self, for_silicon: Option<&str>) -> Result<Invitations> {
        self.json(
            self.request(Method::GET, "invitations")?
                .query(&for_query(for_silicon)),
        )
        .await
    }
    /// Accepts your invitation and joins the group (you see its whole history).
    pub async fn accept_invitation(&self, id: impl std::fmt::Display) -> Result<Conversation> {
        let id = id.to_string();
        validate_conversation_id(&id)?;
        self.json(
            self.request(Method::POST, &format!("groups/{id}/accept"))?
                .json(&json!({})),
        )
        .await
    }
    /// Declines your invitation (a custodian may decline for its Silicon with `for_silicon`).
    pub async fn decline_invitation(
        &self,
        id: impl std::fmt::Display,
        for_silicon: Option<&str>,
    ) -> Result<()> {
        let id = id.to_string();
        validate_conversation_id(&id)?;
        self.empty(
            self.request(Method::POST, &format!("groups/{id}/decline"))?
                .query(&for_query(for_silicon))
                .json(&json!({})),
        )
        .await
    }
    /// Leaves a group; the last owner must hand ownership on first (409 `last_owner`).
    pub async fn leave_group(&self, id: impl std::fmt::Display) -> Result<()> {
        let id = id.to_string();
        validate_conversation_id(&id)?;
        self.empty(
            self.request(Method::POST, &format!("groups/{id}/leave"))?
                .json(&json!({})),
        )
        .await
    }
}
fn validate_message_reference(message: &str) -> Result<()> {
    if Uuid::parse_str(message).is_err() && silicon_dm_protocol::message_sequence(message).is_none()
    {
        return Err(Error::Configuration(
            "expected a conversation-local message code (e.g. 00a) or a legacy UUID".into(),
        ));
    }
    Ok(())
}
impl Client {
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
    /// Sends into a conversation. `conversation` may also be an account id (`c:ada`,
    /// `si:scout`): the direct conversation with it is used, or created on first contact.
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
    /// `POST /messages`: sends to `message.recipient_id` (an account, a group or a conversation
    /// address) without looking the conversation up first.
    pub async fn send_to_recipient(&self, message: &MessageCreate, key: &str) -> Result<Message> {
        if message
            .recipient_id
            .as_deref()
            .is_none_or(|recipient| recipient.trim().is_empty())
        {
            return Err(Error::Configuration(
                "send_to_recipient needs recipient_id: an account id, group id or conversation address".into(),
            ));
        }
        self.json(
            self.request(Method::POST, "messages")?
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
        validate_message_reference(&message)?;
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
        validate_message_reference(&message)?;
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
        validate_message_reference(&message)?;
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
        validate_message_reference(&message)?;
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
        if silicon_dm_protocol::bundle_sequence(bundle).is_none()
            && Uuid::parse_str(bundle).is_err()
        {
            return Err(Error::Configuration(
                "expected a conversation-local bundle code such as 001 (or a legacy UUID)".into(),
            ));
        }
        self.json(self.request(
            Method::GET,
            &format!("conversations/{conversation}/bundles/{bundle}"),
        )?)
        .await
    }
    /// Availability, activity and last-seen time of an account (`c:`/`si:` id or uuid). Its
    /// activity here is about no conversation in particular, or about one you are in; see
    /// [`Client::presence_in`] for one conversation's.
    pub async fn presence(&self, account: &str) -> Result<Presence> {
        self.presence_in(account, None, None).await
    }
    /// An account's presence with its activity about `conversation` only (an address or
    /// UUID; none when you are not in it), and, with `for_silicon`, as a Silicon you look
    /// after sees it (DM 0.17, `features.presence_activity` in the contracts).
    pub async fn presence_in(
        &self,
        account: &str,
        conversation: Option<&str>,
        for_silicon: Option<&str>,
    ) -> Result<Presence> {
        let encoded: String = url::form_urlencoded::byte_serialize(account.as_bytes()).collect();
        let mut query: Vec<(&str, &str)> = Vec::new();
        if let Some(conversation) = conversation {
            query.push(("conversation", conversation));
        }
        if let Some(silicon) = for_silicon {
            query.push(("for", silicon));
        }
        let request = self.request(Method::GET, &format!("presence/{encoded}"))?;
        self.json(if query.is_empty() {
            request
        } else {
            request.query(&query)
        })
        .await
    }
    pub async fn renew_presence(
        &self,
        device_id: &str,
        activity: Option<Activity>,
    ) -> Result<PresenceLease> {
        self.renew_presence_in(device_id, activity, None).await
    }
    /// Renews this device's lease with what it is doing and the conversation that is about
    /// (an address or UUID of one you are in): only accounts in it see the activity. Services
    /// before DM 0.17 refuse `conversation`; they announce it as
    /// `features.presence_activity` in [`Client::contracts`].
    pub async fn renew_presence_in(
        &self,
        device_id: &str,
        activity: Option<Activity>,
        conversation: Option<&str>,
    ) -> Result<PresenceLease> {
        let path = presence_device_path(device_id)?;
        let body = match conversation {
            Some(conversation) => json!({"activity": activity, "conversation_id": conversation}),
            None => json!({"activity": activity}),
        };
        self.json(self.request(Method::PUT, &path)?.json(&body))
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
}
pub enum GifList<'a> {
    Trending,
    Search(&'a str),
    Recent,
}

/// The HTTP settings every DM request uses. Idle connections stay pooled for
/// five minutes with TCP keepalive so a relay that sends rarely still finds a
/// warm connection; the relay's own pings keep it inside load-balancer idle limits.
pub(crate) fn http_client() -> Result<HttpClient> {
    Ok(HttpClient::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(45))
        .pool_idle_timeout(Duration::from_secs(300))
        .tcp_keepalive(Duration::from_secs(30))
        .tcp_nodelay(true)
        .user_agent(concat!("silicon-dm-client/", env!("CARGO_PKG_VERSION")))
        .build()?)
}
/// Normalizes a DM origin or `/api/v1` base to the `/api/v1/` base every route joins.
pub fn api_base(base: &str) -> Result<Url> {
    let mut base = Url::parse(base.trim()).map_err(|e| {
        Error::Configuration(format!(
            "the DM API URL `{}` is not an absolute URL ({e}); use e.g. {DEFAULT_API_URL}",
            base.trim()
        ))
    })?;
    validate_endpoint(&base)?;
    if base.query().is_some() || base.fragment().is_some() {
        return Err(Error::Configuration(
            "the DM API URL must not have a query or fragment".into(),
        ));
    }
    let path = base.path().trim_end_matches('/');
    let path = if path.is_empty() {
        "/api/v1/".to_owned()
    } else {
        format!("{path}/")
    };
    base.set_path(&path);
    Ok(base)
}
/// Whether a URL names this machine: `localhost`, a `*.localhost` name or a loopback address.
pub fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(domain)) => domain == "localhost" || domain.ends_with(".localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}
/// HTTPS everywhere; plain HTTP only for this machine (local development).
pub fn validate_endpoint(url: &Url) -> Result<()> {
    if url.scheme() != "https" && !(url.scheme() == "http" && is_loopback(url)) {
        return Err(Error::Configuration(format!(
            "`{url}` must use https (plain http is only allowed for this machine: localhost or 127.0.0.1)"
        )));
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
    let hint = body
        .pointer("/error/hint")
        .or_else(|| body.pointer("/error/recovery"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    Err(Error::Api {
        status,
        code,
        message,
        hint,
        body: Box::new(body),
        request_id,
        retry_after,
    })
}

pub(crate) fn validate_conversation_id(value: &str) -> Result<()> {
    if !value.is_empty()
        && value.len() <= 515
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_-.:@+".contains(&c))
    {
        Ok(())
    } else {
        Err(Error::Configuration(
            "expected an account id (c:ada, si:scout), a direct conversation address (c:ada::si:scout), a group id (g:name) or a conversation UUID".into(),
        ))
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_are_https_or_this_machine() {
        for good in [
            "https://api.dm.teamofsilicons.com",
            "http://127.0.0.1:4121",
            "http://localhost:4121/api/v1",
            "http://dm.localhost:19780",
            "http://[::1]:4121",
        ] {
            assert!(api_base(good).is_ok(), "{good}");
        }
        for bad in [
            "http://api.dm.teamofsilicons.com",
            "http://localhost.example.com",
            "ftp://127.0.0.1",
            "https://user:secret@api.dm.teamofsilicons.com",
            "https://api.dm.teamofsilicons.com/?x=1",
            "not a url",
        ] {
            assert!(api_base(bad).is_err(), "{bad}");
        }
        assert_eq!(
            api_base("https://api.dm.teamofsilicons.com")
                .map(|u| u.to_string())
                .ok(),
            Some("https://api.dm.teamofsilicons.com/api/v1/".to_owned())
        );
    }

    #[test]
    fn errors_expose_code_message_and_hint() {
        let error = Error::Api {
            status: 403,
            code: "custodian_read_only".into(),
            message: "You can read this conversation as a custodian but only its participants send to it.".into(),
            hint: Some("To write, message si:scout directly in a conversation you are in.".into()),
            body: Box::new(json!({})),
            request_id: None,
            retry_after: None,
        };
        assert_eq!(error.code(), "custodian_read_only");
        assert!(error.is_code("custodian_read_only"));
        assert_eq!(error.status(), Some(403));
        assert!(
            error
                .hint()
                .is_some_and(|hint| hint.contains("message si:scout directly"))
        );
        assert!(!error.retryable());
        assert_eq!(
            Error::Configuration("x".into()).code(),
            "configuration_error"
        );
    }

    #[test]
    fn proof_and_bearer_credentials_are_never_debug_printed() -> Result<()> {
        let bearer = Client::new("http://127.0.0.1:1")?.with_token("secret-access-token");
        let proof = Client::new("http://127.0.0.1:1")?.with_proof("sap_secret");
        assert!(!format!("{bearer:?}").contains("secret"));
        assert!(!format!("{proof:?}").contains("sap_secret"));
        assert!(format!("{proof:?}").contains("proof"));
        Ok(())
    }
}
