//! Explicit recipient consent registration through Ting's published OBO contract.
//!
//! Uses the separately approved registration token. Subscription resource
//! authority remains enforced by Ting on each attempt.

use secrecy::{ExposeSecret as _, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use silicon_iam_client::{Client, Credential, EnvironmentKey, models};
use uuid::Uuid;

use crate::{
    AppError, AppResult,
    application::{auth::AuthContext, state::AppState},
    config::TingSettings,
    domain::IdempotencyKey as DmIdempotencyKey,
};

const TING_APP_ID: &str = "ting";
const ENDPOINT_ID: &str = "subscriptions.register";
const ENDPOINT_PATH: &str = "/v1/subscriptions";
const MAX_RESPONSE_BYTES: usize = 64 * 1024;

#[derive(Deserialize, Serialize)]
struct Subscription {
    id: String,
    app_id: String,
    #[serde(rename = "for")]
    recipient: String,
    active: bool,
}

/// Records enrollment intent before any upstream mutation and caches its result.
///
/// # Errors
/// An incomplete prior attempt returns a conflict: registering again could
/// reactivate a grant the recipient revoked after the uncertain acceptance.
pub(crate) async fn register_cached(
    state: &AppState,
    context: &AuthContext,
    key: &DmIdempotencyKey,
) -> AppResult<Value> {
    let authority = state
        .ting_authorization()?
        .authority(
            state.identity.as_ref(),
            &context.organization_id,
            &context.actor,
            ENDPOINT_ID,
        )
        .await?;
    let material = serde_json::to_vec(&json!({
        "app_id": state.settings.iam.app_id,
        "ting_origin": state.settings.ting.base_url,
        "org_id": context.organization_id,
        "recipient": context.actor,
        "testing_environment_id": state.testing_environment,
        "testing_generation": state.testing_generation,
    }))
    .map_err(AppError::internal)?;
    let hash = blake3::hash(&material);
    let reserved = sqlx::query(
        "INSERT INTO ting_enrollment_receipts(organization_id,actor_kind,actor_id,idempotency_key,request_hash) \
         VALUES($1,$2::text::actor_kind,$3,$4,$5) ON CONFLICT DO NOTHING",
    )
    .bind(context.organization_id.as_str())
    .bind(context.actor.actor_type.as_str())
    .bind(context.actor.id.as_str())
    .bind(key.as_str())
    .bind(hash.as_bytes().as_slice())
    .execute(state.store.pool())
    .await?
    .rows_affected() == 1;
    if !reserved {
        let receipt: Option<(Vec<u8>, Option<Value>)> = sqlx::query_as(
            "SELECT request_hash,response FROM ting_enrollment_receipts \
             WHERE organization_id=$1 AND actor_kind=$2::text::actor_kind AND actor_id=$3 AND idempotency_key=$4",
        )
        .bind(context.organization_id.as_str())
        .bind(context.actor.actor_type.as_str())
        .bind(context.actor.id.as_str())
        .bind(key.as_str())
        .fetch_optional(state.store.pool())
        .await?;
        return replay_receipt(receipt, hash.as_bytes());
    }
    let body=serde_json::to_vec(&json!({"org_id":context.organization_id,"app_id":state.settings.iam.app_id,"for":context.actor.id})).map_err(AppError::internal)?;
    let outcome = submit_registration(
        &state.settings.ting,
        authority,
        body,
        &state.settings.iam.app_id,
        context.actor.id.as_str(),
    )
    .await;
    let response = match outcome {
        Ok(response) => response,
        Err(error) => {
            if matches!(
                error,
                AppError::Forbidden
                    | AppError::RateLimited
                    | AppError::Validation(_)
                    | AppError::Conflict(_)
            ) {
                // These rejections establish that no registration was accepted.
                // An uncertain transport/storage result retains the reservation.
                sqlx::query("DELETE FROM ting_enrollment_receipts WHERE organization_id=$1 AND actor_kind=$2::text::actor_kind AND actor_id=$3 AND idempotency_key=$4 AND request_hash=$5 AND response IS NULL")
                    .bind(context.organization_id.as_str()).bind(context.actor.actor_type.as_str())
                    .bind(context.actor.id.as_str()).bind(key.as_str()).bind(hash.as_bytes().as_slice())
                    .execute(state.store.pool()).await?;
            }
            return Err(error);
        }
    };
    let saved = sqlx::query("UPDATE ting_enrollment_receipts SET response=$6,completed_at=clock_timestamp() WHERE organization_id=$1 AND actor_kind=$2::text::actor_kind AND actor_id=$3 AND idempotency_key=$4 AND request_hash=$5 AND response IS NULL")
        .bind(context.organization_id.as_str()).bind(context.actor.actor_type.as_str())
        .bind(context.actor.id.as_str()).bind(key.as_str()).bind(hash.as_bytes().as_slice())
        .bind(&response).execute(state.store.pool()).await?.rows_affected();
    if saved != 1 {
        return Err(AppError::conflict(
            "Ting enrollment context changed before its result was saved",
        ));
    }
    Ok(response)
}

fn replay_receipt(receipt: Option<(Vec<u8>, Option<Value>)>, hash: &[u8]) -> AppResult<Value> {
    let Some((previous_hash, response)) = receipt else {
        return Err(AppError::conflict(
            "Ting enrollment context changed during this request",
        ));
    };
    if previous_hash != hash {
        return Err(AppError::conflict(
            "Ting enrollment idempotency key was used with different input",
        ));
    }
    response.ok_or_else(|| AppError::conflict(
        "Ting enrollment is still running or its result is uncertain; a new explicit registration requires a new idempotency key",
    ))
}

async fn submit_registration(
    settings: &TingSettings,
    authority: super::ting::TingSendAuthority,
    body: Vec<u8>,
    app_id: &str,
    recipient: &str,
) -> AppResult<Value> {
    let http = reqwest::Client::builder()
        .timeout(settings.request_timeout)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| unavailable())?;
    let url = settings
        .base_url
        .join(ENDPOINT_PATH)
        .map_err(|_| unavailable())?;
    let mut request = http
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .bearer_auth(authority.proof_token.expose_secret())
        .body(body);
    if let Some(testing) = authority.testing {
        request = request
            .header("IAM_TEST_APP_SECRET", testing.app_secret.expose_secret())
            .header(
                "X-Testing-Environment-Key",
                testing.environment_key.expose_secret(),
            );
    }
    let mut response = request.send().await.map_err(|_| unavailable())?;
    if !matches!(response.status().as_u16(), 200 | 201) {
        return Err(match response.status().as_u16() {
            403 => AppError::Forbidden,
            409 => AppError::conflict("Ting rejected the registration attempt"),
            429 => AppError::RateLimited,
            // A rejected downstream proof is not evidence that the DM session
            // expired. Never turn a Ting/IAM integration failure into logout.
            _ => unavailable(),
        });
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| unavailable())? {
        if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err(unavailable());
        }
        bytes.extend_from_slice(&chunk);
    }
    subscription_result(&bytes, app_id, recipient)
}

