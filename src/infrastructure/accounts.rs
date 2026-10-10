//! Silicon Accounts through the official `silicon-accounts-client`: local verification of
//! access tokens against the cached JWKS, introspection on sensitive routes, inbound
//! User verification proofs, account lookups (rate limited), proofs DM issues for Ting,
//! and webhook verification.

use std::{
    collections::{BTreeSet, HashMap},
    sync::Arc,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use secrecy::{ExposeSecret as _, SecretString};
use silicon_accounts_client::{
    AccountKind, AccountSummary, AccountsClient, IssueAppVerification, IssueUserVerification, Jwks,
    ProofKind, ProofRef, ProofVerification, SIGNATURE_HEADER, TIMESTAMP_HEADER, TokenError,
    VerifyOptions, WebhookError, WebhookPayload, verify_access_token, verify_and_parse_webhook,
};
use time::OffsetDateTime;
use tokio::sync::{Mutex, RwLock};

use crate::{
    AppError, AppResult,
    application::{
        auth::{AuthContext, Credential},
        ports::{
            AccountEvent, AccountLookup, AuthRequirement, IdentityProvider, IssuedProof,
            MemberProfile, PresentedCredential, VerifiedEvent,
        },
    },
    config::{AccountsSettings, ProofIssuers, is_loopback},
    domain::{ActorId, ActorRef, ActorType},
};

/// Minimum time between two JWKS downloads triggered by unknown key ids.
const JWKS_REFETCH_INTERVAL: Duration = Duration::from_secs(30);
/// How long a live answer (introspection or proof verification) is reused. Routes that
/// check the sign-in live reuse only negative answers (an ended sign-in never comes back).
const LIVE_CACHE: Duration = Duration::from_secs(30);
/// How long the answer for a token issued in the same second as a sign-out cutoff is kept
/// (the longest access token lifetime; the cutoff is part of the key).
const CUTOFF_DECISIONS: Duration = Duration::from_mins(30);
/// Lookups DM allows itself per minute (Accounts allows 600 per app).
const LOOKUPS_PER_MINUTE: u32 = 550;
const MAX_CREDENTIAL_BYTES: usize = 8192;

/// One live verdict cached for a short time.
#[derive(Clone)]
enum LiveVerdict {
    Inactive,
    Proof(Box<ProofGrant>),
}

#[derive(Clone)]
struct ProofGrant {
    actor: ActorRef,
    public_id: String,
    membership_id: Option<String>,
    issuing_app: String,
    scopes: BTreeSet<String>,
    expires_at: OffsetDateTime,
}

/// Token bucket for account lookups.
struct LookupBudget {
    tokens: f64,
    refreshed: Instant,
}

/// The Silicon Accounts adapter (one per process; cheap to share).
#[derive(Clone)]
pub struct AccountsAdapter {
    inner: Arc<Inner>,
}

struct Inner {
    client: AccountsClient,
    app_id: String,
    app_secret: SecretString,
    issuer: String,
    webhook_secret: SecretString,
    proof_issuers: ProofIssuers,
    jwks: RwLock<Option<Arc<Jwks>>>,
    jwks_fetched: Mutex<Option<Instant>>,
    live: Mutex<HashMap<[u8; 32], (Instant, LiveVerdict)>>,
    cutoff_decisions: Mutex<HashMap<[u8; 32], (Instant, bool)>>,
    budget: Mutex<LookupBudget>,
}

impl AccountsAdapter {
    /// Builds the adapter from validated settings.
    ///
    /// # Errors
    /// Rejects an Accounts URL the client cannot use.
    pub fn new(settings: &AccountsSettings) -> AppResult<Self> {
        let client = AccountsClient::builder()
            .base_url(settings.api_url.as_str().trim_end_matches('/'))
            .allow_insecure_http(is_loopback(&settings.api_url))
            .timeout(settings.request_timeout)
            .connect_timeout(settings.request_timeout)
            .user_agent(format!("silicon-dm/{}", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| {
                AppError::validation(format!("ACCOUNTS_API_URL cannot be used: {error}"))
            })?;
        Ok(Self {
            inner: Arc::new(Inner {
                client,
                app_id: settings.app_id.clone(),
                app_secret: settings.app_secret.clone(),
                issuer: settings.issuer.clone(),
                webhook_secret: settings.webhook_secret.clone(),
                proof_issuers: settings.proof_issuers.clone(),
                jwks: RwLock::new(None),
                jwks_fetched: Mutex::new(None),
                live: Mutex::new(HashMap::new()),
                cutoff_decisions: Mutex::new(HashMap::new()),
                budget: Mutex::new(LookupBudget {
                    tokens: f64::from(LOOKUPS_PER_MINUTE),
                    refreshed: Instant::now(),
                }),
            }),
        })
    }

    /// Preloads the JWKS (tests, or a deployment that ships the key set).
    pub async fn preload_jwks(&self, jwks: Jwks) {
        *self.inner.jwks.write().await = Some(Arc::new(jwks));
    }

    /// The app id DM verifies tokens and proofs for.
    #[must_use]
    pub fn app_id(&self) -> &str {
        &self.inner.app_id
    }

    /// The cached key set. `refetch` (a token named a key the set lacks) downloads it again,
    /// at most once per 30 seconds so forged key ids cannot make DM hammer Accounts; the
    /// first download does not count against that limit.
    async fn jwks(&self, refetch: bool) -> AppResult<Arc<Jwks>> {
        if !refetch && let Some(jwks) = self.inner.jwks.read().await.clone() {
            return Ok(jwks);
        }
        let mut last_refetch = self.inner.jwks_fetched.lock().await;
        let cached = self.inner.jwks.read().await.clone();
        if let Some(jwks) = &cached
            && (!refetch || last_refetch.is_some_and(|at| at.elapsed() < JWKS_REFETCH_INTERVAL))
        {
            return Ok(jwks.clone());
        }
        let jwks = Arc::new(self.inner.client.jwks().await.map_err(|error| {
            tracing::warn!(
                dependency = "accounts",
                code = error.code(),
                "JWKS download failed"
            );
            AppError::DependencyUnavailable {
                dependency: "accounts",
            }
        })?);
        if refetch {
            *last_refetch = Some(Instant::now());
        }
        *self.inner.jwks.write().await = Some(jwks.clone());
        Ok(jwks)
    }

    async fn verify_locally(&self, token: &str) -> AppResult<silicon_accounts_client::Claims> {
        let options = VerifyOptions::for_app(&self.inner.app_id).with_issuer(&self.inner.issuer);
        let jwks = self.jwks(false).await?;
        match verify_access_token(&jwks, token, &options) {
            Err(silicon_accounts_client::Error::Token(TokenError::UnknownKey { .. })) => {
                let jwks = self.jwks(true).await?;
                verify_access_token(&jwks, token, &options)
                    .map_err(|error| self.token_error(&error))
            }
            other => other.map_err(|error| self.token_error(&error)),
        }
    }

    fn token_error(&self, error: &silicon_accounts_client::Error) -> AppError {
        let message = match error {
            silicon_accounts_client::Error::Token(TokenError::Expired { exp }) => {
                let when = exp
                    .and_then(|exp| OffsetDateTime::from_unix_timestamp(exp).ok())
                    .and_then(|at| at.format(&time::format_description::well_known::Rfc3339).ok())
                    .map_or_else(String::new, |at| format!(" at {at}"));
                return AppError::unauthenticated(
                    "token_expired",
                    format!("The access token expired{when}. Refresh it with the refresh token and retry (the dm CLI and website refresh automatically)."),
                );
            }
            silicon_accounts_client::Error::Token(TokenError::WrongAudience { .. }) => format!(
                "The access token was issued to another app. DM accepts only Silicon Accounts tokens issued to `{}` (aud); sign in to DM to get one.",
                self.inner.app_id
            ),
            silicon_accounts_client::Error::Token(TokenError::WrongIssuer { .. }) => format!(
                "The access token was issued by another Silicon Accounts deployment; this DM trusts {}.",
                self.inner.issuer
            ),
            silicon_accounts_client::Error::Token(TokenError::NotYetValid) => {
                "The access token is not valid yet (nbf is in the future); check this machine's clock.".to_owned()
            }
            silicon_accounts_client::Error::Token(TokenError::UnknownKey { kid }) => format!(
                "The access token was signed with a key ({}) that {} does not publish.",
                kid.as_deref().unwrap_or("no kid"),
                self.inner.issuer
            ),
            other => format!(
                "The bearer credential is not a Silicon Accounts access token for DM ({}).",
                other.code()
            ),
        };
        AppError::unauthenticated("invalid_token", message)
    }

    async fn cached_live(&self, key: &[u8; 32]) -> Option<LiveVerdict> {
        let mut live = self.inner.live.lock().await;
        if live.len() > 10_000 {
            live.retain(|_, (at, _)| at.elapsed() < LIVE_CACHE);
        }
        live.get(key)
            .filter(|(at, _)| at.elapsed() < LIVE_CACHE)
            .map(|(_, verdict)| verdict.clone())
    }

    async fn remember_live(&self, key: [u8; 32], verdict: LiveVerdict) {
        self.inner
            .live
            .lock()
            .await
            .insert(key, (Instant::now(), verdict));
    }

    async fn authenticate_bearer(
        &self,
        token: &SecretString,
        requirement: AuthRequirement,
    ) -> AppResult<AuthContext> {
        let raw = token.expose_secret();
        let claims = self.verify_locally(raw).await?;
        let kind = match claims.kind {
            Some(AccountKind::Carbon) => ActorType::Carbon,
            Some(AccountKind::Silicon) => ActorType::Silicon,
            _ => {
                return Err(AppError::unauthenticated(
                    "invalid_token",
                    "The access token names no account kind (carbon or silicon).",
                ));
            }
        };
        if !crate::domain::is_account_uuid(&claims.sub) {
            return Err(AppError::unauthenticated(
                "invalid_token",
                "The access token's subject is not a Silicon Accounts uuid.",
            ));
        }
        if requirement.live {
            // These routes must see a sign-out at once, so Silicon Accounts is asked every
            // time; only a negative answer is reused (an ended sign-in never comes back).
            let key = *blake3::hash(raw.as_bytes()).as_bytes();
            let active = if matches!(self.cached_live(&key).await, Some(LiveVerdict::Inactive)) {
                false
            } else {
                let active = self.introspect(raw, &claims.sub).await?;
                if !active {
                    self.remember_live(key, LiveVerdict::Inactive).await;
                }
                active
            };
            if !active {
                return Err(AppError::unauthenticated(
                    "session_revoked",
                    "Silicon Accounts says this sign-in has ended (signed out, access removed or account deleted). Sign in to DM again.",
                ));
            }
        }
        let expires_at = OffsetDateTime::from_unix_timestamp(claims.exp).map_err(|_| {
            AppError::unauthenticated("invalid_token", "The access token has an invalid exp.")
        })?;
        Ok(AuthContext {
            actor: ActorRef {
                actor_type: kind,
                id: ActorId::from_storage_key(claims.sub.clone()),
            },
            public_id: claims.id.clone().unwrap_or_else(|| claims.sub.clone()),
            membership_id: claims.mid.clone(),
            token_family: claims.fid.clone(),
            scopes: claims.scopes().into_iter().map(str::to_owned).collect(),
            credential: Credential::AccessToken(token.clone()),
            issued_at: claims
                .iat
                .and_then(|iat| OffsetDateTime::from_unix_timestamp(iat).ok()),
            expires_at,
        })
    }
}

impl AccountsAdapter {
    async fn authenticate_proof(
        &self,
        proof: &SecretString,
        requirement: AuthRequirement,
    ) -> AppResult<AuthContext> {
        let Some(scope) = requirement.proof_scope else {
            return Err(AppError::forbidden(
                "proof_not_accepted",
                "This route needs the account's own DM sign-in (Authorization: Bearer). Apps acting for an account with a User verification proof may only read and write conversations, messages, receipts and drafts.",
            ));
        };
        let raw = proof.expose_secret();
        let key = *blake3::hash(format!("proof:{raw}").as_bytes()).as_bytes();
        let grant = match self.cached_live(&key).await {
            // A route that checks live never rests on a cached positive answer.
            Some(LiveVerdict::Proof(grant))
                if !requirement.live && grant.expires_at > OffsetDateTime::now_utc() =>
            {
                grant
            }
            Some(LiveVerdict::Inactive) => return Err(invalid_proof()),
            _ => self.verify_proof_live(raw, key).await?,
        };
        if !grant.scopes.contains(scope) {
            return Err(AppError::forbidden(
                "proof_scope_missing",
                format!(
                    "The proof from {} does not carry the scope {scope} this route needs.",
                    grant.issuing_app
                ),
            ));
        }
        if !self.inner.proof_issuers.allows(scope, &grant.issuing_app) {
            return Err(AppError::forbidden(
                "proof_issuer_not_allowed",
                format!(
                    "DM does not accept {scope} proofs from {}; its operator lists the apps it accepts in DM_PROOF_ISSUERS.",
                    grant.issuing_app
                ),
            ));
        }
        Ok(AuthContext {
            actor: grant.actor.clone(),
            public_id: grant.public_id.clone(),
            membership_id: grant.membership_id.clone(),
            token_family: None,
            scopes: grant.scopes.clone(),
            credential: Credential::Proof {
                issuing_app: grant.issuing_app.clone(),
                scopes: grant.scopes.clone(),
            },
            issued_at: None,
            expires_at: grant.expires_at,
        })
    }

    /// Asks Silicon Accounts about a proof (`POST /v1/proofs/verify` with DM's credentials)
    /// and remembers the answer under `key`: a User verification proof made for DM, for an
    /// account with a valid uuid, becomes a grant; anything else is refused.
    async fn verify_proof_live(&self, raw: &str, key: [u8; 32]) -> AppResult<Box<ProofGrant>> {
        let app = self
            .inner
            .client
            .as_app(&self.inner.app_id, self.inner.app_secret.expose_secret());
        let verification = app.verify_proof(raw).await.map_err(|error| {
            tracing::warn!(
                dependency = "accounts",
                code = error.code(),
                "proof verification failed"
            );
            AppError::DependencyUnavailable {
                dependency: "accounts",
            }
        })?;
        let ProofVerification::Valid(valid) = verification else {
            self.remember_live(key, LiveVerdict::Inactive).await;
            return Err(invalid_proof());
        };
        let (ProofKind::UserVerification, Some(user)) = (&valid.kind, &valid.user) else {
            return Err(AppError::forbidden(
                "proof_not_accepted",
                "DM accepts only User verification proofs (an app acting for one account), not App verification proofs.",
            ));
        };
        if valid.receiving_app.app_id != self.inner.app_id
            || !crate::domain::is_account_uuid(&user.uuid)
        {
            return Err(invalid_proof());
        }
        let kind = match user.kind {
            Some(AccountKind::Silicon) => ActorType::Silicon,
            Some(AccountKind::Carbon) => ActorType::Carbon,
            _ if user.id.starts_with("si:") => ActorType::Silicon,
            _ => ActorType::Carbon,
        };
        let grant = Box::new(ProofGrant {
            actor: ActorRef {
                actor_type: kind,
                id: ActorId::from_storage_key(user.uuid.clone()),
            },
            public_id: user.id.clone(),
            membership_id: user.membership_id.clone(),
            issuing_app: valid.issuing_app.app_id.clone(),
            scopes: valid.scopes.iter().cloned().collect(),
            expires_at: valid.expires_at,
        });
        self.remember_live(key, LiveVerdict::Proof(grant.clone()))
            .await;
        Ok(grant)
    }

    /// Asks Silicon Accounts whether the access token is still live for `sub`.
    async fn introspect(&self, raw: &str, sub: &str) -> AppResult<bool> {
        let app = self
            .inner
            .client
            .as_app(&self.inner.app_id, self.inner.app_secret.expose_secret());
        let introspection = app.introspect(raw).await.map_err(|error| {
            tracing::warn!(
                dependency = "accounts",
                code = error.code(),
                "introspection failed"
            );
            AppError::DependencyUnavailable {
                dependency: "accounts",
            }
        })?;
        Ok(introspection.active && introspection.sub.as_deref() == Some(sub))
    }

    async fn spend_lookup(&self) -> AppResult<()> {
        let mut budget = self.inner.budget.lock().await;
        let elapsed = budget.refreshed.elapsed().as_secs_f64();
        let limit = f64::from(LOOKUPS_PER_MINUTE);
        budget.tokens = (budget.tokens + elapsed * limit / 60.0).min(limit);
        budget.refreshed = Instant::now();
        if budget.tokens < 1.0 {
            return Err(AppError::refused(
                http::StatusCode::TOO_MANY_REQUESTS,
                "accounts_lookup_budget",
                "DM is resolving too many Silicon Accounts ids right now (its lookup budget is 550 a minute). Retry in a few seconds.",
            ));
        }
        budget.tokens -= 1.0;
        Ok(())
    }

    fn lookup_error(id: &str, error: &silicon_accounts_client::Error) -> AppError {
        if error.is_code("account_deleted") {
            AppError::account_deleted(id)
        } else if error.is_not_found() {
            AppError::account_not_found(id)
        } else if error.status() == Some(429) {
            AppError::RateLimited
        } else {
            tracing::warn!(
                dependency = "accounts",
                code = error.code(),
                "account lookup failed"
            );
            AppError::DependencyUnavailable {
                dependency: "accounts",
            }
        }
    }

    fn proof_error(error: &silicon_accounts_client::Error) -> AppError {
        tracing::warn!(
            dependency = "accounts",
            code = error.code(),
            "proof request failed"
        );
        if error
            .status()
            .is_some_and(|status| (400..500).contains(&status))
            && error.status() != Some(429)
        {
            AppError::refused(
                http::StatusCode::BAD_GATEWAY,
                "proof_refused",
                format!(
                    "Silicon Accounts refused DM's proof request ({}).",
                    error.code()
                ),
            )
        } else {
            AppError::DependencyUnavailable {
                dependency: "accounts",
            }
        }
    }
}

fn invalid_proof() -> AppError {
    AppError::unauthenticated(
        "invalid_proof",
        "Silicon Accounts says this proof is not valid for DM (unknown, expired, revoked, issued for another app, or for an account that no longer exists).",
    )
}

fn lookup_from_summary(summary: AccountSummary) -> AccountLookup {
    let deleted = summary.status == "deleted";
    AccountLookup {
        kind: match summary.kind {
            AccountKind::Silicon => ActorType::Silicon,
            AccountKind::Carbon => ActorType::Carbon,
        },
        id: Some(summary.id).filter(|id| !id.is_empty() && !deleted),
        display_name: Some(summary.display_name).filter(|name| !name.is_empty()),
        pfp_url: Some(summary.pfp_url).filter(|url| !url.is_empty()),
        custodian: summary
            .custodian
            .map(|custodian| (custodian.uuid, custodian.id)),
        uuid: summary.uuid,
        deleted,
    }
}

fn issued(proof: silicon_accounts_client::IssuedProof) -> IssuedProof {
    IssuedProof {
        proof_id: proof.proof_id,
        token: SecretString::from(proof.proof_token.expose().to_owned()),
        expires_at: proof.expires_at,
        refresh_token: proof
            .proof_refresh_token
            .map(|token| SecretString::from(token.expose().to_owned())),
        refresh_expires_at: proof.refresh_expires_at,
    }
}

fn kind_of(kind: Option<AccountKind>) -> Option<ActorType> {
    match kind {
        Some(AccountKind::Carbon) => Some(ActorType::Carbon),
        Some(AccountKind::Silicon) => Some(ActorType::Silicon),
        _ => None,
    }
}

#[async_trait]
impl IdentityProvider for AccountsAdapter {
    async fn authenticate(
        &self,
        credential: PresentedCredential<'_>,
        requirement: AuthRequirement,
    ) -> AppResult<AuthContext> {
        let (PresentedCredential::Bearer(token) | PresentedCredential::Proof(token)) = &credential;
        if token.expose_secret().is_empty() || token.expose_secret().len() > MAX_CREDENTIAL_BYTES {
            return Err(AppError::unauthenticated(
                "invalid_token",
                "The credential is empty or longer than 8192 bytes.",
            ));
        }
        match credential {
            PresentedCredential::Bearer(token) => {
                self.authenticate_bearer(token, requirement).await
            }
            PresentedCredential::Proof(proof) => self.authenticate_proof(proof, requirement).await,
        }
    }

    async fn sign_in_live(
        &self,
        token: &SecretString,
        uuid: &str,
        cutoff: OffsetDateTime,
    ) -> AppResult<bool> {
        let raw = token.expose_secret();
        let key =
            *blake3::hash(format!("cutoff:{}:{raw}", cutoff.unix_timestamp_nanos()).as_bytes())
                .as_bytes();
        {
            let mut decisions = self.inner.cutoff_decisions.lock().await;
            if decisions.len() > 10_000 {
                decisions.retain(|_, (at, _)| at.elapsed() < CUTOFF_DECISIONS);
            }
            if let Some((at, live)) = decisions.get(&key)
                && at.elapsed() < CUTOFF_DECISIONS
            {
                return Ok(*live);
            }
        }
        let live = self.introspect(raw, uuid).await?;
        self.inner
            .cutoff_decisions
            .lock()
            .await
            .insert(key, (Instant::now(), live));
        Ok(live)
    }

    async fn lookup(&self, uuid: &str) -> AppResult<AccountLookup> {
        self.spend_lookup().await?;
        let app = self
            .inner
            .client
            .as_app(&self.inner.app_id, self.inner.app_secret.expose_secret());
        app.lookup(uuid)
            .await
            .map(lookup_from_summary)
            .map_err(|error| Self::lookup_error(uuid, &error))
    }

    async fn lookup_by_id(&self, id: &str) -> AppResult<AccountLookup> {
        self.spend_lookup().await?;
        let app = self
            .inner
            .client
            .as_app(&self.inner.app_id, self.inner.app_secret.expose_secret());
        app.lookup_by_id(id)
            .await
            .map(lookup_from_summary)
            .map_err(|error| Self::lookup_error(id, &error))
    }

    async fn member_profile(&self, uuid: &str) -> AppResult<Option<MemberProfile>> {
        self.spend_lookup().await?;
        let app = self
            .inner
            .client
            .as_app(&self.inner.app_id, self.inner.app_secret.expose_secret());
        match app.user(uuid).await {
            Ok(user) if user.status == "active" => Ok(Some(MemberProfile {
                display_name: Some(user.display_name).filter(|name| !name.is_empty()),
                pfp_url: user.pfp_url.filter(|url| !url.is_empty()),
            })),
            Ok(_) => Ok(None),
            Err(error) if error.is_not_found() => Ok(None),
            Err(error) => Err(Self::lookup_error(uuid, &error)),
        }
    }

    fn verify_webhook(&self, headers: &http::HeaderMap, body: &[u8]) -> AppResult<VerifiedEvent> {
        let header = |name: &str| {
            headers
                .get(name)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
        };
        let event = verify_and_parse_webhook(
            self.inner.webhook_secret.expose_secret(),
            header(TIMESTAMP_HEADER),
            header(SIGNATURE_HEADER),
            body,
            silicon_accounts_client::DEFAULT_WEBHOOK_TOLERANCE,
        )
        .map_err(|error| match error {
            WebhookError::InvalidBody(reason) => AppError::refused(
                http::StatusCode::BAD_REQUEST,
                "invalid_webhook_body",
                format!("The webhook body is not a Silicon Accounts event: {reason}"),
            ),
            other => AppError::unauthenticated("invalid_signature", other.to_string()),
        })?;
        let account_event = match event.payload {
            WebhookPayload::AccountIdChanged(change) => AccountEvent::IdChanged {
                uuid: change.uuid,
                kind: kind_of(change.kind),
                new_id: change.new_id,
            },
            WebhookPayload::AccountUpdated(update) => AccountEvent::Updated {
                version: update.account.as_ref().map_or(0, |account| account.version),
                account: update.account.map(|account| AccountLookup {
                    kind: match account.kind {
                        AccountKind::Silicon => ActorType::Silicon,
                        AccountKind::Carbon => ActorType::Carbon,
                    },
                    id: Some(account.id).filter(|id| !id.is_empty()),
                    display_name: Some(account.display_name),
                    pfp_url: Some(account.pfp_url),
                    custodian: account
                        .custodian
                        .map(|custodian| (custodian.uuid, custodian.id)),
                    uuid: account.uuid,
                    deleted: false,
                }),
                uuid: update.uuid,
            },
            WebhookPayload::AccountDeleted(gone) => AccountEvent::Deleted { uuid: gone.uuid },
            WebhookPayload::MembershipSignedOut(signed_out) => AccountEvent::SignedOut {
                uuid: signed_out.uuid,
                reason: signed_out.reason,
            },
            WebhookPayload::MembershipAccessRemoved(removed) => {
                AccountEvent::AccessRemoved { uuid: removed.uuid }
            }
            WebhookPayload::CustodianChanged(change) => AccountEvent::CustodianChanged {
                uuid: change.uuid,
                to: change.to.map(|to| (to.uuid, to.id)),
            },
            WebhookPayload::Ping => AccountEvent::Ping,
            _ => AccountEvent::Unknown,
        };
        Ok(VerifiedEvent {
            event_id: event.event_id,
            event_type: event.event_type,
            occurred_at: event.occurred_at,
            event: account_event,
        })
    }

    async fn issue_user_verification(
        &self,
        subject_token: &SecretString,
        receiving_app: &str,
        scopes: &[&str],
        idempotency_key: &str,
    ) -> AppResult<IssuedProof> {
        let app = self
            .inner
            .client
            .as_app(&self.inner.app_id, self.inner.app_secret.expose_secret());
        let request = IssueUserVerification {
            subject_token: subject_token.expose_secret().to_owned(),
            receiving_app: receiving_app.to_owned(),
            scopes: scopes.iter().map(|scope| (*scope).to_owned()).collect(),
            access_ttl_seconds: None,
        };
        app.issue_user_verification(&request, Some(idempotency_key))
            .await
            .map(issued)
            .map_err(|error| Self::proof_error(&error))
    }

    async fn issue_app_verification(
        &self,
        receiving_app: &str,
        scopes: &[&str],
        idempotency_key: &str,
    ) -> AppResult<IssuedProof> {
        let app = self
            .inner
            .client
            .as_app(&self.inner.app_id, self.inner.app_secret.expose_secret());
        let request = IssueAppVerification {
            receiving_app: receiving_app.to_owned(),
            scopes: scopes.iter().map(|scope| (*scope).to_owned()).collect(),
            access_ttl_seconds: None,
        };
        app.issue_app_verification(&request, Some(idempotency_key))
            .await
            .map(issued)
            .map_err(|error| Self::proof_error(&error))
    }

    async fn refresh_proof(&self, refresh_token: &SecretString) -> AppResult<IssuedProof> {
        let app = self
            .inner
            .client
            .as_app(&self.inner.app_id, self.inner.app_secret.expose_secret());
        app.refresh_proof(refresh_token.expose_secret(), None)
            .await
            .map(issued)
            .map_err(|error| Self::proof_error(&error))
    }

    async fn revoke_proof(&self, proof_id: &str) -> AppResult<()> {
        let app = self
            .inner
            .client
            .as_app(&self.inner.app_id, self.inner.app_secret.expose_secret());
        app.revoke_proof(&ProofRef::Id(proof_id.to_owned()))
            .await
            .map_err(|error| Self::proof_error(&error))
    }
}
