//! Encrypted control records, transactional schema lifecycle, and key rotation.

use secrecy::{ExposeSecret as _, SecretString};
use sqlx::{AssertSqlSafe, Row as _};
use uuid::Uuid;

use super::{TestingEnvironment, TestingRegistry, exclusive_lock, schema_name, validate_key};
use crate::{
    AppError, AppResult,
    application::{auth::AuthContext, state::AppState},
    infrastructure::{iam::IamClient, postgres::MIGRATOR},
};

impl TestingRegistry {
    pub(super) async fn list(
        &self,
        org: &str,
        include_deleted: bool,
    ) -> AppResult<Vec<TestingEnvironment>> {
        Ok(sqlx::query_as("SELECT * FROM dm.testing_environments WHERE organization_id = $1 AND (status = 'active' OR ($2 AND status = 'deleted')) ORDER BY created_at DESC, environment_id DESC")
            .bind(org).bind(include_deleted).fetch_all(self.production.pool()).await?)
    }

    pub(super) async fn get(&self, id: Uuid, org: &str) -> AppResult<TestingEnvironment> {
        sqlx::query_as("SELECT * FROM dm.testing_environments WHERE environment_id = $1 AND organization_id = $2 AND status IN ('active', 'deleted')")
            .bind(id).bind(org).fetch_optional(self.production.pool()).await?.ok_or(AppError::NotFound)
    }

    pub(super) fn authorize(environment: &TestingEnvironment, auth: &AuthContext) -> AppResult<()> {
        if environment.organization_id == auth.organization_id.as_str()
            && ((environment.creator_actor_id == auth.actor.id.as_str()
                && environment.creator_actor_kind == auth.actor.actor_type.as_str())
                || auth.is_org_administrator())
        {
            Ok(())
        } else {
            Err(AppError::Forbidden)
        }
    }

    pub(super) async fn create(
        &self,
        state: &AppState,
        auth: &AuthContext,
        input: super::handlers::CreateEnvironment,
        mutation: &super::journal::Mutation,
    ) -> AppResult<serde_json::Value> {
        if input.iam_environment_id.is_nil() || input.iam_app_id != state.settings.iam.app_id {
            return Err(AppError::validation(
                "pairing requires a non-nil IAM test environment and the imported DM application ID",
            ));
        }
        validate_name(&input.name)?;
        validate_description(input.description.as_deref())?;
        if input.iam_webhook_secret.is_some() != input.iam_webhook_key_version.is_some()
            || input
                .iam_webhook_key_version
                .is_some_and(|version| version < 1)
            || input
                .iam_webhook_secret
                .as_ref()
                .is_some_and(|secret| secret.expose_secret().len() < 32)
        {
            return Err(AppError::validation(
                "webhook signer override requires a secret of at least 32 bytes and a positive key version together",
            ));
        }
        validate_key(input.iam_environment_key.expose_secret()).map_err(|_| {
            AppError::validation("IAM testing environment key must be 32 alphanumeric characters")
        })?;
        let mut iam_settings = state.settings.iam.clone();
        if let (Some(secret), Some(version)) =
            (&input.iam_webhook_secret, input.iam_webhook_key_version)
        {
            iam_settings.webhook_secret = secret.clone();
            iam_settings.webhook_key_version = version;
        }
        let iam = IamClient::for_environment(
            &iam_settings,
            &input.iam_app_id,
            input.iam_app_secret.clone(),
            input.iam_environment_key.clone(),
            input.iam_environment_id,
        )?;
        iam.validate_environment().await?;
        let id = mutation.environment_id;
        let key = &mutation.key;
        sqlx::query("INSERT INTO dm.testing_environments (environment_id, organization_id, creator_actor_id, creator_actor_kind, name, description, iam_environment_id, iam_app_id, iam_environment_key_ciphertext, iam_app_secret_ciphertext, iam_webhook_secret_ciphertext, iam_webhook_key_version, status) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,'creating') ON CONFLICT(environment_id) DO NOTHING")
            .bind(id).bind(auth.organization_id.as_str()).bind(auth.actor.id.as_str()).bind(auth.actor.actor_type.as_str())
            .bind(input.name.trim()).bind(input.description).bind(input.iam_environment_id).bind(input.iam_app_id)
            .bind(self.encrypt(id, "iam-key", &input.iam_environment_key)?).bind(self.encrypt(id,"iam-app",&input.iam_app_secret)?)
            .bind(input.iam_webhook_secret.as_ref().map(|secret|self.encrypt(id,"iam-webhook",secret)).transpose()?)
            .bind(input.iam_webhook_key_version)
            .execute(self.production.pool()).await?;
        if let Err(error) = self.upgrade_schema(id).await {
            self.drop_schema(id).await?;
            sqlx::query(
                "DELETE FROM dm.testing_environments WHERE environment_id=$1 AND status='creating'",
            )
            .bind(id)
            .execute(self.production.pool())
            .await?;
            return Err(error);
        }
        let mut transaction = self.production.pool().begin().await?;
        let environment:TestingEnvironment=sqlx::query_as("UPDATE dm.testing_environments SET status='active', root_key_digest=$2, root_key_ciphertext=$3 WHERE environment_id=$1 AND status='creating' RETURNING *")
            .bind(id).bind(blake3::hash(key.expose_secret().as_bytes()).as_bytes().as_slice())
            .bind(self.encrypt(id,"root",key)?).fetch_optional(&mut *transaction).await?.ok_or_else(||AppError::conflict("environment creation is not resumable in its current state"))?;
        let response = super::handlers::environment_with_key(environment, key)?;
        self.complete_mutation(&mut transaction, mutation, &response)
            .await?;
        transaction.commit().await?;
        Ok(response)
    }