pub(crate) async fn validate_testing_context(
    iam: &Client,
    expected: Option<Uuid>,
    testing: Option<&models::OboTestingContext>,
) -> AppResult<()> {
    match (expected, testing) {
        (None, None) => Ok(()),
        (Some(expected), Some(testing)) if testing.app_id == TING_APP_ID => {
            let environment =
                EnvironmentKey::new(&testing.iam_test_key).map_err(|_| AppError::Forbidden)?;
            let audience = iam
                .with_credential(Credential::Application {
                    app_id: TING_APP_ID.to_owned(),
                    secret: SecretString::from(testing.app_secret.clone()),
                })
                .with_environment(environment);
            let context = audience
                .applications()
                .testing_context()
                .await
                .map_err(map_iam_error)?;
            if context.environment_id != expected || context.application.app_id != TING_APP_ID {
                return Err(AppError::Forbidden);
            }
            Ok(())
        }
        // Neither missing test context nor an unsolicited one may select a
        // different plane or release a test secret to a production request.
        _ => Err(AppError::Forbidden),
    }
}

fn subscription_result(bytes: &[u8], app_id: &str, recipient: &str) -> AppResult<Value> {
    let subscription: Subscription = serde_json::from_slice(bytes).map_err(|_| unavailable())?;
    if subscription.id.is_empty()
        || subscription.id.len() > 255
        || subscription.id.chars().any(char::is_control)
        || subscription.app_id != app_id
        || subscription.recipient != recipient
        || !subscription.active
    {
        return Err(unavailable());
    }
    // Return only the public contract fields, never arbitrary upstream fields.
    serde_json::to_value(subscription).map_err(AppError::internal)
}

fn unavailable() -> AppError {
    AppError::DependencyUnavailable { dependency: "ting" }
}

#[allow(clippy::needless_pass_by_value)]
fn map_iam_error(error: silicon_iam_client::Error) -> AppError {
    match error.api().map(|error| error.status) {
        Some(403 | 404) => AppError::Forbidden,
        Some(429) => AppError::RateLimited,
        _ => AppError::DependencyUnavailable { dependency: "iam" },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_json, header, method, path},
    };
    #[tokio::test]
    async fn enrollment_uses_dedicated_token_and_keeps_subscription_identity_bound()
    -> Result<(), Box<dyn std::error::Error>> {
        let server = MockServer::start().await;
        let settings = TingSettings {
            base_url: server.uri().parse()?,
            request_timeout: std::time::Duration::from_secs(2),
        };
        let body = json!({"org_id":"tos","app_id":"dm","for":"c:alice"});
        Mock::given(method("POST"))
            .and(path("/v1/subscriptions"))
            .and(header("authorization", "Bearer oba_approved_registration"))
            .and(body_json(&body))
            .respond_with(ResponseTemplate::new(201).set_body_json(
                json!({"id":"sub_fixture","app_id":"dm","for":"c:alice","active":true}),
            ))
            .expect(1)
            .mount(&server)
            .await;
        let result = submit_registration(
            &settings,
            super::super::ting::TingSendAuthority {
                proof_token: SecretString::from("oba_approved_registration"),
                testing: None,
            },
            serde_json::to_vec(&body)?,
            "dm",
            "c:alice",
        )
        .await?;
        assert_eq!(result["for"], "c:alice");
        assert!(
            subscription_result(
                br#"{"id":"sub_fixture","app_id":"other","for":"c:alice","active":true}"#,
                "dm",
                "c:alice"
            )
            .is_err()
        );
        Ok(())
    }
}
