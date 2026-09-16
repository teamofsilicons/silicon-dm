//! Live IAM validation and idempotent creation of secret-selected sandboxes.
use super::{TestingEnvironment, TestingRegistry, exclusive_lock, schema_name};
use crate::{AppError, AppResult, application::state::AppState, infrastructure::iam::IamClient};
use secrecy::{ExposeSecret as _, SecretString};
use sqlx::AssertSqlSafe;
use std::sync::Arc;
use time::OffsetDateTime;

impl TestingRegistry {
    pub(super) async fn state_for_app_secret(
        &self,
        parent: &AppState,
        secret: &SecretString,
    ) -> AppResult<AppState> {
        let (identity, current) = IamClient::discover(&parent.settings.iam, secret.clone()).await?;
        let meta = current.environment.as_ref().ok_or(AppError::Unauthorized)?;
        if !matches!(meta.creator_type.as_str(), "carbon" | "silicon") || meta.creator_id.is_empty()
        {
            return Err(AppError::Unauthorized);
        }
        let id = current.environment_id;
        let managed: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM dm.honeycomb_environments WHERE environment_id=$1)",
        )
        .bind(id)
        .fetch_one(self.production.pool())
        .await?;
        if managed {
            return self.managed_state(parent, secret, &current, identity).await;
        }
        if self.honeycomb_service_token.is_some() {
            return Err(AppError::conflict(
                "Honeycomb must prepare DM before this sandbox can be used",
            ));
        }
        let digest = blake3::hash(secret.expose_secret().as_bytes());
        let unchanged: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM dm.testing_environments WHERE environment_id=$1 AND status='active' AND iam_control_version=$2 AND root_key_digest=$3 AND NOT iam_sync_pending)")
            .bind(id).bind(meta.version).bind(digest.as_bytes().as_slice()).fetch_one(self.production.pool()).await?;
        if !unchanged {
            let mut fence = self.admin.pool().begin().await?;
            exclusive_lock(&mut fence, id).await?;
            let managed_now: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM dm.honeycomb_environments WHERE environment_id=$1)",
            )
            .bind(id)
            .fetch_one(self.production.pool())
            .await?;
            if managed_now {
                return Err(AppError::conflict(
                    "Honeycomb adopted this environment; retry discovery",
                ));
            }

            let unchanged_now: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM dm.testing_environments WHERE environment_id=$1 AND status='active' AND iam_control_version=$2 AND root_key_digest=$3 AND NOT iam_sync_pending)")
                .bind(id).bind(meta.version).bind(digest.as_bytes().as_slice()).fetch_one(self.production.pool()).await?;
            if unchanged_now {
                fence.commit().await?;
            } else {
                let mut tx = self.production.pool().begin().await?;
                let prior: Option<(String, Option<i64>, Option<OffsetDateTime>, bool)> = sqlx::query_as("SELECT status,iam_control_version,iam_cleaned_at,iam_sync_pending FROM dm.testing_environments WHERE environment_id=$1 FOR UPDATE")
                .bind(id).fetch_optional(&mut *tx).await?;
                if let Some((status, revision, _, _)) = &prior
                    && (status != "active" || revision.is_some_and(|v| v > meta.version))
                {
                    return Err(AppError::Unauthorized);
                }
                let reset = prior.as_ref().is_some_and(|(_, _, cleaned, pending)| {
                    *pending || *cleaned != meta.cleaned_at
                });
                let root = self.encrypt(id, "root", secret)?;
                let app = self.encrypt(id, "iam-app", secret)?;
                sqlx::query("INSERT INTO dm.testing_environments(environment_id,organization_id,creator_actor_id,creator_actor_kind,name,description,iam_environment_id,iam_app_id,iam_environment_key_ciphertext,iam_app_secret_ciphertext,root_key_digest,root_key_ciphertext,status,iam_control_version,iam_cleaned_at,iam_sync_pending) VALUES($1,$2,$3,$4,$5,$6,$1,$7,'',$8,$9,$10,'active',$11,$12,$13) ON CONFLICT(environment_id) DO UPDATE SET name=EXCLUDED.name,description=EXCLUDED.description,iam_app_secret_ciphertext=EXCLUDED.iam_app_secret_ciphertext,root_key_digest=EXCLUDED.root_key_digest,root_key_ciphertext=EXCLUDED.root_key_ciphertext,iam_control_version=EXCLUDED.iam_control_version,iam_cleaned_at=EXCLUDED.iam_cleaned_at,iam_sync_pending=EXCLUDED.iam_sync_pending,version=dm.testing_environments.version+CASE WHEN EXCLUDED.iam_sync_pending THEN 1 ELSE 0 END,last_activity_at=clock_timestamp()")
                .bind(id).bind(&meta.org_id).bind(&meta.creator_id).bind(&meta.creator_type).bind(&meta.name).bind(&meta.description).bind(&current.application.app_id)
                .bind(app).bind(digest.as_bytes().as_slice()).bind(root).bind(meta.version).bind(meta.cleaned_at).bind(reset).execute(&mut *tx).await?;
                let generation: i64 = sqlx::query_scalar(
                    "SELECT version FROM dm.testing_environments WHERE environment_id=$1",
                )
                .bind(id)
                .fetch_one(&mut *tx)
                .await?;
                tx.commit().await?;
                self.invalidate(id).await;
                if reset {
                    let schema = schema_name(id);
                    sqlx::raw_sql(AssertSqlSafe(format!("DROP SCHEMA IF EXISTS {schema} CASCADE; DROP SCHEMA IF EXISTS {schema}_private CASCADE"))).execute(&mut *fence).await?;
                }
                fence.commit().await?;
                sqlx::query("UPDATE dm.testing_environments SET iam_sync_pending=false WHERE environment_id=$1 AND version=$2")
                .bind(id).bind(generation).execute(self.production.pool()).await?;
            }
        }
        let row: TestingEnvironment = sqlx::query_as("UPDATE dm.testing_environments SET last_activity_at=clock_timestamp() WHERE environment_id=$1 AND status='active' AND NOT iam_sync_pending AND iam_control_version=$2 RETURNING *")
            .bind(id).bind(meta.version).fetch_optional(self.production.pool()).await?.ok_or(AppError::Unauthorized)?;
        let mut state = self.state_for_environment(parent, &row).await?;
        state.identity = Arc::new(identity.with_directory(state.store.clone()));
        Ok(state)
    }

    async fn managed_state(
        &self,
        parent: &AppState,
        secret: &SecretString,
        current: &silicon_iam_client::models::ApplicationTestingContext,
        identity: IamClient,
    ) -> AppResult<AppState> {
        let id = current.environment_id;
        let meta = current.environment.as_ref().ok_or(AppError::Unauthorized)?;
        let mut fence = self.admin.pool().begin().await?;
        exclusive_lock(&mut fence, id).await?;
        let row: TestingEnvironment = sqlx::query_as("SELECT e.* FROM dm.testing_environments e JOIN dm.honeycomb_environments h USING(environment_id) WHERE e.environment_id=$1 AND e.organization_id=$2 AND h.app_id=$3 AND h.state='active' AND NOT e.iam_sync_pending")
            .bind(id).bind(&meta.org_id).bind(&current.application.app_id).fetch_optional(self.production.pool()).await?.ok_or(AppError::Unauthorized)?;
        let stored: String = sqlx::query_scalar(
            "SELECT iam_app_secret_ciphertext FROM dm.testing_environments WHERE environment_id=$1",
        )
        .bind(id)
        .fetch_one(self.production.pool())
        .await?;
        let changed = !stored.is_empty()
            && self.decrypt(id, "iam-app", &stored)?.expose_secret() != secret.expose_secret();
        sqlx::query("UPDATE dm.testing_environments SET iam_app_secret_ciphertext=$2,iam_control_version=$3,iam_cleaned_at=$4,version=version+$5 WHERE environment_id=$1")
            .bind(id).bind(self.encrypt(id,"iam-app",secret)?).bind(meta.version).bind(meta.cleaned_at).bind(i64::from(changed)).execute(self.production.pool()).await?;
        if changed {
            self.invalidate(id).await;
        }
        fence.commit().await?;
        let mut row = row;
        row.version += i64::from(changed);
        let mut selected = self.state_for_environment(parent, &row).await?;
        selected.identity = Arc::new(identity.with_directory(selected.store.clone()));
        Ok(selected)
    }

    /// Public metadata for the selected environment; never returns any credentials.
    /// # Errors
    /// Rejects unavailable or unknown environments.
    pub async fn selected_metadata(&self, id: uuid::Uuid) -> AppResult<TestingEnvironment> {
        sqlx::query_as("SELECT * FROM dm.testing_environments WHERE environment_id=$1 AND status='active' AND NOT iam_sync_pending")
            .bind(id).fetch_optional(self.production.pool()).await?.ok_or(AppError::Unauthorized)
    }
}
