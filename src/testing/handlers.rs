//! Production control-plane endpoints. The clean endpoint also accepts a root key.

use super::{TestingEnvironment, TestingRegistry};
use crate::{
    AppError, AppResult,
    api::extract::{ApiJson, ApiPath, ApiQuery, Authenticated, Idempotency},
    application::state::AppState,
};
use axum::{
    Json,
    extract::{FromRequestParts as _, Request, State},
    http::StatusCode,
};
use secrecy::{ExposeSecret as _, SecretString};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

/// IAM pairing details and user-supplied metadata.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateEnvironment {
    /// Human-readable environment name.
    pub name: String,
    /// Optional environment description.
    pub description: Option<String>,
    /// IAM testing environment UUID.
    pub iam_environment_id: Uuid,
    /// IAM root key; never persisted in plaintext.
    pub iam_environment_key: SecretString,
    /// Canonical imported DM IAM app ID.
    pub iam_app_id: String,
    /// Test-only secret returned by IAM app import.
    pub iam_app_secret: SecretString,
    /// Optional dedicated IAM test webhook signing secret; paired with its version.
    pub iam_webhook_secret: Option<SecretString>,
    /// Key version registered for the dedicated test webhook signer.
    pub iam_webhook_key_version: Option<i64>,
}
/// Mutable environment metadata.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateEnvironment {
    /// Replacement name, if supplied.
    pub name: Option<String>,
    /// Replacement description; an empty string clears the displayed description.
    pub description: Option<String>,
}
/// Environment listing options.
#[derive(Default, Deserialize)]
pub struct ListQuery {
    /// Include recoverable deleted environments.
    #[serde(default)]
    pub include_deleted: bool,
}
#[derive(Serialize)]
struct EnvironmentWithKey {
    #[serde(flatten)]
    environment: TestingEnvironment,
    root_key: String,
}
#[derive(Serialize)]
struct KeyResponse {
    environment_id: Uuid,
    root_key: String,
}

fn registry(state: &AppState) -> AppResult<Arc<TestingRegistry>> {
    state.testing.clone().ok_or_else(||AppError::conflict("testing environments are not configured; set DM_TEST_DATABASE_URL and DM_TEST_KEY_ENCRYPTION_KEY"))
}