    pub(super) async fn key(&self, id: Uuid, auth: &AuthContext) -> AppResult<SecretString> {
        let environment = self.get(id, auth.organization_id.as_str()).await?;
        Self::authorize(&environment, auth)?;
        let encrypted: Option<String> = sqlx::query_scalar("SELECT root_key_ciphertext FROM dm.testing_environments WHERE environment_id=$1 AND status='active'")
            .bind(id).fetch_optional(self.production.pool()).await?.flatten();
        self.decrypt(
            id,
            "root",
            &encrypted.ok_or_else(|| {
                AppError::conflict(
                    "deleted environments have no key; restore the environment first",
                )
            })?,
        )
    }

    pub(super) async fn update(
        &self,
        id: Uuid,
        auth: &AuthContext,
        name: Option<String>,
        description: Option<String>,
        mutation: &super::journal::Mutation,
    ) -> AppResult<serde_json::Value> {
        let environment = self.get(id, auth.organization_id.as_str()).await?;
        Self::authorize(&environment, auth)?;
        if let Some(value) = &name {
            validate_name(value)?;
        }
        validate_description(description.as_deref())?;
        let mut transaction = self.production.pool().begin().await?;
        let row:TestingEnvironment=sqlx::query_as("UPDATE dm.testing_environments SET name=COALESCE($2,name), description=COALESCE($3,description), last_activity_at=clock_timestamp() WHERE environment_id=$1 AND status='active' RETURNING *")
            .bind(id).bind(name.map(|value|value.trim().to_owned())).bind(description).fetch_optional(&mut *transaction).await?.ok_or_else(||AppError::conflict("only active environments can be changed"))?;
        let response = serde_json::to_value(row).map_err(AppError::internal)?;
        self.complete_mutation(&mut transaction, mutation, &response)
            .await?;
        transaction.commit().await?;
        Ok(response)
    }

    pub(super) async fn rotate(
        &self,
        id: Uuid,
        auth: &AuthContext,
        restore: bool,
        mutation: &super::journal::Mutation,
    ) -> AppResult<serde_json::Value> {
        let environment = self.get(id, auth.organization_id.as_str()).await?;
        Self::authorize(&environment, auth)?;
        let mut fence = self.admin.pool().begin().await?;
        exclusive_lock(&mut fence, id).await?;
        let key = &mutation.key;
        let status = if restore { "deleted" } else { "active" };
        let mut transaction = self.production.pool().begin().await?;
        let environment:TestingEnvironment=sqlx::query_as("UPDATE dm.testing_environments SET status='active', root_key_digest=$2, root_key_ciphertext=$3, deleted_at=NULL, purge_after=NULL, last_activity_at=clock_timestamp(), version=version+1 WHERE environment_id=$1 AND status=$4 AND (purge_after IS NULL OR purge_after > clock_timestamp()) RETURNING *")
            .bind(id).bind(blake3::hash(key.expose_secret().as_bytes()).as_bytes().as_slice()).bind(self.encrypt(id,"root",key)?).bind(status).fetch_optional(&mut *transaction).await?.ok_or_else(||AppError::conflict("environment is not in the required state or its recovery window expired"))?;
        let response = super::handlers::environment_with_key(environment, key)?;
        self.complete_mutation(&mut transaction, mutation, &response)
            .await?;
        transaction.commit().await?;
        self.invalidate(id).await;
        fence.commit().await?;
        Ok(response)
    }

