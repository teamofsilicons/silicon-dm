//! Bounded encrypted cache of verified IAM access tokens for originator-owned Ting proofs.
//! Refresh tokens and OBO proofs never enter this store; clients own refresh rotation.

use std::fmt;

use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, OsRng, Payload, rand_core::RngCore as _},
};
use secrecy::{ExposeSecret as _, SecretString};
use serde_json::json;
use sqlx::{FromRow, PgConnection};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{
    AppError, AppResult,
    application::auth::{AuthContext, PresentedCredential},
    domain::{ActorRef, OrganizationId},
    infrastructure::postgres::{PostgresStore, TingDeliveryContext},
};

const KEY_DOMAIN: &str = "silicon-dm Ting originator credential encryption v1";
const MAX_TOKEN_BYTES: usize = 64 * 1024;
const MAX_CANDIDATES: i64 = 8;
const REQUIRED_SCOPES: [&str; 2] = ["self.identity.read", "obo:ting:tings.send"];

/// A candidate must be freshly authorized with IAM before minting any proof.
pub struct CachedTingCredential {
    /// Decrypted access token; diagnostic output always redacts it.
    pub token: SecretString,
    /// Exact-token digest used for conditional invalidation.
    pub digest: [u8; 32],
    /// IAM session identifier, only when disclosed by verified introspection.
    pub session_id: Option<Uuid>,
    /// Absolute IAM access-token expiry; cache reads never extend it.
    pub expires_at: OffsetDateTime,
}

impl fmt::Debug for CachedTingCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CachedTingCredential")
            .field("token", &"[REDACTED]")
            .field("session_id", &self.session_id)
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

/// One explicitly selected DM data plane's cache, encrypted with its app secret.
/// Callers retain responsibility for the live sandbox lifecycle fence.
pub struct TingCredentialCache {
    store: PostgresStore,
    cipher: Aes256Gcm,
    context: TingDeliveryContext,
    schema: String,
}

#[derive(FromRow)]
struct CredentialRecord {
    aad_app_id: Option<String>,
    aad_actor_id: Option<String>,
    token_digest: Vec<u8>,
    nonce: Vec<u8>,
    ciphertext: Vec<u8>,
    session_id: Option<Uuid>,
    expires_at: OffsetDateTime,
}

