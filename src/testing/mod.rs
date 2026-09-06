//! Test environments share one separate database, with an immutable schema scope
//! and an environment UUID on every data row. Control records live in production.

pub mod handlers;
mod journal;
mod storage;

use std::{collections::HashMap, sync::Arc, time::Duration};

use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, OsRng, Payload, rand_core::RngCore},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use secrecy::{ExposeSecret as _, SecretString};
use serde::Serialize;
use sqlx::{FromRow, PgConnection, Postgres, Transaction};
use time::OffsetDateTime;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    AppError, AppResult,
    application::state::AppState,
    config::{DatabaseSettings, TestingSettings},
    infrastructure::{iam::IamClient, postgres::PostgresStore},
    realtime::RealtimeHub,
    worker::DeliveryWorker,
};

/// Public environment metadata. Secret values are returned only by explicit key operations.
#[derive(Clone, Debug, Serialize, FromRow)]
pub struct TestingEnvironment {
    /// DM environment UUID, accepted by the CLI's `--test` selector.
    pub environment_id: Uuid,
    /// Owning production organization.
    pub organization_id: String,
    /// Production actor who created the environment.
    pub creator_actor_id: String,
    /// Actor namespace of the creator.
    pub creator_actor_kind: String,
    /// Friendly environment name.
    pub name: String,
    /// Optional description.
    pub description: Option<String>,
    /// Paired IAM testing environment; production IAM is never used.
    pub iam_environment_id: Uuid,
    /// Imported test application identifier.
    pub iam_app_id: String,
    /// Active, deleted, creating, or purging lifecycle state.
    pub status: String,
    /// Generation changes invalidate cached state and open sessions.
    pub version: i64,
    /// Creation time.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// Last authenticated activity time.
    #[serde(with = "time::serde::rfc3339")]
    pub last_activity_at: OffsetDateTime,
    /// Soft deletion time.
    #[serde(with = "time::serde::rfc3339::option")]
    pub deleted_at: Option<OffsetDateTime>,
    /// Permanent destruction time.
    #[serde(with = "time::serde::rfc3339::option")]
    pub purge_after: Option<OffsetDateTime>,
}

struct EnvironmentRuntime {
    version: i64,
    store: PostgresStore,
    identity: Arc<IamClient>,
    realtime: RealtimeHub,
    cancellation: CancellationToken,
}

/// Application-wide test environment registry and lifecycle coordinator.
pub struct TestingRegistry {
    production: PostgresStore,
    admin: PostgresStore,
    database: DatabaseSettings,
    cipher: Aes256Gcm,
    runtimes: Mutex<HashMap<Uuid, EnvironmentRuntime>>,
    initialization: Mutex<()>,
}

