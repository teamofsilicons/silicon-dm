//! External service interfaces used by application workflows. No SDK type crosses them.

use async_trait::async_trait;
use secrecy::SecretString;
use time::OffsetDateTime;

use crate::{
    AppResult,
    application::auth::AuthContext,
    domain::{ActorType, Gif},
};

/// Credential material presented with a request.
pub enum PresentedCredential<'a> {
    /// `Authorization: Bearer <Silicon Accounts access token>`.
    Bearer(&'a SecretString),
    /// `Authorization: Proof <sap_… User verification proof>` from another app.
    Proof(&'a SecretString),
}

/// What one route requires of the credential.
#[derive(Clone, Copy, Debug, Default)]
pub struct AuthRequirement {
    /// The proof scope that lets another app call this route for an account; `None` means
    /// only the account's own access token is accepted.
    pub proof_scope: Option<&'static str>,
    /// Check the token with Silicon Accounts (introspection, cached ≤ 30 s) instead of only
    /// verifying it locally, so a sign-out is seen at once (sensitive routes).
    pub live: bool,
}

/// What an account shares with DM by signing in to it (DM's user base). Lookups never
/// carry a name or photo; an account that removed DM's access has no profile here.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MemberProfile {
    /// Display name, when set.
    pub display_name: Option<String>,
    /// Photo URL, when set.
    pub pfp_url: Option<String>,
}

/// An account as Silicon Accounts describes it to DM (lookups and token claims).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AccountLookup {
    /// Permanent uuid.
    pub uuid: String,
    /// Carbon or Silicon.
    pub kind: ActorType,
    /// Current public id; `None` when deleted.
    pub id: Option<String>,
    /// Display name, when shared with DM.
    pub display_name: Option<String>,
    /// Photo URL, when shared with DM.
    pub pfp_url: Option<String>,
    /// A Silicon's custodian: `(uuid, current id)`.
    pub custodian: Option<(String, String)>,
    /// The account was deleted.
    pub deleted: bool,
}

/// A verified Silicon Accounts webhook delivery.
#[derive(Clone, Debug)]
pub struct VerifiedEvent {
    /// Dedupe key: the same on every retry and replay.
    pub event_id: String,
    /// The event type as sent.
    pub event_type: String,
    /// When the change happened.
    pub occurred_at: Option<OffsetDateTime>,
    /// What changed.
    pub event: AccountEvent,
}

/// The app events DM acts on.
#[derive(Clone, Debug)]
pub enum AccountEvent {
    /// `account.id_changed`.
    IdChanged {
        /// Account uuid.
        uuid: String,
        /// Carbon or Silicon, when sent.
        kind: Option<ActorType>,
        /// The new public id.
        new_id: String,
    },
    /// `account.updated` (profile as DM may see it now).
    Updated {
        /// Account uuid.
        uuid: String,
        /// The account after the change, when sent.
        account: Option<AccountLookup>,
        /// Monotonic profile version.
        version: i64,
    },
    /// `account.deleted`.
    Deleted {
        /// Account uuid.
        uuid: String,
    },
    /// `membership.signed_out`.
    SignedOut {
        /// Account uuid.
        uuid: String,
        /// Why (`app_revoked`, `stk_rotated`, …).
        reason: Option<String>,
    },
    /// `membership.access_removed`.
    AccessRemoved {
        /// Account uuid.
        uuid: String,
    },
    /// `silicon.custodian_changed`.
    CustodianChanged {
        /// The Silicon's uuid.
        uuid: String,
        /// The new custodian `(uuid, id)`.
        to: Option<(String, String)>,
    },
    /// `ping` (a test delivery).
    Ping,
    /// A type DM does not act on.
    Unknown,
}

/// A proof DM obtained from Silicon Accounts (for Ting).
pub struct IssuedProof {
    /// Proof family id (revoke with it).
    pub proof_id: String,
    /// `sap_…`: sent to the receiving app.
    pub token: SecretString,
    /// When `token` expires.
    pub expires_at: Option<OffsetDateTime>,
    /// `sapr_…`: rotates on every use; kept encrypted when DM needs the proof later.
    pub refresh_token: Option<SecretString>,
    /// When the refresh token stops working.
    pub refresh_expires_at: Option<OffsetDateTime>,
}

/// Identity, lookups, proofs and webhooks: everything DM asks of Silicon Accounts.
#[async_trait]
pub trait IdentityProvider: Send + Sync {
    /// Verifies the credential for a route (the accounts cache and sign-out cutoffs are the
    /// application's job, see [`crate::application::accounts`]).
    async fn authenticate(
        &self,
        credential: PresentedCredential<'_>,
        requirement: AuthRequirement,
    ) -> AppResult<AuthContext>;

