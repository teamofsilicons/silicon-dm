//! Explicit recipient consent registration through Ting's published OBO contract.
//!
//! User tokens and single-use proofs exist only for this call. This adapter does
//! not provide authority for a durable background message publisher.

use secrecy::{ExposeSecret as _, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use silicon_iam_client::{Client, Credential, EnvironmentKey, IdempotencyKey, Mutation, models};
use uuid::Uuid;

use crate::{
    AppError, AppResult,
    application::{
        auth::{AuthContext, PresentedCredential},
        state::AppState,
    },
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
    // Each registration execution receives a fresh proof-exchange identity.
    // Retrying a completed DM operation never reaches this point.
    let exchange_attempt = Uuid::new_v4().to_string();
    let outcome = state
        .identity
        .register_ting_delivery(context, &state.settings.ting, &exchange_attempt)
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

/// Registers the authenticated recipient using a fresh, request-bound IAM proof.
///
/// `exchange_attempt_key` identifies one proof issuance attempt, not the public
/// DM operation. A new downstream attempt needs a new proof; callers must not
/// reuse an already-consumed proof by replaying its IAM exchange key.
///
/// # Errors
/// Rejects missing consent, mismatched audience/test authority, malformed
/// upstream responses, and unavailable IAM or Ting without exposing credentials.
pub(crate) async fn register(
    iam: &Client,
    settings: &TingSettings,
    app_id: &str,
    environment_id: Option<Uuid>,
    context: &AuthContext,
    exchange_attempt_key: &str,
) -> AppResult<Value> {
    if !context.has_capability("obo:ting:subscriptions.register")
        || !context.has_capability("self.identity.read")
    {
        return Err(AppError::Forbidden);
    }
    let body = serde_json::to_vec(&json!({
        "org_id": context.organization_id,
        "app_id": app_id,
        "for": context.actor.id,
    }))
    .map_err(AppError::internal)?;
    let catalog = iam
        .obo()
        .endpoints(TING_APP_ID)
        .await
        .map_err(map_iam_error)?;
    let endpoint = catalog
        .endpoints
        .iter()
        .find(|endpoint| endpoint.endpoint_id == ENDPOINT_ID)
        .ok_or(AppError::Forbidden)?;
    if endpoint.path != ENDPOINT_PATH
        || !endpoint
            .metadata
            .as_object()
            .is_some_and(serde_json::Map::is_empty)
        || !endpoint
            .ttl_seconds
            .is_some_and(|ttl| (1..=60).contains(&ttl))
    {
        return Err(unavailable());
    }
    let PresentedCredential::Bearer(subject_token) = &context.credential;
    let mutation = Mutation::with_key(
        IdempotencyKey::parse(exchange_attempt_key)
            .map_err(|_| AppError::validation("invalid Ting enrollment attempt key"))?,
    );
    let proof = iam
        .obo()
        .exchange_signed(
            &models::OboExchangeRequest {
                org_id: Some(context.organization_id.as_str().to_owned()),
                subject_token: subject_token.expose_secret().to_owned(),
                audience: TING_APP_ID.to_owned(),
                endpoint_id: ENDPOINT_ID.to_owned(),
                metadata: json!({}),
                request: models::OboExchangeRequestBinding {
                    method: "POST".to_owned(),
                    body_sha256: silicon_iam_client::api::obo::body_sha256(&body),
                },
            },
            &catalog,
            &mutation,
        )
        .await
        .map_err(map_iam_error)?;
    if !(1..=60).contains(&proof.expires_in) || proof.expires_at <= time::OffsetDateTime::now_utc()
    {
        return Err(unavailable());
    }
    validate_testing_context(iam, environment_id, proof.testing_context.as_ref()).await?;
    submit_registration(settings, proof, body, app_id, context.actor.id.as_str()).await
}

async fn submit_registration(
    settings: &TingSettings,
    proof: models::OboProofResponse,
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
        .bearer_auth(&proof.access_proof)
        .body(body);
    if let Some(testing) = proof.testing_context {
        request = request
            .header("IAM_TEST_APP_SECRET", testing.app_secret)
            .header("X-Testing-Environment-Key", testing.iam_test_key);
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

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "one local contract fixture follows catalog, signed exchange, and Ting submission"
    )]
    async fn real_iam_sdk_binds_each_enrollment_attempt_to_exact_ting_bytes()
    -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        use base64::{Engine as _, engine::general_purpose::STANDARD};
        use std::collections::BTreeSet;
        use wiremock::{
            Mock, MockServer, ResponseTemplate,
            matchers::{header, method, path},
        };

        let issuer = MockServer::start().await;
        let ting = MockServer::start().await;
        let iam = Client::builder(&issuer.uri())?
            .credential(Credential::Application {
                app_id: "dm".to_owned(),
                secret: SecretString::from("fixture-app-secret"),
            })
            .build()?;
        let settings = TingSettings {
            base_url: ting.uri().parse()?,
            request_timeout: std::time::Duration::from_secs(5),
        };
        let context = AuthContext {
            actor: crate::domain::ActorRef {
                actor_type: crate::domain::ActorType::Carbon,
                id: "c:alice".parse()?,
            },
            organization_id: "tos".parse()?,
            session_id: None,
            org_role: None,
            tag_ids: None,
            represented_actor_ids: BTreeSet::new(),
            capabilities: BTreeSet::from([
                "self.identity.read".to_owned(),
                "obo:ting:subscriptions.register".to_owned(),
            ]),
            credential: PresentedCredential::Bearer(SecretString::from("fixture-recipient-oat")),
            credential_expires_at: time::OffsetDateTime::now_utc() + time::Duration::hours(1),
        };
        let catalog: models::OboEndpointCatalog = serde_json::from_value(json!({
            "application":{"app_id":"ting","org_id":"tos"},
            "endpoints":[{"endpoint_id":ENDPOINT_ID,"path":ENDPOINT_PATH,
                "metadata":{},"critical":true,"ttl_seconds":60}]
        }))?;
        let application_auth = format!("Basic {}", STANDARD.encode("dm:fixture-app-secret"));
        Mock::given(method("GET"))
            .and(path("/api/v1/obo-access/applications/ting/endpoints"))
            .and(header("authorization", application_auth.clone()))
            .respond_with(ResponseTemplate::new(200).set_body_json(&catalog))
            .expect(2)
            .mount(&issuer)
            .await;
        let public_response = json!({"id":"sub_alice","app_id":"dm","for":"c:alice","active":true});
        let attempts = [Uuid::new_v4().to_string(), Uuid::new_v4().to_string()];
        for attempt in &attempts {
            let proof = models::OboProofResponse {
                access_proof: format!("fixture-proof-{attempt}"),
                proof_id: Uuid::new_v4(),
                expires_in: 60,
                expires_at: time::OffsetDateTime::now_utc() + time::Duration::seconds(60),
                testing_context: None,
            };
            Mock::given(method("POST"))
                .and(path("/api/v1/obo-access/exchanges"))
                .and(header("authorization", application_auth.clone()))
                .and(header("idempotency-key", attempt))
                .respond_with(ResponseTemplate::new(200).set_body_json(&proof))
                .expect(1)
                .mount(&issuer)
                .await;
            let mut upstream_response = public_response.clone();
            upstream_response["access_proof"] = json!(proof.access_proof);
            Mock::given(method("POST"))
                .and(path(ENDPOINT_PATH))
                .and(header(
                    "authorization",
                    format!("Bearer {}", proof.access_proof),
                ))
                .respond_with(ResponseTemplate::new(201).set_body_json(upstream_response))
                .expect(1)
                .mount(&ting)
                .await;
            assert_eq!(
                register(&iam, &settings, "dm", None, &context, attempt).await?,
                public_response
            );
        }
        let exchanges = issuer
            .received_requests()
            .await
            .ok_or("missing IAM calls")?;
        let submissions = ting.received_requests().await.ok_or("missing Ting calls")?;
        assert_eq!(submissions.len(), 2);
        let exchanges: Vec<_> = exchanges
            .iter()
            .filter(|request| request.url.path().ends_with("/exchanges"))
            .collect();
        assert_eq!(exchanges.len(), 2);
        for (index, (exchange, submission)) in exchanges.iter().zip(&submissions).enumerate() {
            let body: models::OboExchangeRequest = serde_json::from_slice(&exchange.body)?;
            assert_eq!(body.org_id.as_deref(), Some("tos"));
            assert_eq!(body.subject_token, "fixture-recipient-oat");
            assert_eq!(body.audience, TING_APP_ID);
            assert_eq!(body.endpoint_id, ENDPOINT_ID);
            assert_eq!(body.metadata, json!({}));
            assert_eq!(body.request.method, "POST");
            assert_eq!(
                body.request.body_sha256,
                silicon_iam_client::api::obo::body_sha256(&submission.body)
            );
            assert_eq!(
                serde_json::from_slice::<Value>(&submission.body)?,
                json!({"org_id":"tos","app_id":"dm","for":"c:alice"})
            );
            let timestamp = exchange.headers["x-obo-timestamp"].to_str()?.parse()?;
            let mutation = Mutation::with_key(IdempotencyKey::parse(&attempts[index])?);
            assert_eq!(
                exchange.headers["x-obo-signature"].to_str()?,
                iam.obo()
                    .sign_exchange(&body, &catalog, timestamp, &mutation)?
            );
            assert!(submission.headers.get("iam_test_app_secret").is_none());
            assert!(
                submission
                    .headers
                    .get("x-testing-environment-key")
                    .is_none()
            );
        }
        Ok(())
    }

    #[test]
    fn registration_replays_only_confirmed_matching_receipts() -> AppResult<()> {
        let hash = [7_u8; 32];
        let response = json!({"id":"sub_123","app_id":"dm","for":"c:alice","active":true});
        assert_eq!(
            replay_receipt(Some((hash.to_vec(), Some(response.clone()))), &hash)?,
            response
        );
        assert!(replay_receipt(Some((hash.to_vec(), None)), &hash).is_err());
        assert!(replay_receipt(Some((vec![8; 32], Some(response))), &hash).is_err());
        assert!(replay_receipt(None, &hash).is_err());
        Ok(())
    }

    #[test]
    fn registration_response_cannot_switch_recipient_or_return_upstream_secrets() -> AppResult<()> {
        let good = json!({"id":"sub_123","app_id":"dm","for":"c:alice","active":true,
            "access_proof":"must-not-reach-the-client"});
        let output = subscription_result(
            &serde_json::to_vec(&good).map_err(AppError::internal)?,
            "dm",
            "c:alice",
        )?;
        assert!(output.get("access_proof").is_none());
        for replacement in [
            json!({"id":"sub_123","app_id":"other","for":"c:alice","active":true}),
            json!({"id":"sub_123","app_id":"dm","for":"c:bob","active":true}),
            json!({"id":"sub_123","app_id":"dm","for":"c:alice","active":false}),
        ] {
            assert!(
                subscription_result(
                    &serde_json::to_vec(&replacement).map_err(AppError::internal)?,
                    "dm",
                    "c:alice"
                )
                .is_err()
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn production_and_testing_proofs_cannot_cross_planes() -> AppResult<()> {
        let iam = Client::new("http://127.0.0.1:1")
            .map_err(|_| AppError::internal(anyhow::anyhow!("fixture IAM client invalid")))?;
        let testing = models::OboTestingContext {
            app_id: TING_APP_ID.to_owned(),
            app_secret: "fixture-secret".to_owned(),
            iam_test_key: "a".repeat(32),
        };
        assert!(
            validate_testing_context(&iam, None, Some(&testing))
                .await
                .is_err()
        );
        assert!(
            validate_testing_context(&iam, Some(Uuid::new_v4()), None)
                .await
                .is_err()
        );
        assert!(validate_testing_context(&iam, None, None).await.is_ok());
        Ok(())
    }

    #[tokio::test]
    async fn transport_keeps_the_proof_bound_body_and_never_follows_redirects() -> AppResult<()> {
        use wiremock::{
            Mock, MockServer, ResponseTemplate,
            matchers::{body_bytes, header, method, path},
        };

        let server = MockServer::start().await;
        let settings = TingSettings {
            base_url: server.uri().parse().map_err(AppError::internal)?,
            request_timeout: std::time::Duration::from_secs(5),
        };
        // Deliberately noncanonical whitespace must survive the transport intact.
        let body = br#"{ "org_id": "tos", "for": "c:alice", "app_id": "dm" }"#.to_vec();
        let proof = || models::OboProofResponse {
            testing_context: None,
            access_proof: "fixture-proof".to_owned(),
            proof_id: Uuid::new_v4(),
            expires_in: 60,
            expires_at: time::OffsetDateTime::now_utc() + time::Duration::seconds(60),
        };
        Mock::given(method("POST"))
            .and(path(ENDPOINT_PATH))
            .and(header("authorization", "Bearer fixture-proof"))
            .and(body_bytes(body.clone()))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "id":"sub_123","app_id":"dm","for":"c:alice","active":true
            })))
            .expect(1)
            .mount(&server)
            .await;
        assert_eq!(
            submit_registration(&settings, proof(), body.clone(), "dm", "c:alice").await?["id"],
            "sub_123"
        );
        server.verify().await;
        server.reset().await;
        Mock::given(path(ENDPOINT_PATH))
            .respond_with(ResponseTemplate::new(307).insert_header(
                "location",
                format!("{}/must-not-receive-proof", server.uri()),
            ))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(path("/must-not-receive-proof"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        assert!(
            submit_registration(&settings, proof(), body, "dm", "c:alice")
                .await
                .is_err()
        );
        Ok(())
    }
}