    pub(super) async fn delete(
        &self,
        id: Uuid,
        auth: &AuthContext,
        mutation: &super::journal::Mutation,
    ) -> AppResult<()> {
        let environment = self.get(id, auth.organization_id.as_str()).await?;
        Self::authorize(&environment, auth)?;
        let mut fence = self.admin.pool().begin().await?;
        exclusive_lock(&mut fence, id).await?;
        let mut transaction = self.production.pool().begin().await?;
        sqlx::query("UPDATE dm.testing_environments SET status='deleted', root_key_digest=NULL, root_key_ciphertext=NULL, deleted_at=clock_timestamp(), purge_after=clock_timestamp()+INTERVAL '30 days', version=version+1 WHERE environment_id=$1 AND status='active'")
            .bind(id).execute(&mut *transaction).await?;
        self.complete_mutation(&mut transaction, mutation, &serde_json::Value::Null)
            .await?;
        transaction.commit().await?;
        self.invalidate(id).await;
        fence.commit().await?;
        Ok(())
    }

    pub(super) async fn clean(
        &self,
        id: Uuid,
        root_key: Option<&str>,
        auth: Option<&AuthContext>,
        mutation: &super::journal::Mutation,
    ) -> AppResult<()> {
        let mut fence = self.admin.pool().begin().await?;
        exclusive_lock(&mut fence, id).await?;
        if let Some(key) = root_key {
            validate_key(key)?;
            let valid:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM dm.testing_environments WHERE environment_id=$1 AND status='active' AND root_key_digest=$2)")
                .bind(id).bind(blake3::hash(key.as_bytes()).as_bytes().as_slice()).fetch_one(self.production.pool()).await?;
            if !valid {
                return Err(AppError::Unauthorized);
            }
        } else {
            let auth = auth.ok_or(AppError::Unauthorized)?;
            let environment = self.get(id, auth.organization_id.as_str()).await?;
            Self::authorize(&environment, auth)?;
            if environment.status != "active" {
                return Err(AppError::conflict(
                    "restore the environment before cleaning it",
                ));
            }
        }
        let schema = schema_name(id);
        let already_cleaned: bool = sqlx::query_scalar(AssertSqlSafe(format!(
            "SELECT EXISTS(SELECT 1 FROM {schema}.__dm_clean_receipts WHERE mutation_id=$1)"
        )))
        .bind(mutation.id)
        .fetch_one(&mut *fence)
        .await?;
        if !already_cleaned {
            self.invalidate(id).await;
            let tables:Vec<String>=sqlx::query_scalar("SELECT tablename::text FROM pg_tables WHERE schemaname=$1 AND tablename NOT IN ('__dm_migrations', '__dm_clean_receipts', 'iam_authorization_revision') ORDER BY tablename")
                .bind(&schema).fetch_all(&mut *fence).await?;
            if !tables.is_empty() {
                let qualified = tables
                    .iter()
                    .map(|name| format!("{schema}.\"{}\"", name.replace('"', "\"\"")))
                    .collect::<Vec<_>>()
                    .join(",");
                sqlx::raw_sql(AssertSqlSafe(format!(
                    "TRUNCATE {qualified} RESTART IDENTITY"
                )))
                .execute(&mut *fence)
                .await?;
            }
            sqlx::query(AssertSqlSafe(format!(
                "UPDATE {schema}.iam_authorization_revision SET revision=revision+1 WHERE singleton"
            )))
            .execute(&mut *fence)
            .await?;
            sqlx::query(AssertSqlSafe(format!(
                "INSERT INTO {schema}.__dm_clean_receipts(mutation_id) VALUES ($1)"
            )))
            .bind(mutation.id)
            .execute(&mut *fence)
            .await?;
        }
        // The marker and destructive effect commit together. If the process dies
        // before the production response journal commits, retry sees the marker
        // and cannot erase messages accepted after this clean completed.
        fence.commit().await?;
        let mut transaction = self.production.pool().begin().await?;
        sqlx::query("UPDATE dm.testing_environments SET version=version+1,last_activity_at=clock_timestamp() WHERE environment_id=$1 AND status='active'")
            .bind(id).execute(&mut *transaction).await?;
        self.complete_mutation(&mut transaction, mutation, &serde_json::Value::Null)
            .await?;
        transaction.commit().await?;
        Ok(())
    }

    pub(super) async fn upgrade_schema(&self, id: Uuid) -> AppResult<()> {
        let schema = schema_name(id);
        let mut transaction = self.admin.pool().begin().await?;
        exclusive_lock(&mut transaction, id).await?;
        let live:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM dm.testing_environments WHERE environment_id=$1 AND status IN ('active','creating'))").bind(id).fetch_one(self.production.pool()).await?;
        if !live {
            return Err(AppError::Unauthorized);
        }
        sqlx::raw_sql(AssertSqlSafe(format!("CREATE SCHEMA IF NOT EXISTS {schema}; CREATE TABLE IF NOT EXISTS {schema}.__dm_migrations (version bigint PRIMARY KEY, checksum bytea NOT NULL, testing_environment_id uuid NOT NULL DEFAULT '{id}' CHECK(testing_environment_id='{id}'))")))
            .execute(&mut *transaction).await?;
        sqlx::query("SELECT set_config('search_path',$1,true)")
            .bind(&schema)
            .execute(&mut *transaction)
            .await?;
        sqlx::raw_sql(AssertSqlSafe(format!("CREATE TABLE IF NOT EXISTS {schema}.__dm_clean_receipts (mutation_id uuid PRIMARY KEY, completed_at timestamptz NOT NULL DEFAULT clock_timestamp(), testing_environment_id uuid NOT NULL DEFAULT '{id}' CHECK(testing_environment_id='{id}'))"))).execute(&mut *transaction).await?;
        for migration in MIGRATOR
            .iter()
            .filter(|migration| !matches!(migration.version, 6 | 8 | 11))
        {
            let existing: Option<Vec<u8>> = sqlx::query_scalar(AssertSqlSafe(format!(
                "SELECT checksum FROM {schema}.__dm_migrations WHERE version=$1"
            )))
            .bind(migration.version)
            .fetch_optional(&mut *transaction)
            .await?;
            if let Some(checksum) = existing {
                if checksum != migration.checksum.as_ref() {
                    return Err(AppError::internal(anyhow::anyhow!(
                        "testing schema migration checksum mismatch"
                    )));
                }
                continue;
            }
            let sql = scope_sql(migration.sql.as_str(), &schema);
            sqlx::raw_sql(AssertSqlSafe(sql))
                .execute(&mut *transaction)
                .await?;
            sqlx::query(AssertSqlSafe(format!(
                "INSERT INTO {schema}.__dm_migrations(version,checksum) VALUES ($1,$2)"
            )))
            .bind(migration.version)
            .bind(migration.checksum.as_ref())
            .execute(&mut *transaction)
            .await?;
        }
        let rows = sqlx::query("SELECT tablename::text FROM pg_tables WHERE schemaname=$1")
            .bind(&schema)
            .fetch_all(&mut *transaction)
            .await?;
        for row in rows {
            let table: String = row.try_get("tablename")?;
            sqlx::raw_sql(AssertSqlSafe(format!("ALTER TABLE {schema}.\"{}\" ADD COLUMN IF NOT EXISTS testing_environment_id uuid NOT NULL DEFAULT '{id}' CHECK (testing_environment_id='{id}')",table.replace('"',"\"\""))))
                .execute(&mut *transaction).await?;
        }
        transaction.commit().await?;
        Ok(())
    }

    async fn drop_schema(&self, id: Uuid) -> AppResult<()> {
        let schema = schema_name(id);
        sqlx::raw_sql(AssertSqlSafe(format!("DROP SCHEMA IF EXISTS {schema} CASCADE; DROP SCHEMA IF EXISTS {schema}_private CASCADE;")))
            .execute(self.admin.pool()).await?;
        Ok(())
    }

    pub(super) async fn maintain(&self) -> AppResult<()> {
        let idle:Vec<Uuid>=sqlx::query_scalar("SELECT environment_id FROM dm.testing_environments WHERE status='active' AND last_activity_at <= clock_timestamp()-INTERVAL '15 days'")
            .fetch_all(self.production.pool()).await?;
        for id in idle {
            let mut fence = self.admin.pool().begin().await?;
            exclusive_lock(&mut fence, id).await?;
            let changed=sqlx::query("UPDATE dm.testing_environments SET status='deleted',root_key_digest=NULL,root_key_ciphertext=NULL,deleted_at=clock_timestamp(),purge_after=clock_timestamp()+INTERVAL '30 days',version=version+1 WHERE environment_id=$1 AND status='active' AND last_activity_at <= clock_timestamp()-INTERVAL '15 days'")
                .bind(id).execute(self.production.pool()).await?.rows_affected();
            if changed == 1 {
                self.invalidate(id).await;
            }
            fence.commit().await?;
        }
        let expired:Vec<Uuid>=sqlx::query_scalar("SELECT environment_id FROM dm.testing_environments WHERE (status='deleted' AND purge_after<=clock_timestamp()) OR status='purging' OR (status='creating' AND created_at<=clock_timestamp()-INTERVAL '1 hour')")
            .fetch_all(self.production.pool()).await?;
        for id in expired {
            let mut fence = self.admin.pool().begin().await?;
            exclusive_lock(&mut fence, id).await?;
            let claimed=sqlx::query("UPDATE dm.testing_environments SET status='purging',deleted_at=COALESCE(deleted_at,clock_timestamp()),purge_after=COALESCE(purge_after,clock_timestamp()),version=version+1 WHERE environment_id=$1 AND ((status='deleted' AND purge_after<=clock_timestamp()) OR status='purging' OR (status='creating' AND created_at<=clock_timestamp()-INTERVAL '1 hour'))")
                .bind(id).execute(self.production.pool()).await?.rows_affected();
            if claimed == 1 {
                self.invalidate(id).await;
                let schema = schema_name(id);
                sqlx::raw_sql(AssertSqlSafe(format!("DROP SCHEMA IF EXISTS {schema} CASCADE; DROP SCHEMA IF EXISTS {schema}_private CASCADE")))
                    .execute(&mut *fence).await?;
                fence.commit().await?;
                let mut transaction = self.production.pool().begin().await?;
                sqlx::query("DELETE FROM dm.testing_mutations WHERE environment_id=$1")
                    .bind(id)
                    .execute(&mut *transaction)
                    .await?;
                sqlx::query("DELETE FROM dm.testing_environments WHERE environment_id=$1 AND status='purging'").bind(id).execute(&mut *transaction).await?;
                transaction.commit().await?;
            } else {
                fence.commit().await?;
            }
        }
        Ok(())
    }
}

fn validate_name(value: &str) -> AppResult<()> {
    if value.trim().is_empty() || value.chars().count() > 128 || value.chars().any(char::is_control)
    {
        Err(AppError::validation(
            "name must contain 1 to 128 characters without control characters",
        ))
    } else {
        Ok(())
    }
}
fn validate_description(value: Option<&str>) -> AppResult<()> {
    if value.is_some_and(|value| value.chars().count() > 4096) {
        Err(AppError::validation(
            "description must not exceed 4096 characters",
        ))
    } else {
        Ok(())
    }
}
// Replace identifier tokens rather than substrings: dm_delivery notification
// channels and descriptive strings retain their original names.
fn scope_sql(sql: &str, schema: &str) -> String {
    let mut result = String::with_capacity(sql.len());
    let mut token = String::new();
    for character in sql.chars().chain(std::iter::once('\0')) {
        if character.is_ascii_alphanumeric() || character == '_' {
            token.push(character);
            continue;
        }
        match token.as_str() {
            "dm" => result.push_str(schema),
            "dm_private" => {
                result.push_str(schema);
                result.push_str("_private");
            }
            _ => result.push_str(&token),
        }
        token.clear();
        if character != '\0' {
            result.push(character);
        }
    }
    result
}