impl TingCredentialCache {
    /// Creates a context-bound cache; this does not authenticate any token.
    ///
    /// # Errors
    /// Rejects empty encryption material or incomplete data context.
    pub fn new(
        store: PostgresStore,
        key_material: &SecretString,
        context: TingDeliveryContext,
    ) -> AppResult<Self> {
        if key_material.expose_secret().is_empty()
            || context.app_id.is_empty()
            || context.app_id.len() > 255
            || context.app_id.chars().any(char::is_control)
        {
            return Err(AppError::validation(
                "Ting credential cache requires encryption material and a valid app ID",
            ));
        }
        match (context.testing_environment_id, context.testing_generation) {
            (None, None) => {}
            (Some(id), Some(generation)) if !id.is_nil() && generation > 0 => {}
            _ => {
                return Err(AppError::validation(
                    "Ting credential cache requires a sandbox ID and positive generation together",
                ));
            }
        }
        let schema = context
            .testing_environment_id
            .map_or_else(|| "dm".to_owned(), |id| format!("dm_test_{}", id.simple()));
        let key = blake3::derive_key(KEY_DOMAIN, key_material.expose_secret().as_bytes());
        let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| credential_error())?;
        Ok(Self {
            store,
            cipher,
            context,
            schema,
        })
    }

    /// Remembers only a fresh, IAM-verified token with both required proof scopes.
    /// Returns false for expired or insufficient authority and evicts that exact token.
    /// Same-token revalidation never extends its previously stored absolute expiry.
    ///
    /// # Errors
    /// Reports redacted context, encryption or database failures. The caller must
    /// supply `AuthContext` from live IAM verification, never an unverified token.
    pub async fn remember(&self, auth: &AuthContext) -> AppResult<bool> {
        let PresentedCredential::Bearer(token) = &auth.credential;
        let digest = *blake3::hash(token.expose_secret().as_bytes()).as_bytes();
        if !REQUIRED_SCOPES
            .iter()
            .all(|scope| auth.has_capability(scope))
            || auth.credential_expires_at <= OffsetDateTime::now_utc()
        {
            self.forget(&auth.organization_id, &auth.actor, &digest)
                .await?;
            return Ok(false);
        }
        if token.expose_secret().is_empty() || token.expose_secret().len() > MAX_TOKEN_BYTES {
            return Err(AppError::validation(
                "verified Ting access token exceeds the cache bounds",
            ));
        }
        let mut transaction = self.store.pool().begin().await?;
        self.verify_schema(&mut transaction).await?;
        let lock = self.actor_lock(&auth.organization_id, &auth.actor)?;
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(lock)
            .execute(&mut *transaction)
            .await?;
        prune_expired(&mut transaction).await?;
        let existing = sqlx::query_as::<_, CredentialRecord>(
            "SELECT token_digest,nonce,ciphertext,session_id,expires_at,aad_app_id,aad_actor_id FROM ting_credentials \
             WHERE app_id=$1 AND generation=$2 AND organization_id=$3 AND actor_kind=$4::text::actor_kind \
             AND actor_id=$5 AND token_digest=$6",
        ).bind(&self.context.app_id).bind(self.generation()).bind(auth.organization_id.as_str())
            .bind(auth.actor.actor_type.as_str()).bind(auth.actor.id.as_str()).bind(digest.as_slice())
            .fetch_optional(&mut *transaction).await?;
        let old_readable = existing.as_ref().is_some_and(|row| {
            self.decrypt(&auth.organization_id, &auth.actor, row)
                .is_ok()
        });
        let expires_at = existing.as_ref().map_or(auth.credential_expires_at, |row| {
            row.expires_at.min(auth.credential_expires_at)
        });
        // PostgreSQL timestamps preserve microseconds, so authenticate that exact precision.
        let expires_at = expires_at
            .replace_nanosecond(expires_at.nanosecond() / 1000 * 1000)
            .map_err(AppError::internal)?;
        let aad = self.aad(
            &auth.organization_id,
            &auth.actor,
            &digest,
            auth.session_id,
            expires_at,
        )?;
        let mut nonce = [0_u8; 12];
        OsRng.fill_bytes(&mut nonce);
        let ciphertext = self
            .cipher
            .encrypt(
                &Nonce::from(nonce),
                Payload {
                    msg: token.expose_secret().as_bytes(),
                    aad: &aad,
                },
            )
            .map_err(|_| credential_error())?;
        let stored = sqlx::query(
            "INSERT INTO ting_credentials(app_id,generation,organization_id,actor_kind,actor_id,token_digest,nonce,ciphertext,session_id,expires_at) \
             SELECT $1,$2,$3,$4::text::actor_kind,$5,$6,$7,$8,$9,$10 WHERE $10>clock_timestamp() \
             ON CONFLICT(app_id,generation,organization_id,actor_kind,actor_id,token_digest) DO UPDATE \
             SET nonce=EXCLUDED.nonce,ciphertext=EXCLUDED.ciphertext,session_id=EXCLUDED.session_id,expires_at=EXCLUDED.expires_at,verified_at=clock_timestamp(),aad_app_id=NULL,aad_actor_id=NULL",
        ).bind(&self.context.app_id).bind(self.generation()).bind(auth.organization_id.as_str())
            .bind(auth.actor.actor_type.as_str()).bind(auth.actor.id.as_str()).bind(digest.as_slice())
            .bind(nonce.as_slice()).bind(ciphertext).bind(auth.session_id).bind(expires_at)
            .execute(&mut *transaction).await?.rows_affected()>0;
        sqlx::query(
            "DELETE FROM ting_credentials WHERE app_id=$1 AND generation=$2 AND organization_id=$3 \
             AND actor_kind=$4::text::actor_kind AND actor_id=$5 AND token_digest IN (
                 SELECT token_digest FROM ting_credentials WHERE app_id=$1 AND generation=$2 AND organization_id=$3 \
                 AND actor_kind=$4::text::actor_kind AND actor_id=$5 ORDER BY verified_at DESC,expires_at DESC,token_digest DESC OFFSET $6)",
        ).bind(&self.context.app_id).bind(self.generation()).bind(auth.organization_id.as_str())
            .bind(auth.actor.actor_type.as_str()).bind(auth.actor.id.as_str()).bind(MAX_CANDIDATES)
            .execute(&mut *transaction).await?;
        if stored && !old_readable {
            self.wake_originator(&mut transaction, auth).await?;
        }
        transaction.commit().await?;
        Ok(stored)
    }

    /// Returns at most eight unexpired candidates, most recently verified first.
    /// Undecryptable, transplanted or tampered entries never yield authority.
    ///
    /// # Errors
    /// Reports database/context failures. Unreadable ciphertext is skipped with a
    /// fixed diagnostic so a new verified token can recover after key rotation.
    pub async fn candidates(
        &self,
        org: &OrganizationId,
        actor: &ActorRef,
    ) -> AppResult<Vec<CachedTingCredential>> {
        let mut transaction = self.store.pool().begin().await?;
        self.verify_schema(&mut transaction).await?;
        prune_expired(&mut transaction).await?;
        let rows = sqlx::query_as::<_, CredentialRecord>(
            "SELECT token_digest,nonce,ciphertext,session_id,expires_at,aad_app_id,aad_actor_id FROM ting_credentials \
             WHERE app_id=$1 AND generation=$2 AND organization_id=$3 AND actor_kind=$4::text::actor_kind AND actor_id=$5 \
             AND expires_at>clock_timestamp() ORDER BY verified_at DESC,expires_at DESC,token_digest DESC LIMIT $6",
        ).bind(&self.context.app_id).bind(self.generation()).bind(org.as_str())
            .bind(actor.actor_type.as_str()).bind(actor.id.as_str()).bind(MAX_CANDIDATES)
            .fetch_all(&mut *transaction).await?;
        transaction.commit().await?;
        let mut credentials = Vec::with_capacity(rows.len());
        for row in rows {
            if row.expires_at <= OffsetDateTime::now_utc() {
                continue;
            }
            if let Ok(token) = self.decrypt(org, actor, &row) {
                credentials.push(CachedTingCredential {
                    token,
                    digest: row
                        .token_digest
                        .as_slice()
                        .try_into()
                        .map_err(|_| credential_error())?,
                    session_id: row.session_id,
                    expires_at: row.expires_at,
                });
            } else {
                tracing::warn!(
                    code = "ting_cached_credential_unreadable",
                    "Ting cached authority was not usable"
                );
            }
        }
        Ok(credentials)
    }

    /// Removes only this exact token in the selected app, actor and generation.
    /// A failed older token cannot revoke another candidate or a newly refreshed token.
    ///
    /// # Errors
    /// Reports database/context failures without exposing token contents.
    pub async fn forget(
        &self,
        org: &OrganizationId,
        actor: &ActorRef,
        digest: &[u8; 32],
    ) -> AppResult<()> {
        let mut transaction = self.store.pool().begin().await?;
        self.verify_schema(&mut transaction).await?;
        sqlx::query("DELETE FROM ting_credentials WHERE app_id=$1 AND generation=$2 AND organization_id=$3 AND actor_kind=$4::text::actor_kind AND actor_id=$5 AND token_digest=$6")
            .bind(&self.context.app_id).bind(self.generation()).bind(org.as_str()).bind(actor.actor_type.as_str())
            .bind(actor.id.as_str()).bind(digest.as_slice()).execute(&mut *transaction).await?;
        prune_expired(&mut transaction).await?;
        transaction.commit().await?;
        Ok(())
    }

    fn generation(&self) -> i64 {
        self.context.testing_generation.unwrap_or(0)
    }

    async fn verify_schema(&self, connection: &mut PgConnection) -> AppResult<()> {
        let actual: String = sqlx::query_scalar("SELECT current_schema()")
            .fetch_one(connection)
            .await?;
        if actual == self.schema {
            Ok(())
        } else {
            Err(AppError::conflict(
                "Ting credential cache does not match the DM data plane",
            ))
        }
    }

    fn actor_lock(&self, org: &OrganizationId, actor: &ActorRef) -> AppResult<i64> {
        let identity = serde_json::to_vec(&json!([
            "ting-credentials",
            self.schema,
            self.context.app_id,
            self.generation(),
            org,
            actor
        ]))
        .map_err(AppError::internal)?;
        let hash = blake3::hash(&identity);
        let mut bytes = [0_u8; 8];
        bytes.copy_from_slice(&hash.as_bytes()[..8]);
        Ok(i64::from_be_bytes(bytes))
    }

    fn aad(
        &self,
        org: &OrganizationId,
        actor: &ActorRef,
        digest: &[u8; 32],
        session_id: Option<Uuid>,
        expires_at: OffsetDateTime,
    ) -> AppResult<Vec<u8>> {
        serde_json::to_vec(&json!({
            "version":1,"schema":self.schema,"app_id":self.context.app_id,
            "testing_environment_id":self.context.testing_environment_id,"testing_generation":self.context.testing_generation,
            "organization_id":org,"actor":actor,"token_digest":digest,"session_id":session_id,
            "expires_at_microseconds":expires_at.unix_timestamp_nanos()/1000,
        })).map_err(AppError::internal)
    }

    fn decrypt(
        &self,
        org: &OrganizationId,
        actor: &ActorRef,
        row: &CredentialRecord,
    ) -> AppResult<SecretString> {
        let digest: [u8; 32] = row
            .token_digest
            .as_slice()
            .try_into()
            .map_err(|_| credential_error())?;
        let nonce: [u8; 12] = row
            .nonce
            .as_slice()
            .try_into()
            .map_err(|_| credential_error())?;
        let mut aad: serde_json::Value = serde_json::from_slice(&self.aad(
            org,
            actor,
            &digest,
            row.session_id,
            row.expires_at,
        )?)
        .map_err(|_| credential_error())?;
        if let Some(original) = &row.aad_app_id {
            let mapped = original
                .split_once('>')
                .map_or(original.as_str(), |(_, app)| app);
            if mapped != self.context.app_id {
                return Err(credential_error());
            }
            aad["app_id"] = json!(original);
        }
        if let Some(original) = &row.aad_actor_id {
            let mapped = if original.starts_with("c:") || original.starts_with("si:") {
                original.clone()
            } else {
                match actor.actor_type {
                    crate::domain::ActorType::Carbon => format!("c:{original}"),
                    crate::domain::ActorType::Silicon => {
                        let (handle, original_org) =
                            original.split_once(':').ok_or_else(credential_error)?;
                        if original_org != org.as_str() {
                            return Err(credential_error());
                        }
                        format!("si:{handle}")
                    }
                }
            };
            if mapped != actor.id.as_str() {
                return Err(credential_error());
            }
            aad["actor"]["id"] = json!(original);
        }
        let aad = serde_json::to_vec(&aad).map_err(|_| credential_error())?;
        let plaintext = self
            .cipher
            .decrypt(
                &Nonce::from(nonce),
                Payload {
                    msg: &row.ciphertext,
                    aad: &aad,
                },
            )
            .map_err(|_| credential_error())?;
        if blake3::hash(&plaintext).as_bytes() != &digest {
            return Err(credential_error());
        }
        String::from_utf8(plaintext)
            .map(SecretString::from)
            .map_err(|_| credential_error())
    }

    async fn wake_originator(
        &self,
        connection: &mut PgConnection,
        auth: &AuthContext,
    ) -> AppResult<()> {
        sqlx::query(
            "UPDATE ting_handoffs SET next_attempt_at=clock_timestamp() WHERE organization_id=$1 \
             AND originator_kind=$2::text::actor_kind AND originator_id=$3 AND accepted_at IS NULL \
             AND (lease_id IS NULL OR lease_expires_at<=clock_timestamp()) \
             AND (request_body IS NULL OR (request_body::jsonb->'metadata'->'testing_environment_id'=$4::jsonb \
                 AND request_body::jsonb->'metadata'->'testing_generation'=$5::jsonb AND request_body::jsonb->>'type'=$6))",
        ).bind(auth.organization_id.as_str()).bind(auth.actor.actor_type.as_str()).bind(auth.actor.id.as_str())
            .bind(sqlx::types::Json(json!(self.context.testing_environment_id)))
            .bind(sqlx::types::Json(json!(self.context.testing_generation))).bind(format!("{}.sync.changed",self.context.app_id))
            .execute(&mut *connection).await?;
        let wakeup = serde_json::to_string(&json!({"org_id":auth.organization_id,"actor_type":auth.actor.actor_type,"actor_id":auth.actor.id}))
            .map_err(AppError::internal)?;
        sqlx::query("SELECT pg_notify('dm_delivery',$1)")
            .bind(wakeup)
            .execute(connection)
            .await?;
        Ok(())
    }
}

async fn prune_expired(connection: &mut PgConnection) -> AppResult<()> {
    sqlx::query("DELETE FROM ting_credentials WHERE expires_at<=clock_timestamp()")
        .execute(connection)
        .await?;
    Ok(())
}

fn credential_error() -> AppError {
    AppError::internal(anyhow::anyhow!(
        "Ting credential encryption or verification failed"
    ))
}
