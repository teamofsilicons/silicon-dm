//! Durable Honeycomb lifecycle participant, independent of runtime test sessions.
use super::{TestingRegistry, exclusive_lock, schema_name, validate_key};
use crate::{AppError, AppResult, application::state::AppState};
use axum::{
    Json,
    extract::{Path, State},
    http::HeaderMap,
};
use secrecy::{ExposeSecret as _, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{AssertSqlSafe, FromRow};
use subtle::ConstantTimeEq as _;
use uuid::Uuid;

/// Coordinator instruction. Secret-bearing bodies are hashed, never journaled.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Operation {
    /// Stable operation identity.
    pub operation_id: Uuid,
    /// Shared sandbox identity.
    pub environment_id: Uuid,
    /// Production organization owner.
    pub org_id: String,
    /// Canonical participant application.
    pub app_id: String,
    /// Monotonic coordinator revision.
    pub environment_revision: i64,
    /// Shared cleaning generation.
    pub generation: i64,
    /// Shared root key version.
    pub key_version: i64,
    /// Lifecycle action.
    pub action: String,
    /// Shared root key, encrypted at rest.
    pub testing_key: String,
    /// Display name.
    #[serde(default)]
    pub name: Option<String>,
    /// Description.
    #[serde(default)]
    pub description: Option<String>,
    /// Accepted application configuration.
    #[serde(default)]
    pub snapshot: Value,
    /// Coordinator reason.
    #[serde(default)]
    pub reason: String,
    /// Selected applications for retirement.
    #[serde(default)]
    pub retired_apps: Vec<String>,
}
impl Operation {
    fn validate(&self, app: &str) -> AppResult<()> {
        validate_key(&self.testing_key)?;
        if self.app_id != app
            || self.environment_id.is_nil()
            || self.operation_id.is_nil()
            || self.org_id.is_empty()
            || self.org_id.len() > 128
            || self.environment_revision < 1
            || self.generation < 1
            || self.key_version < 1
            || self
                .name
                .as_ref()
                .is_some_and(|n| n.trim().is_empty() || n.chars().count() > 128)
            || self
                .description
                .as_ref()
                .is_some_and(|d| d.chars().count() > 4096)
            || !matches!(
                self.action.as_str(),
                "prepare"
                    | "import"
                    | "refresh-import"
                    | "rotate-key"
                    | "clean"
                    | "disable"
                    | "restore"
                    | "purge"
                    | "retire-applications"
            )
        {
            return Err(AppError::validation(
                "invalid Honeycomb lifecycle instruction",
            ));
        }
        Ok(())
    }
    fn retires(&self) -> bool {
        self.action == "retire-applications" && self.retired_apps.contains(&self.app_id)
    }
    fn receipt(&self, state: &str) -> Value {
        json!({"operation_id":self.operation_id,"environment_id":self.environment_id,
            "app_id":self.app_id,"environment_revision":self.environment_revision,
            "generation":self.generation,"key_version":self.key_version,"state":state,
            "retired_apps":self.retired_apps})
    }
}
#[derive(FromRow)]
struct Prior {
    organization_id: String,
    app_id: String,
    environment_revision: i64,
    generation: i64,
    key_version: i64,
    operation_id: Uuid,
    state: String,
    testing_key_ciphertext: String,
}
impl TestingRegistry {
    pub(crate) async fn is_honeycomb_environment(&self, id: Uuid) -> AppResult<bool> {
        Ok(sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM dm.honeycomb_environments WHERE environment_id=$1)",
        )
        .bind(id)
        .fetch_one(self.production.pool())
        .await?)
    }

    fn authenticate_honeycomb(&self, headers: &HeaderMap) -> AppResult<()> {
        let expected = self
            .honeycomb_service_token
            .as_ref()
            .ok_or(AppError::Unauthorized)?;
        if headers.get_all("authorization").iter().count() != 1 {
            return Err(AppError::Unauthorized);
        }
        let token = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or(AppError::Unauthorized)?;
        if bool::from(
            blake3::hash(token.as_bytes())
                .as_bytes()
                .ct_eq(blake3::hash(expected.expose_secret().as_bytes()).as_bytes()),
        ) {
            Ok(())
        } else {
            Err(AppError::Unauthorized)
        }
    }

    /// Apply or replay one authenticated operation. Failures leave access fenced.
    /// # Errors
    /// Rejects altered retries, stale versions, wrong identities and invalid transitions.
    pub async fn honeycomb_operation(&self, op: &Operation, app: &str) -> AppResult<Value> {
        op.validate(app)?;
        let hash = blake3::hash(&serde_json::to_vec(op).map_err(AppError::internal)?);
        let result = self.execute_operation(op, hash.as_bytes()).await;
        if result.is_err() {
            sqlx::query("UPDATE dm.honeycomb_operations SET receipt=jsonb_set(receipt,'{state}',to_jsonb('failed'::text)) WHERE environment_id=$1 AND operation_id=$2 AND request_hash=$3 AND receipt->>'state'<>'completed'")
                .bind(op.environment_id).bind(op.operation_id).bind(hash.as_bytes().as_slice()).execute(self.production.pool()).await?;
        }
        result
    }

    #[allow(
        clippy::too_many_lines,
        reason = "cross-database claim, effect and receipt ordering must stay explicit"
    )]
    async fn execute_operation(&self, op: &Operation, hash: &[u8]) -> AppResult<Value> {
        let id = op.environment_id;
        // Every API/socket write holds the shared counterpart of this lock.
        let mut fence = self.admin.pool().begin().await?;
        exclusive_lock(&mut fence, id).await?;
        let mut tx = self.production.pool().begin().await?;
        let replay: Option<(Vec<u8>, Value)> = sqlx::query_as("SELECT request_hash,receipt FROM dm.honeycomb_operations WHERE environment_id=$1 AND operation_id=$2")
            .bind(id).bind(op.operation_id).fetch_optional(&mut *tx).await?;
        if let Some((prior_hash, receipt)) = &replay {
            if prior_hash != hash {
                return Err(AppError::conflict(
                    "Honeycomb operation ID was reused with a different request",
                ));
            }
            if receipt["state"] == "completed" {
                return Ok(receipt.clone());
            }
        }
        let prior: Option<Prior> = sqlx::query_as(
            "SELECT * FROM dm.honeycomb_environments WHERE environment_id=$1 FOR UPDATE",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(prior) = &prior {
            let retry = prior.operation_id == op.operation_id;
            let reimport =
                prior.state == "retired" && matches!(op.action.as_str(), "prepare" | "import");
            if prior.organization_id != op.org_id
                || prior.app_id != op.app_id
                || (!retry && prior.environment_revision >= op.environment_revision)
                || op.generation < prior.generation
                || op.key_version < prior.key_version
                || (!retry
                    && op.generation != prior.generation
                    && op.action != "clean"
                    && !reimport)
                || (!retry
                    && op.key_version != prior.key_version
                    && op.action != "rotate-key"
                    && !reimport)
                || (!retry && op.action == "clean" && op.generation <= prior.generation)
                || (!retry && op.action == "rotate-key" && op.key_version <= prior.key_version)
                || prior.state == "purged"
                || (!retry && prior.state == "pending")
                || (prior.state == "retired" && !reimport && op.action != "purge")
                || (prior.state == "disabled"
                    && !matches!(
                        op.action.as_str(),
                        "restore" | "purge" | "disable" | "clean" | "rotate-key"
                    ))
            {
                return Err(AppError::conflict(
                    "stale or incompatible Honeycomb lifecycle operation",
                ));
            }
            let same_key = self
                .decrypt(id, "honeycomb-key", &prior.testing_key_ciphertext)?
                .expose_secret()
                == op.testing_key;
            if (op.key_version == prior.key_version && !same_key)
                || (!retry && op.action == "rotate-key" && same_key)
            {
                return Err(AppError::conflict(
                    "Honeycomb key version does not match its key",
                ));
            }
        } else if !matches!(op.action.as_str(), "prepare" | "import") {
            return Err(AppError::conflict(
                "Honeycomb must prepare the environment first",
            ));
        }
        // Preserve the resulting state in the pending receipt so retries of clean
        // or rotate on a disabled environment cannot accidentally restore access.
        let final_state = replay
            .as_ref()
            .and_then(|(_, r)| r["target_state"].as_str())
            .map_or_else(
                || {
                    if op.action == "purge" {
                        "purged"
                    } else if op.retires() {
                        "retired"
                    } else if op.action == "disable"
                        || (prior.as_ref().is_some_and(|p| p.state == "disabled")
                            && op.action != "restore")
                    {
                        "disabled"
                    } else {
                        "active"
                    }
                },
                |state| state,
            )
            .to_owned();
        let mut pending = op.receipt("pending");
        pending["target_state"] = json!(final_state);
        if replay.is_none() {
            // Existing auto-discovered rows may be adopted, but never across organizations.
            let owner: Option<(String, String)> = sqlx::query_as("SELECT organization_id,iam_app_id FROM dm.testing_environments WHERE environment_id=$1 FOR UPDATE")
                .bind(id).fetch_optional(&mut *tx).await?;
            if owner.is_some_and(|(org, app)| org != op.org_id || app != op.app_id) {
                return Err(AppError::Forbidden);
            }
            let root = SecretString::from(op.testing_key.clone());
            sqlx::query("INSERT INTO dm.honeycomb_environments(environment_id,organization_id,app_id,environment_revision,generation,key_version,operation_id,state,testing_key_ciphertext) VALUES($1,$2,$3,$4,$5,$6,$7,'pending',$8) ON CONFLICT(environment_id) DO UPDATE SET environment_revision=$4,generation=$5,key_version=$6,operation_id=$7,state='pending',testing_key_ciphertext=$8")
                .bind(id).bind(&op.org_id).bind(&op.app_id).bind(op.environment_revision).bind(op.generation).bind(op.key_version).bind(op.operation_id).bind(self.encrypt(id,"honeycomb-key",&root)?).execute(&mut *tx).await?;
            sqlx::query("INSERT INTO dm.testing_environments(environment_id,organization_id,creator_actor_id,creator_actor_kind,name,description,iam_environment_id,iam_app_id,iam_environment_key_ciphertext,iam_app_secret_ciphertext,root_key_digest,root_key_ciphertext,status,iam_sync_pending) VALUES($1,$2,'honeycomb','silicon',$3,$4,$1,$5,'','',$6,$7,'active',true) ON CONFLICT(environment_id) DO UPDATE SET name=$3,description=$4,status='active',deleted_at=NULL,purge_after=NULL,root_key_digest=$6,root_key_ciphertext=$7,iam_environment_key_ciphertext='',iam_sync_pending=true,version=dm.testing_environments.version+1")
                .bind(id).bind(&op.org_id).bind(op.name.as_deref().unwrap_or("Honeycomb sandbox")).bind(&op.description).bind(&op.app_id).bind(blake3::hash(op.testing_key.as_bytes()).as_bytes().as_slice()).bind(self.encrypt(id,"root",&root)?).execute(&mut *tx).await?;
            if op.action == "clean" || op.retires() || op.action == "purge" {
                sqlx::query("UPDATE dm.honeycomb_environments SET last_activity_at=NULL,activity_reported_at=NULL WHERE environment_id=$1").bind(id).execute(&mut *tx).await?;
            }
            sqlx::query("INSERT INTO dm.honeycomb_operations(environment_id,operation_id,request_hash,receipt) VALUES($1,$2,$3,$4)")
                .bind(id).bind(op.operation_id).bind(hash).bind(&pending).execute(&mut *tx).await?;
        } else {
            sqlx::query("UPDATE dm.honeycomb_operations SET receipt=$3 WHERE environment_id=$1 AND operation_id=$2")
                .bind(id).bind(op.operation_id).bind(&pending).execute(&mut *tx).await?;
        }
        tx.commit().await?; // Durable fence survives a crash before or after the effect.
        self.invalidate(id).await;
        if op.action == "clean" || op.action == "purge" || op.retires() {
            let schema = schema_name(id);
            sqlx::raw_sql(AssertSqlSafe(format!("DROP SCHEMA IF EXISTS {schema} CASCADE; DROP SCHEMA IF EXISTS {schema}_private CASCADE"))).execute(&mut *fence).await?;
        }
        if !matches!(final_state.as_str(), "purged" | "retired") {
            self.upgrade_schema_locked(id, &mut fence).await?;
        }
        fence.commit().await?; // Cleanup must finish before completion becomes visible.
        let mut tx = self.production.pool().begin().await?;
        sqlx::query("UPDATE dm.honeycomb_environments SET state=$2 WHERE environment_id=$1")
            .bind(id)
            .bind(&final_state)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "UPDATE dm.testing_environments SET iam_sync_pending=$2 WHERE environment_id=$1",
        )
        .bind(id)
        .bind(final_state != "active")
        .execute(&mut *tx)
        .await?;
        if final_state == "purged" {
            sqlx::query("DELETE FROM dm.testing_mutations WHERE environment_id=$1")
                .bind(id)
                .execute(&mut *tx)
                .await?;
            sqlx::query("DELETE FROM dm.testing_environments WHERE environment_id=$1")
                .bind(id)
                .execute(&mut *tx)
                .await?;
            sqlx::query("UPDATE dm.honeycomb_environments SET testing_key_ciphertext='' WHERE environment_id=$1").bind(id).execute(&mut *tx).await?;
        }
        let receipt = op.receipt("completed");
        sqlx::query("UPDATE dm.honeycomb_operations SET receipt=$3 WHERE environment_id=$1 AND operation_id=$2").bind(id).bind(op.operation_id).bind(&receipt).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(receipt)
    }

    /// Reports observed use to Honeycomb without independently retiring sandboxes.
    /// # Errors
    /// Returns storage or configuration failures; failed deliveries remain retryable.
    pub async fn report_honeycomb_activity(&self) -> AppResult<()> {
        let Some(origin) = &self.honeycomb_base_url else {
            return Ok(());
        };
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .map_err(AppError::internal)?;
        let rows: Vec<(Uuid,String,i64,i64,String,time::OffsetDateTime)> = sqlx::query_as("SELECT environment_id,app_id,generation,key_version,testing_key_ciphertext,last_activity_at FROM dm.honeycomb_environments WHERE state='active' AND last_activity_at IS NOT NULL AND (activity_reported_at IS NULL OR last_activity_at>activity_reported_at)")
            .fetch_all(self.production.pool()).await?;
        for (id, app, generation, key_version, cipher, at) in rows {
            let mut endpoint = origin.join("api/v1/").map_err(AppError::internal)?;
            endpoint
                .path_segments_mut()
                .map_err(|()| AppError::validation("invalid Honeycomb origin"))?
                .pop_if_empty()
                .extend(["environments", &id.to_string(), "apps", &app, "activity"]);
            let key = self.decrypt(id, "honeycomb-key", &cipher)?;
            let response = client
                .post(endpoint)
                .header("X-Testing-Environment-Key", key.expose_secret())
                .header(
                    "Idempotency-Key",
                    format!(
                        "{app}:{id}:{generation}:{key_version}:{}",
                        at.unix_timestamp_nanos()
                    ),
                )
                .json(&json!({"generation":generation,"key_version":key_version}))
                .send()
                .await;
            if response.is_ok_and(|r| r.status().is_success()) {
                sqlx::query("UPDATE dm.honeycomb_environments SET activity_reported_at=GREATEST(activity_reported_at,$2) WHERE environment_id=$1 AND generation=$3 AND key_version=$4 AND state='active'")
                    .bind(id).bind(at).bind(generation).bind(key_version).execute(self.production.pool()).await?;
            }
        }
        Ok(())
    }
}