    /// Whether the sign-in behind `token` (an access token of `uuid`) is still live at
    /// Silicon Accounts. Asked when the token's `iat` (whole seconds) falls in the same
    /// second as the account's sign-out `cutoff`, so `iat` cannot tell which came first.
    /// The answer for one token and cutoff is kept for the token's lifetime.
    ///
    /// # Errors
    /// `dependency_unavailable` when Silicon Accounts cannot be asked.
    async fn sign_in_live(
        &self,
        token: &SecretString,
        uuid: &str,
        cutoff: OffsetDateTime,
    ) -> AppResult<bool>;

    /// `GET /v1/accounts/{uuid}`.
    async fn lookup(&self, uuid: &str) -> AppResult<AccountLookup>;

    /// `GET /v1/accounts/by-id/{id}` (current ids only).
    async fn lookup_by_id(&self, id: &str) -> AppResult<AccountLookup>;

    /// `GET /v1/apps/{app_id}/users/{uuid}`: the profile a member shared with DM, or
    /// `None` for an account that never signed in to DM or removed its access.
    async fn member_profile(&self, uuid: &str) -> AppResult<Option<MemberProfile>>;

    /// Verifies a webhook delivery's signature over the raw body and parses it.
    ///
    /// # Errors
    /// 401 for a bad or stale signature, 400 for a body that is not an event.
    fn verify_webhook(&self, headers: &http::HeaderMap, body: &[u8]) -> AppResult<VerifiedEvent>;

    /// A User verification proof for `receiving_app`, for the account behind
    /// `subject_token` (its current DM access token).
    async fn issue_user_verification(
        &self,
        subject_token: &SecretString,
        receiving_app: &str,
        scopes: &[&str],
        idempotency_key: &str,
    ) -> AppResult<IssuedProof>;

    /// An App verification proof (DM itself, no account) for `receiving_app`.
    async fn issue_app_verification(
        &self,
        receiving_app: &str,
        scopes: &[&str],
        idempotency_key: &str,
    ) -> AppResult<IssuedProof>;

    /// Exchanges a proof refresh token for a new proof (the refresh token rotates).
    async fn refresh_proof(&self, refresh_token: &SecretString) -> AppResult<IssuedProof>;

    /// Revokes a proof DM issued.
    async fn revoke_proof(&self, proof_id: &str) -> AppResult<()>;
}

/// Giphy operations required by DM.
#[async_trait]
pub trait GifProvider: Send + Sync {
    /// Returns current safe trending results.
    async fn trending(&self) -> AppResult<Vec<Gif>>;

    /// Searches safe provider results.
    async fn search(&self, query: &str) -> AppResult<Vec<Gif>>;
}

/// What to render: a script in one provider voice with one model.
#[derive(Clone, Copy, Debug)]
pub struct SynthesisRequest<'a> {
    /// Words and square-bracket cues.
    pub script: &'a str,
    /// The provider's voice id.
    pub voice_id: &'a str,
    /// The speech model (`eleven_v4`).
    pub model: &'a str,
}

/// Rendered speech: raw 24 kHz mono 16-bit little-endian PCM.
#[derive(Clone, Debug)]
pub struct SynthesizedSpeech {
    /// The PCM samples.
    pub pcm: Vec<u8>,
    /// Time from connecting to the first audio.
    pub first_audio_after: std::time::Duration,
}

/// Audio to transcribe, streamed as it arrives.
pub struct TranscriptionRequest {
    /// The audio bytes (the request body), never buffered whole.
    pub audio: futures::stream::BoxStream<'static, Result<bytes::Bytes, std::io::Error>>,
    /// The audio's media type (`audio/webm`, `audio/wav`, …).
    pub content_type: String,
    /// A language hint (`en`, `pt-BR`); `None` lets the provider detect the language.
    pub language: Option<String>,
}

/// What was said.
#[derive(Clone, Debug, PartialEq)]
pub struct Transcription {
    /// The words, punctuated; empty when nothing was said.
    pub transcript: String,
    /// The language: the hint, or the one detected.
    pub language: Option<String>,
    /// Length of the audio.
    pub duration_milliseconds: Option<u64>,
    /// The provider's confidence, 0 to 1.
    pub confidence: Option<f64>,
}

/// Text to speech and speech to text (Deepgram). Errors use DM's codes:
/// `speech_rejected` (422, the provider refused the voice or text), `speech_unavailable`
/// (503 with `Retry-After`), `audio_unreadable` (422) and `audio_too_large` (413).
#[async_trait]
pub trait SpeechProvider: Send + Sync {
    /// Renders a script.
    async fn synthesize(&self, request: SynthesisRequest<'_>) -> AppResult<SynthesizedSpeech>;

    /// Transcribes audio.
    async fn transcribe(&self, request: TranscriptionRequest) -> AppResult<Transcription>;
}
