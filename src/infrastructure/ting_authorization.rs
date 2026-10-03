//! Explicit Ting consent and encrypted, durable endpoint credentials.
//!
//! Ordinary DM sessions never enter this store. Refresh retries derive their
//! identity from the exact previous credential, including after process failure.
use super::{
    ting::{TingSendAuthority, TingTestingHeaders},
    ting_credentials::TingCredentialCache,
};
use crate::{
    AppError, AppResult,
    application::{
        auth::{AuthContext, PresentedCredential},
        ports::IdentityProvider,
    },
    domain::{ActorRef, OrganizationId},
};
use aes_gcm::{
    Nonce,
    aead::{Aead as _, OsRng, Payload, rand_core::RngCore as _},
};
use secrecy::{ExposeSecret as _, SecretString};
use serde_json::{Value, json};
use silicon_iam_client::models;
use sqlx::{FromRow, PgConnection};
use time::OffsetDateTime;
use uuid::Uuid;

pub(crate) const ENDPOINTS: [&str; 2] = ["subscriptions.register", "tings.send"];

#[derive(FromRow)]
struct Record {
    grant_id: Uuid,
    nonce: Vec<u8>,
    ciphertext: Vec<u8>,
}

/// Dedicated authorization storage in one selected DM data plane.
pub struct TingAuthorization(pub(crate) TingCredentialCache);

impl TingAuthorization {
    /// Binds the dedicated grant store to an already validated data plane.
    #[must_use]
    pub fn new(cache: TingCredentialCache) -> Self {
        Self(cache)
    }