pub(crate) async fn apply(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((org, id, operation)): Path<(String, Uuid, Uuid)>,
    Json(body): Json<Operation>,
) -> AppResult<Json<Value>> {
    let registry = state.testing.as_ref().ok_or(AppError::Unauthorized)?;
    registry.authenticate_honeycomb(&headers)?;
    if body.org_id != org || body.environment_id != id || body.operation_id != operation {
        return Err(AppError::validation(
            "Honeycomb lifecycle path/body mismatch",
        ));
    }
    Ok(Json(
        registry
            .honeycomb_operation(&body, &state.settings.iam.app_id)
            .await?,
    ))
}
pub(crate) async fn receipt(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((org, id, operation)): Path<(String, Uuid, Uuid)>,
) -> AppResult<Json<Value>> {
    let registry = state.testing.as_ref().ok_or(AppError::Unauthorized)?;
    registry.authenticate_honeycomb(&headers)?;
    let receipt = sqlx::query_scalar("SELECT o.receipt FROM dm.honeycomb_operations o JOIN dm.honeycomb_environments e USING(environment_id) WHERE o.environment_id=$1 AND o.operation_id=$2 AND e.organization_id=$3 AND e.app_id=$4")
        .bind(id).bind(operation).bind(org).bind(&state.settings.iam.app_id).fetch_optional(registry.production.pool()).await?.ok_or(AppError::NotFound)?;
    Ok(Json(receipt))
}