/// Lists environments owned by the authenticated production organization.
///
/// # Errors
/// Returns a database error if environment metadata cannot be read.
pub async fn list(
    State(state): State<AppState>,
    Authenticated(auth): Authenticated,
    ApiQuery(query): ApiQuery<ListQuery>,
) -> AppResult<Json<serde_json::Value>> {
    Ok(Json(
        serde_json::json!({"items":registry(&state)?.list(auth.organization_id.as_str(),query.include_deleted).await?}),
    ))
}
/// Creates an empty, validated DM/IAM test environment pair.
///
/// # Errors
/// Returns validation or IAM errors for invalid pairing, conflicts for reused request keys, or storage errors.
pub async fn create(
    State(state): State<AppState>,
    Authenticated(auth): Authenticated,
    Idempotency(key): Idempotency,
    ApiJson(input): ApiJson<CreateEnvironment>,
) -> AppResult<(StatusCode, Json<serde_json::Value>)> {
    let registry = registry(&state)?;
    let mut body = serde_json::json!({"name":input.name,"description":input.description,"iam_environment_id":input.iam_environment_id,"iam_app_id":input.iam_app_id,"iam_environment_key":input.iam_environment_key.expose_secret(),"iam_app_secret":input.iam_app_secret.expose_secret()});
    if let Some(secret) = &input.iam_webhook_secret {
        body["iam_webhook_secret"] = serde_json::json!(secret.expose_secret());
    }
    if let Some(version) = input.iam_webhook_key_version {
        body["iam_webhook_key_version"] = serde_json::json!(version);
    }

    let mutation = registry
        .begin_mutation(&auth, "create", key.as_str(), &body, None)
        .await?;
    if let Some(response) = &mutation.replay {
        return Ok((StatusCode::CREATED, Json(response.clone())));
    }
    Ok((
        StatusCode::CREATED,
        Json(registry.create(&state, &auth, input, &mutation).await?),
    ))
}
/// Retrieves one environment's non-secret metadata.
///
/// # Errors
/// Returns not found for another organization or unknown environment, or a database error.
pub async fn get(
    State(state): State<AppState>,
    Authenticated(auth): Authenticated,
    ApiPath(id): ApiPath<Uuid>,
) -> AppResult<Json<TestingEnvironment>> {
    Ok(Json(
        registry(&state)?
            .get(id, auth.organization_id.as_str())
            .await?,
    ))
}
/// Changes creator/admin-managed metadata.
///
/// # Errors
/// Returns permission, validation, lifecycle-conflict, idempotency, or storage errors.
pub async fn update(
    State(state): State<AppState>,
    Authenticated(auth): Authenticated,
    ApiPath(id): ApiPath<Uuid>,
    Idempotency(key): Idempotency,
    ApiJson(input): ApiJson<UpdateEnvironment>,
) -> AppResult<Json<serde_json::Value>> {
    let registry = registry(&state)?;
    let mutation=registry.begin_mutation(&auth,"update",key.as_str(),&serde_json::json!({"environment_id":id,"name":input.name,"description":input.description}),Some(id)).await?;
    if let Some(response) = &mutation.replay {
        return Ok(Json(response.clone()));
    }
    Ok(Json(
        registry
            .update(id, &auth, input.name, input.description, &mutation)
            .await?,
    ))
}
/// Retrieves the active root key for a creator or organization administrator.
///
/// # Errors
/// Returns permission errors, a conflict for deleted environments, or secret/storage errors.
pub async fn key(
    State(state): State<AppState>,
    Authenticated(auth): Authenticated,
    ApiPath(id): ApiPath<Uuid>,
) -> AppResult<Json<serde_json::Value>> {
    let key = registry(&state)?.key(id, &auth).await?;
    Ok(Json(
        serde_json::to_value(KeyResponse {
            environment_id: id,
            root_key: key.expose_secret().to_owned(),
        })
        .map_err(AppError::internal)?,
    ))
}
/// Invalidates the old root key and issues a fresh key.
///
/// # Errors
/// Returns permission, lifecycle-conflict, idempotency, or secret/storage errors.
pub async fn rotate(
    State(state): State<AppState>,
    Authenticated(auth): Authenticated,
    ApiPath(id): ApiPath<Uuid>,
    Idempotency(key): Idempotency,
) -> AppResult<Json<serde_json::Value>> {
    let registry = registry(&state)?;
    let mutation = registry
        .begin_mutation(
            &auth,
            "rotate",
            key.as_str(),
            &serde_json::json!({"environment_id":id}),
            Some(id),
        )
        .await?;
    if let Some(response) = &mutation.replay {
        return Ok(Json(response.clone()));
    }
    Ok(Json(registry.rotate(id, &auth, false, &mutation).await?))
}
/// Restores retained data with a fresh key within the 30-day window.
///
/// # Errors
/// Returns permission errors, an expired recovery-window conflict, or idempotency/storage errors.
pub async fn restore(
    State(state): State<AppState>,
    Authenticated(auth): Authenticated,
    ApiPath(id): ApiPath<Uuid>,
    Idempotency(key): Idempotency,
) -> AppResult<Json<serde_json::Value>> {
    let registry = registry(&state)?;
    let mutation = registry
        .begin_mutation(
            &auth,
            "restore",
            key.as_str(),
            &serde_json::json!({"environment_id":id}),
            Some(id),
        )
        .await?;
    if let Some(response) = &mutation.replay {
        return Ok(Json(response.clone()));
    }
    Ok(Json(registry.rotate(id, &auth, true, &mutation).await?))
}
/// Soft-deletes an environment and revokes its root key immediately.
///
/// # Errors
/// Returns permission, idempotency, or storage errors.
pub async fn delete(
    State(state): State<AppState>,
    Authenticated(auth): Authenticated,
    ApiPath(id): ApiPath<Uuid>,
    Idempotency(key): Idempotency,
) -> AppResult<StatusCode> {
    let registry = registry(&state)?;
    let mutation = registry
        .begin_mutation(
            &auth,
            "delete",
            key.as_str(),
            &serde_json::json!({"environment_id":id}),
            Some(id),
        )
        .await?;
    if mutation.replay.is_none() {
        registry.delete(id, &auth, &mutation).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}
/// Cleans every data row while retaining the environment and current root key.
///
/// # Errors
/// Returns authentication errors for mismatched keys, lifecycle/idempotency conflicts, or storage errors.
pub async fn clean(
    State(state): State<AppState>,
    ApiPath(id): ApiPath<Uuid>,
    Idempotency(idempotency): Idempotency,
    request: Request,
) -> AppResult<StatusCode> {
    let (mut parts, _body) = request.into_parts();
    let keys = parts.headers.get_all("x-testing-environment-key");
    let mut values = keys.iter();
    let key = values
        .next()
        .map(|value| value.to_str().map(str::to_owned))
        .transpose()
        .map_err(|_| AppError::Unauthorized)?;
    if values.next().is_some() {
        return Err(AppError::validation(
            "provide exactly one testing environment key",
        ));
    }
    let auth = if key.is_none() {
        Some(
            Authenticated::from_request_parts(&mut parts, &state)
                .await?
                .0,
        )
    } else {
        None
    };
    let registry = registry(&state)?;
    let input = serde_json::json!({"environment_id":id});
    let mutation = if let Some(key) = &key {
        let selected = registry.state_for_key(&state, key).await?;
        if selected.testing_environment != Some(id) {
            return Err(AppError::Unauthorized);
        }
        let key_id = blake3::hash(key.as_bytes()).to_hex().to_string();
        registry
            .begin_scoped_mutation(
                ("testing-root", "root", &key_id),
                "clean",
                idempotency.as_str(),
                &input,
                Some(id),
            )
            .await?
    } else {
        registry
            .begin_mutation(
                auth.as_ref().ok_or(AppError::Unauthorized)?,
                "clean",
                idempotency.as_str(),
                &input,
                Some(id),
            )
            .await?
    };
    if mutation.replay.is_none() {
        registry
            .clean(id, key.as_deref(), auth.as_ref(), &mutation)
            .await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

pub(super) fn environment_with_key(
    environment: TestingEnvironment,
    key: &SecretString,
) -> AppResult<serde_json::Value> {
    serde_json::to_value(EnvironmentWithKey {
        environment,
        root_key: key.expose_secret().to_owned(),
    })
    .map_err(AppError::internal)
}