    fn aad(
        &self,
        org: &OrganizationId,
        actor: &ActorRef,
        endpoint: &str,
        grant: Uuid,
    ) -> AppResult<Vec<u8>> {
        serde_json::to_vec(&json!([
            "dm.ting.obo.v1",
            self.0.context.app_id,
            self.0.context.testing_environment_id,
            self.0.generation(),
            org,
            actor,
            endpoint,
            grant
        ]))
        .map_err(AppError::internal)
    }
    fn encrypt(
        &self,
        org: &OrganizationId,
        actor: &ActorRef,
        pair: &models::OboTokenPair,
    ) -> AppResult<(Vec<u8>, Vec<u8>)> {
        self.encrypt_payload(org, actor, &pair.endpoint_id, pair.grant_id, pair)
    }
    fn encrypt_payload(
        &self,
        org: &OrganizationId,
        actor: &ActorRef,
        endpoint: &str,
        grant: Uuid,
        value: &impl serde::Serialize,
    ) -> AppResult<(Vec<u8>, Vec<u8>)> {
        let mut nonce = [0_u8; 12];
        OsRng.fill_bytes(&mut nonce);
        let plain = secrecy::SecretBox::new(Box::new(
            serde_json::to_vec(value).map_err(AppError::internal)?,
        ));
        let ciphertext = self
            .0
            .cipher
            .encrypt(
                &Nonce::from(nonce),
                Payload {
                    msg: plain.expose_secret(),
                    aad: &self.aad(org, actor, endpoint, grant)?,
                },
            )
            .map_err(|_| unavailable())?;
        Ok((nonce.to_vec(), ciphertext))
    }
    fn decrypt<T: serde::de::DeserializeOwned>(
        &self,
        org: &OrganizationId,
        actor: &ActorRef,
        endpoint: &str,
        row: &Record,
    ) -> AppResult<T> {
        let nonce: [u8; 12] = row.nonce.as_slice().try_into().map_err(|_| required())?;
        let plain = self
            .0
            .cipher
            .decrypt(
                &Nonce::from(nonce),
                Payload {
                    msg: &row.ciphertext,
                    aad: &self.aad(org, actor, endpoint, row.grant_id)?,
                },
            )
            .map_err(|_| required())?;
        serde_json::from_slice(&plain).map_err(|_| required())
    }
    async fn lock(
        &self,
        connection: &mut PgConnection,
        org: &OrganizationId,
        actor: &ActorRef,
    ) -> AppResult<()> {
        self.0.verify_schema(connection).await?;
        let hash = blake3::hash(&self.aad(org, actor, "lock", Uuid::nil())?);
        let mut bytes = [0; 8];
        bytes.copy_from_slice(&hash.as_bytes()[..8]);
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(i64::from_be_bytes(bytes))
            .execute(connection)
            .await?;
        Ok(())
    }
    async fn prepare_start(&self, auth: &AuthContext, key: &str) -> AppResult<()> {
        let PresentedCredential::Bearer(token) = &auth.credential;
        let request = models::OboAuthorizationRequest {
            org_id: auth.organization_id.as_str().into(),
            subject_token: token.expose_secret().into(),
            endpoints: ENDPOINTS
                .iter()
                .map(|id| models::OboAuthorizationEndpoint {
                    audience: "ting".into(),
                    endpoint_id: (*id).into(),
                })
                .collect(),
            redirect_uri: None,
            state: None,
        };
        let (nonce, ciphertext) = self.encrypt_payload(
            &auth.organization_id,
            &auth.actor,
            &format!("start:{key}"),
            Uuid::nil(),
            &request,
        )?;
        let mut tx = self.0.store.pool().begin().await?;
        self.lock(&mut tx, &auth.organization_id, &auth.actor)
            .await?;
        sqlx::query("INSERT INTO ting_authorizations(app_id,generation,organization_id,actor_kind,actor_id,authorization_id,expires_at,start_key,start_nonce,start_ciphertext) VALUES($1,$2,$3,$4::text::actor_kind,$5,$6,$7,$8,$9,$10) ON CONFLICT(app_id,generation,organization_id,actor_kind,actor_id,start_key) DO NOTHING")
          .bind(&self.0.context.app_id).bind(self.0.generation()).bind(auth.organization_id.as_str()).bind(auth.actor.actor_type.as_str()).bind(auth.actor.id.as_str()).bind(Uuid::new_v4()).bind(OffsetDateTime::now_utc()+time::Duration::minutes(10)).bind(key).bind(nonce).bind(ciphertext).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }
    /// Creates a pending request only after an explicit action by the user.
    ///
    /// # Errors
    /// Rejects invalid or missing authority, mismatched data planes and unavailable storage or IAM.
    pub async fn begin(
        &self,
        identity: &dyn IdentityProvider,
        auth: &AuthContext,
        key: &str,
    ) -> AppResult<Value> {
        silicon_iam_client::IdempotencyKey::parse(key)
            .map_err(|_| AppError::validation("invalid authorization key"))?;
        // AuthContext already proves the current account. Persist the original
        // subject token before IAM receives anything, so refreshed login tokens
        // do not change a retry after an uncertain authorization response.
        self.prepare_start(auth, key).await?;
        let mut tx = self.0.store.pool().begin().await?;
        self.lock(&mut tx, &auth.organization_id, &auth.actor)
            .await?;
        let (expires, nonce, ciphertext):(OffsetDateTime,Vec<u8>,Vec<u8>)=sqlx::query_as("SELECT expires_at,start_nonce,start_ciphertext FROM ting_authorizations WHERE app_id=$1 AND generation=$2 AND organization_id=$3 AND actor_kind=$4::text::actor_kind AND actor_id=$5 AND start_key=$6 FOR UPDATE")
          .bind(&self.0.context.app_id).bind(self.0.generation()).bind(auth.organization_id.as_str()).bind(auth.actor.actor_type.as_str()).bind(auth.actor.id.as_str()).bind(key).fetch_one(&mut *tx).await?;
        if expires <= OffsetDateTime::now_utc() {
            return Err(required());
        }
        let request = self.decrypt(
            &auth.organization_id,
            &auth.actor,
            &format!("start:{key}"),
            &Record {
                grant_id: Uuid::nil(),
                nonce,
                ciphertext,
            },
        )?;
        let detail = identity.authorize_ting(&request, key).await?;
        if detail.app_id != self.0.context.app_id
            || detail.org_id != auth.organization_id.as_str()
            || detail.actor.public_id != auth.actor.id.as_str()
            || detail.expires_at <= OffsetDateTime::now_utc()
        {
            return Err(AppError::Forbidden);
        }
        let url = detail
            .authorization_url
            .as_deref()
            .ok_or_else(unavailable)?;
        let parsed = url::Url::parse(url).map_err(|_| unavailable())?;
        if parsed.scheme() != "https"
            && !(parsed.scheme() == "http"
                && matches!(parsed.host_str(), Some("localhost" | "127.0.0.1" | "[::1]")))
        {
            return Err(unavailable());
        }
        sqlx::query("UPDATE ting_authorizations SET authorization_id=$7,expires_at=$8 WHERE app_id=$1 AND generation=$2 AND organization_id=$3 AND actor_kind=$4::text::actor_kind AND actor_id=$5 AND start_key=$6")
          .bind(&self.0.context.app_id).bind(self.0.generation()).bind(auth.organization_id.as_str()).bind(auth.actor.actor_type.as_str()).bind(auth.actor.id.as_str()).bind(key).bind(detail.id).bind(detail.expires_at).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(
            json!({"authorization_id":detail.id,"authorization_url":url,"expires_at":detail.expires_at,"status":"pending","endpoints":ENDPOINTS}),
        )
    }
    /// Exchanges the reviewed authorization code; credentials never leave the server.
    ///
    /// # Errors
    /// Rejects invalid or missing authority, mismatched data planes and unavailable storage or IAM.
    pub async fn complete(
        &self,
        identity: &dyn IdentityProvider,
        auth: &AuthContext,
        id: Uuid,
        code: &str,
    ) -> AppResult<Value> {
        if code.is_empty() || code.len() > 4096 {
            return Err(AppError::validation("authorization code is invalid"));
        }
        let digest = blake3::hash(code.as_bytes());
        let mut tx = self.0.store.pool().begin().await?;
        self.lock(&mut tx, &auth.organization_id, &auth.actor)
            .await?;
        let request:Option<(OffsetDateTime,Option<Vec<u8>>,Option<OffsetDateTime>)>=sqlx::query_as("SELECT expires_at,completion_digest,completed_at FROM ting_authorizations WHERE app_id=$1 AND generation=$2 AND organization_id=$3 AND actor_kind=$4::text::actor_kind AND actor_id=$5 AND authorization_id=$6 FOR UPDATE")
          .bind(&self.0.context.app_id).bind(self.0.generation()).bind(auth.organization_id.as_str()).bind(auth.actor.actor_type.as_str()).bind(auth.actor.id.as_str()).bind(id).fetch_optional(&mut *tx).await?;
        let Some((expires, previous, completed)) = request else {
            return Err(AppError::NotFound);
        };
        if previous
            .as_deref()
            .is_some_and(|value| value != digest.as_bytes())
        {
            return Err(AppError::conflict("authorization completion changed"));
        }
        if completed.is_some() {
            return Ok(json!({"status":"authorized"}));
        }
        if expires <= OffsetDateTime::now_utc() {
            return Err(required());
        }
        // Check the provider identity the user selected, because DM notifications
        // must stay in the originating DM account and organization.
        let detail = identity.ting_authorization(id).await?;
        if detail.id != id
            || detail.app_id != self.0.context.app_id
            || detail.actor.public_id != auth.actor.id.as_str()
            || detail.org_id != auth.organization_id.as_str()
            || detail.providers.as_ref().is_none_or(|providers| {
                !providers.iter().any(|p| {
                    p.app_id == "ting"
                        && p.actor.public_id == auth.actor.id.as_str()
                        && p.org_id == auth.organization_id.as_str()
                })
            })
        {
            return Err(AppError::Forbidden);
        }
        let response = identity
            .ting_tokens(
                models::OboTokenRequest {
                    grant_id: None,
                    subject_token: None,
                    authorization_id: Some(id),
                    authorization_code: Some(code.to_owned()),
                    refresh_token: None,
                },
                &retry_key(&format!("code:{id}:{code}")),
            )
            .await?;
        if response.items.len() != ENDPOINTS.len()
            || !ENDPOINTS.iter().all(|endpoint| {
                response
                    .items
                    .iter()
                    .filter(|pair| pair.endpoint_id == *endpoint)
                    .count()
                    == 1
            })
        {
            return Err(unavailable());
        }
        for pair in &response.items {
            self.validate_pair(identity, &auth.organization_id, &auth.actor, pair)
                .await?;
            self.save(&mut tx, &auth.organization_id, &auth.actor, pair)
                .await?;
        }
        sqlx::query("UPDATE ting_authorizations SET completion_digest=$7,completed_at=clock_timestamp() WHERE app_id=$1 AND generation=$2 AND organization_id=$3 AND actor_kind=$4::text::actor_kind AND actor_id=$5 AND authorization_id=$6")
          .bind(&self.0.context.app_id).bind(self.0.generation()).bind(auth.organization_id.as_str()).bind(auth.actor.actor_type.as_str()).bind(auth.actor.id.as_str()).bind(id).bind(digest.as_bytes().as_slice()).execute(&mut *tx).await?;
        // Wake suspended sends, while every recipient still revalidates the token.
        sqlx::query("UPDATE ting_handoffs SET next_attempt_at=clock_timestamp() WHERE organization_id=$1 AND originator_kind=$2::text::actor_kind AND originator_id=$3 AND accepted_at IS NULL")
          .bind(auth.organization_id.as_str()).bind(auth.actor.actor_type.as_str()).bind(auth.actor.id.as_str()).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(json!({"status":"authorized"}))
    }
    async fn validate_pair(
        &self,
        identity: &dyn IdentityProvider,
        org: &OrganizationId,
        actor: &ActorRef,
        pair: &models::OboTokenPair,
    ) -> AppResult<()> {
        if pair.actor.as_ref().is_none_or(|selected| {
            selected.public_id != actor.id.as_str()
                || serde_json::to_value(&selected.type_field).ok().as_ref()
                    != Some(&json!(actor.actor_type.as_str()))
        }) || pair.audience != "ting"
            || pair.org_id != org.as_str()
            || !ENDPOINTS.contains(&pair.endpoint_id.as_str())
            || !pair.access_token.starts_with("oba_")
            || !pair.refresh_token.starts_with("obr_")
            || pair.scope != format!("obo:ting:{}", pair.endpoint_id)
            || pair.expires_in <= 0
            || pair.grant_id.is_nil()
            || pair.expires_at <= OffsetDateTime::now_utc()
        {
            return Err(unavailable());
        }
        identity
            .validate_ting_context(pair.testing_context.as_ref())
            .await
    }
    async fn save(
        &self,
        connection: &mut PgConnection,
        org: &OrganizationId,
        actor: &ActorRef,
        pair: &models::OboTokenPair,
    ) -> AppResult<()> {
        let (nonce, ciphertext) = self.encrypt(org, actor, pair)?;
        sqlx::query("INSERT INTO ting_obo_credentials(app_id,generation,organization_id,actor_kind,actor_id,endpoint_id,grant_id,nonce,ciphertext) VALUES($1,$2,$3,$4::text::actor_kind,$5,$6,$7,$8,$9) ON CONFLICT(app_id,generation,organization_id,actor_kind,actor_id,endpoint_id) DO UPDATE SET grant_id=EXCLUDED.grant_id,nonce=EXCLUDED.nonce,ciphertext=EXCLUDED.ciphertext,updated_at=clock_timestamp()")
          .bind(&self.0.context.app_id).bind(self.0.generation()).bind(org.as_str()).bind(actor.actor_type.as_str()).bind(actor.id.as_str()).bind(&pair.endpoint_id).bind(pair.grant_id).bind(nonce).bind(ciphertext).execute(connection).await?;
        Ok(())
    }
    /// Returns only separately approved authority, rotating its own refresh family.
    ///
    /// # Errors
    /// Rejects invalid or missing authority, mismatched data planes and unavailable storage or IAM.
    pub async fn authority(
        &self,
        identity: &dyn IdentityProvider,
        org: &OrganizationId,
        actor: &ActorRef,
        endpoint: &str,
    ) -> AppResult<TingSendAuthority> {
        if !ENDPOINTS.contains(&endpoint) {
            return Err(AppError::Forbidden);
        }
        let mut tx = self.0.store.pool().begin().await?;
        self.lock(&mut tx, org, actor).await?;
        let row:Option<Record>=sqlx::query_as("SELECT grant_id,nonce,ciphertext FROM ting_obo_credentials WHERE app_id=$1 AND generation=$2 AND organization_id=$3 AND actor_kind=$4::text::actor_kind AND actor_id=$5 AND endpoint_id=$6 FOR UPDATE")
          .bind(&self.0.context.app_id).bind(self.0.generation()).bind(org.as_str()).bind(actor.actor_type.as_str()).bind(actor.id.as_str()).bind(endpoint).fetch_optional(&mut *tx).await?;
        let mut pair: models::OboTokenPair =
            self.decrypt(org, actor, endpoint, &row.ok_or_else(required)?)?;
        if pair.expires_at <= OffsetDateTime::now_utc() + time::Duration::seconds(5) {
            let response = identity
                .ting_tokens(
                    models::OboTokenRequest {
                        grant_id: None,
                        subject_token: None,
                        authorization_id: None,
                        authorization_code: None,
                        refresh_token: Some(pair.refresh_token.clone()),
                    },
                    &retry_key(&format!("refresh:{}:{}", pair.grant_id, pair.refresh_token)),
                )
                .await;
            let response = match response {
                Ok(value) => value,
                Err(
                    AppError::Unauthorized
                    | AppError::Forbidden
                    | AppError::TingAuthorizationRequired,
                ) => {
                    self.remove(&mut tx, org, actor, Some(endpoint)).await?;
                    tx.commit().await?;
                    return Err(required());
                }
                Err(error) => return Err(error),
            };
            if response.items.len() != 1 {
                return Err(unavailable());
            }
            let next = response.items.into_iter().next().ok_or_else(unavailable)?;
            if next.grant_id != pair.grant_id || next.endpoint_id != endpoint {
                return Err(unavailable());
            }
            self.validate_pair(identity, org, actor, &next).await?;
            self.save(&mut tx, org, actor, &next).await?;
            pair = next;
        }
        self.validate_pair(identity, org, actor, &pair).await?;
        tx.commit().await?;
        Ok(TingSendAuthority {
            proof_token: SecretString::from(pair.access_token),
            testing: pair.testing_context.map(|ctx| TingTestingHeaders {
                app_secret: SecretString::from(ctx.app_secret),
                environment_key: SecretString::from(ctx.iam_test_key),
            }),
        })
    }
    async fn remove(
        &self,
        connection: &mut PgConnection,
        org: &OrganizationId,
        actor: &ActorRef,
        endpoint: Option<&str>,
    ) -> AppResult<()> {
        sqlx::query("DELETE FROM ting_obo_credentials WHERE app_id=$1 AND generation=$2 AND organization_id=$3 AND actor_kind=$4::text::actor_kind AND actor_id=$5 AND ($6::text IS NULL OR endpoint_id=$6)")
          .bind(&self.0.context.app_id).bind(self.0.generation()).bind(org.as_str()).bind(actor.actor_type.as_str()).bind(actor.id.as_str()).bind(endpoint).execute(connection).await?;
        Ok(())
    }
    /// Stops DM's local delegation immediately. IAM remains the grant revocation UI.
    ///
    /// # Errors
    /// Rejects invalid or missing authority, mismatched data planes and unavailable storage or IAM.
    pub async fn disconnect(&self, auth: &AuthContext) -> AppResult<Value> {
        let mut tx = self.0.store.pool().begin().await?;
        self.lock(&mut tx, &auth.organization_id, &auth.actor)
            .await?;
        self.remove(&mut tx, &auth.organization_id, &auth.actor, None)
            .await?;
        tx.commit().await?;
        Ok(json!({"status":"authorization_required"}))
    }
    /// Reports locally stored consent without disclosing credentials.
    ///
    /// # Errors
    /// Rejects invalid or missing authority, mismatched data planes and unavailable storage or IAM.
    pub async fn status(&self, auth: &AuthContext) -> AppResult<Value> {
        let mut tx = self.0.store.pool().begin().await?;
        self.lock(&mut tx, &auth.organization_id, &auth.actor)
            .await?;
        let count:i64=sqlx::query_scalar("SELECT count(*) FROM ting_obo_credentials WHERE app_id=$1 AND generation=$2 AND organization_id=$3 AND actor_kind=$4::text::actor_kind AND actor_id=$5")
          .bind(&self.0.context.app_id).bind(self.0.generation()).bind(auth.organization_id.as_str()).bind(auth.actor.actor_type.as_str()).bind(auth.actor.id.as_str()).fetch_one(&mut *tx).await?;
        tx.commit().await?;
        Ok(
            json!({"status":if count==2{"authorized"}else{"authorization_required"},"endpoints":ENDPOINTS}),
        )
    }
}
fn retry_key(input: &str) -> String {
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&blake3::hash(input.as_bytes()).as_bytes()[..16]);
    Uuid::from_bytes(bytes).to_string()
}
fn required() -> AppError {
    AppError::TingAuthorizationRequired
}
fn unavailable() -> AppError {
    AppError::DependencyUnavailable { dependency: "iam" }
}