impl TestingRegistry {
    /// Connects the separate shared test database and validates encryption configuration.
    ///
    /// # Errors
    /// Returns configuration errors for an invalid encryption key or shared production database, and connection errors.
    pub async fn new(production: PostgresStore, settings: &TestingSettings) -> AppResult<Self> {
        let key = STANDARD
            .decode(settings.encryption_key.expose_secret())
            .map_err(|_| {
                AppError::validation(
                    "DM_TEST_KEY_ENCRYPTION_KEY must be base64 for 32 random bytes",
                )
            })?;
        let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| {
            AppError::validation("DM_TEST_KEY_ENCRYPTION_KEY must decode to 32 bytes")
        })?;
        let admin = PostgresStore::connect(&settings.database).await?;
        let identity_sql = "SELECT current_database() || ':' || COALESCE(inet_server_addr()::text, 'local') || ':' || COALESCE(inet_server_port()::text, 'local')";
        let production_identity: String = sqlx::query_scalar(identity_sql)
            .fetch_one(production.pool())
            .await?;
        let test_identity: String = sqlx::query_scalar(identity_sql)
            .fetch_one(admin.pool())
            .await?;
        if production_identity == test_identity {
            return Err(AppError::validation(
                "testing and production must use separate databases",
            ));
        }
        Ok(Self {
            production,
            admin,
            database: settings.database.clone(),
            cipher,
            runtimes: Mutex::new(HashMap::new()),
            initialization: Mutex::new(()),
        })
    }

    /// Selects the isolated data plane with an active root key; invalid keys fail closed.
    ///
    /// # Errors
    /// Returns unauthorized for malformed or inactive keys, or errors opening the selected database/IAM adapters.
    pub async fn state_for_key(&self, parent: &AppState, key: &str) -> AppResult<AppState> {
        validate_key(key)?;
        let row = sqlx::query_as::<_, TestingEnvironment>(
            "UPDATE dm.testing_environments SET last_activity_at = clock_timestamp() WHERE root_key_digest = $1 AND status = 'active' RETURNING *")
            .bind(blake3::hash(key.as_bytes()).as_bytes().as_slice()).fetch_optional(self.production.pool()).await?
            .ok_or(AppError::Unauthorized)?;
        self.state_for_environment(parent, &row).await
    }

    /// Builds states for verifying IAM webhooks without granting unverified payloads access.
    ///
    /// # Errors
    /// Returns database, decryption, migration, or IAM-client configuration errors.
    pub async fn active_states(&self, parent: &AppState) -> AppResult<Vec<AppState>> {
        let rows = sqlx::query_as::<_, TestingEnvironment>(
            "SELECT * FROM dm.testing_environments WHERE status = 'active'",
        )
        .fetch_all(self.production.pool())
        .await?;
        let mut states = Vec::with_capacity(rows.len());
        for row in rows {
            states.push(self.state_for_environment(parent, &row).await?);
        }
        Ok(states)
    }

    /// Verifies exact webhook bytes before initializing any testing runtime, then
    /// selects every active DM environment paired with the authenticated IAM plane.
    ///
    /// # Errors
    /// Returns unauthorized unless an exact matching signer verifies the bytes, or errors loading authenticated targets.
    pub async fn verified_webhook_states(
        &self,
        parent: &AppState,
        headers: &axum::http::HeaderMap,
        body: &[u8],
        key_hint: &SecretString,
    ) -> AppResult<Vec<(AppState, silicon_iam_client::models::WebhookEvent)>> {
        use crate::application::ports::IdentityProvider as _;
        use subtle::ConstantTimeEq as _;
        let rows: Vec<TestingEnvironment> =
            sqlx::query_as("SELECT * FROM dm.testing_environments WHERE status='active'")
                .fetch_all(self.production.pool())
                .await?;
        let mut verified = None;
        for environment in &rows {
            let credentials:Option<(String,String,Option<String>,Option<i64>)>=sqlx::query_as("SELECT iam_environment_key_ciphertext,iam_app_secret_ciphertext,iam_webhook_secret_ciphertext,iam_webhook_key_version FROM dm.testing_environments WHERE environment_id=$1 AND status='active'")
                .bind(environment.environment_id).fetch_optional(self.production.pool()).await?;
            let Some((iam_key, iam_secret, webhook_secret, webhook_version)) = credentials else {
                continue;
            };
            let iam_key = self.decrypt(environment.environment_id, "iam-key", &iam_key)?;
            if !bool::from(
                iam_key
                    .expose_secret()
                    .as_bytes()
                    .ct_eq(key_hint.expose_secret().as_bytes()),
            ) {
                continue;
            }
            let mut settings = parent.settings.iam.clone();
            if let (Some(secret), Some(version)) = (webhook_secret, webhook_version) {
                settings.webhook_secret =
                    self.decrypt(environment.environment_id, "iam-webhook", &secret)?;
                settings.webhook_key_version = version;
            }
            let identity = IamClient::for_environment(
                &settings,
                &environment.iam_app_id,
                self.decrypt(environment.environment_id, "iam-app", &iam_secret)?,
                iam_key,
                environment.iam_environment_id,
            )?;
            if let Ok(event) = identity.verify_webhook(headers, body) {
                verified = Some((
                    environment.iam_environment_id,
                    environment.iam_app_id.clone(),
                    event,
                ));
                break;
            }
        }
        let Some((iam_environment_id, iam_app_id, event)) = verified else {
            return Err(AppError::Unauthorized);
        };
        let mut states = Vec::new();
        for environment in rows.into_iter().filter(|environment| {
            environment.iam_environment_id == iam_environment_id
                && environment.iam_app_id == iam_app_id
        }) {
            let stored_key:Option<String>=sqlx::query_scalar("SELECT iam_environment_key_ciphertext FROM dm.testing_environments WHERE environment_id=$1 AND status='active'")
                .bind(environment.environment_id).fetch_optional(self.production.pool()).await?;
            let Some(stored_key) = stored_key else {
                continue;
            };
            let expected = self.decrypt(environment.environment_id, "iam-key", &stored_key)?;
            if !bool::from(
                expected
                    .expose_secret()
                    .as_bytes()
                    .ct_eq(key_hint.expose_secret().as_bytes()),
            ) {
                continue;
            }
            match self.state_for_environment(parent, &environment).await {
                Ok(state) => states.push((state, event.clone())),
                Err(AppError::Unauthorized) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(states)
    }

    async fn state_for_environment(
        &self,
        parent: &AppState,
        environment: &TestingEnvironment,
    ) -> AppResult<AppState> {
        // Never hold the runtime map while acquiring a database lifecycle lock.
        // Lifecycle changes acquire those locks before removing cached runtimes.
        let _initialization = self.initialization.lock().await;
        let mut runtimes = self.runtimes.lock().await;
        if runtimes
            .get(&environment.environment_id)
            .is_some_and(|runtime| runtime.version != environment.version)
        {
            let old = runtimes.remove(&environment.environment_id);
            drop(runtimes);
            if let Some(old) = old {
                old.cancellation.cancel();
                old.realtime.disconnect_all("testing-environment-changed");
                old.store.pool().close().await;
            }
            runtimes = self.runtimes.lock().await;
        }
        if !runtimes.contains_key(&environment.environment_id) {
            drop(runtimes);
            let (iam_key, iam_secret, webhook_secret, webhook_version): (String, String, Option<String>, Option<i64>) = sqlx::query_as(
                "SELECT iam_environment_key_ciphertext, iam_app_secret_ciphertext, iam_webhook_secret_ciphertext, iam_webhook_key_version FROM dm.testing_environments WHERE environment_id = $1 AND status = 'active' AND version = $2")
                .bind(environment.environment_id).bind(environment.version).fetch_optional(self.production.pool()).await?.ok_or(AppError::Unauthorized)?;
            let mut iam_settings = parent.settings.iam.clone();
            if let (Some(secret), Some(version)) = (webhook_secret, webhook_version) {
                iam_settings.webhook_secret =
                    self.decrypt(environment.environment_id, "iam-webhook", &secret)?;
                iam_settings.webhook_key_version = version;
            }
            let identity = IamClient::for_environment(
                &iam_settings,
                &environment.iam_app_id,
                self.decrypt(environment.environment_id, "iam-app", &iam_secret)?,
                self.decrypt(environment.environment_id, "iam-key", &iam_key)?,
                environment.iam_environment_id,
            )?;
            self.upgrade_schema(environment.environment_id).await?;
            let store = PostgresStore::connect_schema(
                &self.database,
                &schema_name(environment.environment_id),
            )
            .await?;
            if let Err(error) = self
                .ensure_active(environment.environment_id, environment.version)
                .await
            {
                store.pool().close().await;
                return Err(error);
            }
            let identity = Arc::new(identity.with_directory(store.clone()));
            let realtime = RealtimeHub::default();
            let cancellation = CancellationToken::new();
            let worker = DeliveryWorker::new(
                store.clone(),
                realtime.clone(),
                parent.instance_id.clone(),
                parent.settings.worker.clone(),
            );
            let worker_cancel = cancellation.clone();
            tokio::spawn(async move {
                if let Err(error) = worker.run(worker_cancel).await {
                    tracing::error!(code = error.code(), "testing delivery worker stopped");
                }
            });
            runtimes = self.runtimes.lock().await;
            runtimes.insert(
                environment.environment_id,
                EnvironmentRuntime {
                    version: environment.version,
                    store,
                    identity,
                    realtime,
                    cancellation,
                },
            );
        }
        let runtime = runtimes
            .get(&environment.environment_id)
            .ok_or_else(|| AppError::internal(anyhow::anyhow!("missing testing runtime")))?;
        let mut selected = parent.clone();
        selected.store = runtime.store.clone();
        selected.identity = runtime.identity.clone();
        selected.realtime = runtime.realtime.clone();
        selected.testing_environment = Some(environment.environment_id);
        selected.testing_generation = Some(environment.version);
        Ok(selected)
    }

    /// Verifies a long-lived connection still belongs to an active generation.
    ///
    /// # Errors
    /// Returns unauthorized for a retired generation, or a database error.
    pub async fn ensure_active(&self, environment_id: Uuid, generation: i64) -> AppResult<()> {
        let active: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM dm.testing_environments WHERE environment_id = $1 AND status = 'active' AND version = $2)")
            .bind(environment_id).bind(generation).fetch_one(self.production.pool()).await?;
        if active {
            Ok(())
        } else {
            Err(AppError::Unauthorized)
        }
    }

    /// Marks validated ongoing realtime activity as use of the environment.
    ///
    /// # Errors
    /// Returns unauthorized for a retired generation, or a database error.
    pub async fn touch(&self, environment_id: Uuid, generation: i64) -> AppResult<()> {
        let changed = sqlx::query("UPDATE dm.testing_environments SET last_activity_at = clock_timestamp() WHERE environment_id = $1 AND version = $2 AND status = 'active'")
            .bind(environment_id).bind(generation).execute(self.production.pool()).await?.rows_affected();
        if changed == 1 {
            Ok(())
        } else {
            Err(AppError::Unauthorized)
        }
    }

    /// Holds a shared advisory lock until a request has completed. Lifecycle mutations
    /// take the exclusive lock, preventing clean/delete from racing an accepted write.
    ///
    /// # Errors
    /// Returns unauthorized for a retired generation, or database lock/acquisition errors.
    pub async fn request_fence(
        &self,
        environment_id: Uuid,
        generation: i64,
    ) -> AppResult<Transaction<'static, Postgres>> {
        let mut connection = self.admin.pool().begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock_shared($1)")
            .bind(lock_id(environment_id))
            .execute(&mut *connection)
            .await?;
        self.ensure_active(environment_id, generation).await?;
        Ok(connection)
    }

    async fn invalidate(&self, environment_id: Uuid) {
        let removed = self.runtimes.lock().await.remove(&environment_id);
        if let Some(runtime) = removed {
            runtime.cancellation.cancel();
            runtime
                .realtime
                .disconnect_all("testing-environment-changed");
            runtime.store.pool().close().await;
        }
    }

    fn encrypt(&self, id: Uuid, field: &str, value: &SecretString) -> AppResult<String> {
        let mut nonce_bytes = [0u8; 12];
        OsRng.fill_bytes(&mut nonce_bytes);
        let aad = format!("silicon-dm:{id}:{field}");
        let encrypted = self
            .cipher
            .encrypt(
                &Nonce::from(nonce_bytes),
                Payload {
                    msg: value.expose_secret().as_bytes(),
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| AppError::internal(anyhow::anyhow!("testing secret encryption failed")))?;
        let mut payload = nonce_bytes.to_vec();
        payload.extend(encrypted);
        Ok(STANDARD.encode(payload))
    }

    fn decrypt(&self, id: Uuid, field: &str, value: &str) -> AppResult<SecretString> {
        let bytes = STANDARD.decode(value).map_err(|_| secret_error())?;
        if bytes.len() < 28 {
            return Err(secret_error());
        }
        let aad = format!("silicon-dm:{id}:{field}");
        let nonce: [u8; 12] = bytes[..12].try_into().map_err(|_| secret_error())?;
        let plain = self
            .cipher
            .decrypt(
                &Nonce::from(nonce),
                Payload {
                    msg: &bytes[12..],
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| secret_error())?;
        String::from_utf8(plain)
            .map(SecretString::from)
            .map_err(|_| secret_error())
    }

    /// Runs lifecycle maintenance until the API stops, with no schema-wide data queries.
    pub async fn run_maintenance(self: Arc<Self>, cancellation: CancellationToken) {
        let mut interval = tokio::time::interval(Duration::from_secs(300));
        loop {
            tokio::select! {
                () = cancellation.cancelled() => break,
                _ = interval.tick() => {
                    if let Err(error) = self.maintain().await { tracing::error!(code=error.code(), "testing lifecycle maintenance failed"); }
                }
            }
        }
        for (_, runtime) in self.runtimes.lock().await.drain() {
            runtime.cancellation.cancel();
            runtime.realtime.disconnect_all("server-shutdown");
        }
    }
}

fn secret_error() -> AppError {
    AppError::internal(anyhow::anyhow!("testing secret could not be decrypted"))
}
fn schema_name(id: Uuid) -> String {
    format!("dm_test_{}", id.simple())
}
fn lock_id(id: Uuid) -> i64 {
    i64::from_be_bytes(id.as_bytes()[..8].try_into().unwrap_or([0; 8]))
}
fn validate_key(key: &str) -> AppResult<()> {
    if key.len() == 32 && key.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
        Ok(())
    } else {
        Err(AppError::Unauthorized)
    }
}
fn new_root_key() -> SecretString {
    const ALPHABET: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    let mut key = String::with_capacity(32);
    while key.len() < 32 {
        let mut byte = [0u8; 1];
        OsRng.fill_bytes(&mut byte);
        if byte[0] < 248 {
            key.push(char::from(ALPHABET[usize::from(byte[0] % 62)]));
        }
    }
    SecretString::from(key)
}
async fn exclusive_lock(connection: &mut PgConnection, id: Uuid) -> AppResult<()> {
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(lock_id(id))
        .execute(connection)
        .await?;
    Ok(())
}
