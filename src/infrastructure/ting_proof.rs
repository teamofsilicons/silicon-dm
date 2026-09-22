//! Fresh Ting send proofs bound to a logged-in originator and exact outbox bytes.

use secrecy::{ExposeSecret as _, SecretString};
use serde::Deserialize;
use serde_json::json;
use silicon_iam_client::{Client, IdempotencyKey, Mutation, models};
use time::OffsetDateTime;
use uuid::Uuid;

use super::{
    ting::{TingSendAuthority, TingTestingHeaders},
    ting_enrollment::validate_testing_context,
};
use crate::{
    AppError, AppResult,
    application::auth::{AuthContext, PresentedCredential},
};

const AUDIENCE: &str = "tos>ting";
const ENDPOINT: &str = "tings.send";
const PATH: &str = "/v1/tings";
const MAX_REQUEST_BYTES: usize = 256 * 1024;

#[derive(Deserialize)]
struct SendBinding {
    org_id: String,
    #[serde(rename = "type")]
    event_type: String,
}

/// Obtains one single-use proof; the caller owns retries and originator selection.
///
/// This method never refreshes or persists a user credential. The caller supplies
/// a freshly authenticated context and a new UUID for each downstream attempt.
///
/// # Errors
/// Rejects expired/missing authority, changed app/org bindings, invalid audience
/// contracts, and mismatched testing authority without exposing secret responses.
pub(crate) async fn issue(
    iam: &Client,
    app_id: &str,
    environment_id: Option<Uuid>,
    context: &AuthContext,
    request_body: &str,
    attempt_key: &str,
) -> AppResult<TingSendAuthority> {
    if context.credential_expires_at <= OffsetDateTime::now_utc() {
        return Err(AppError::Unauthorized);
    }
    if !context.has_capability("obo:tos>ting:tings.send")
        || !context.has_capability("self.identity.read")
    {
        return Err(AppError::Forbidden);
    }
    if request_body.len() > MAX_REQUEST_BYTES {
        return Err(AppError::validation("Ting request exceeds its body limit"));
    }
    let binding: SendBinding = serde_json::from_str(request_body)
        .map_err(|_| AppError::validation("Ting request binding is invalid"))?;
    if binding.org_id != context.organization_id.as_str()
        || binding.event_type != format!("{app_id}.sync.changed")
    {
        return Err(AppError::Forbidden);
    }
    let attempt = Uuid::parse_str(attempt_key)
        .map_err(|_| AppError::validation("Ting proof attempt key must be a UUID"))?;
    if attempt.is_nil() || attempt_key != attempt.to_string() {
        return Err(AppError::validation(
            "Ting proof attempt key must be a non-nil canonical UUID",
        ));
    }
    let mutation = Mutation::with_key(
        IdempotencyKey::parse(attempt_key)
            .map_err(|_| AppError::validation("invalid Ting proof attempt key"))?,
    );
    let catalog = iam.obo().endpoints(AUDIENCE).await.map_err(map_iam_error)?;
    let endpoint = catalog
        .endpoints
        .iter()
        .find(|endpoint| endpoint.endpoint_id == ENDPOINT)
        .ok_or(AppError::Forbidden)?;
    if endpoint.path != PATH
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
    let proof = iam
        .obo()
        .exchange_signed(
            &models::OboExchangeRequest {
                org_id: Some(context.organization_id.as_str().to_owned()),
                subject_token: subject_token.expose_secret().to_owned(),
                audience: AUDIENCE.to_owned(),
                endpoint_id: ENDPOINT.to_owned(),
                metadata: json!({}),
                request: models::OboExchangeRequestBinding {
                    method: "POST".to_owned(),
                    body_sha256: silicon_iam_client::api::obo::body_sha256(request_body.as_bytes()),
                },
            },
            &catalog,
            &mutation,
        )
        .await
        .map_err(map_iam_error)?;
    if !(1..=60).contains(&proof.expires_in)
        || proof.expires_at <= OffsetDateTime::now_utc()
        || proof.access_proof.is_empty()
    {
        return Err(unavailable());
    }
    validate_testing_context(iam, environment_id, proof.testing_context.as_ref()).await?;
    Ok(TingSendAuthority {
        proof_token: SecretString::from(proof.access_proof),
        testing: proof.testing_context.map(|testing| TingTestingHeaders {
            app_secret: SecretString::from(testing.app_secret),
            environment_key: SecretString::from(testing.iam_test_key),
        }),
    })
}

fn unavailable() -> AppError {
    AppError::DependencyUnavailable { dependency: "iam" }
}

#[allow(clippy::needless_pass_by_value)]
fn map_iam_error(error: silicon_iam_client::Error) -> AppError {
    match error.api().map(|error| error.status) {
        Some(401 | 410) => AppError::Unauthorized,
        Some(403 | 404) => AppError::Forbidden,
        Some(429) => AppError::RateLimited,
        _ => unavailable(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use silicon_iam_client::Credential;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{header, method, path},
    };

    use super::*;
    use crate::domain::{ActorRef, ActorType};

    type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
    const BODY: &str = r#"{ "org_id": "tos", "type":"tos>dm.sync.changed", "for":"bob", "key":"event-key", "data":{} }"#;

    fn context() -> Result<AuthContext> {
        Ok(AuthContext {
            actor: ActorRef {
                actor_type: ActorType::Carbon,
                id: "alice".parse()?,
            },
            organization_id: "tos".parse()?,
            session_id: None,
            org_role: None,
            tag_ids: None,
            represented_actor_ids: BTreeSet::new(),
            capabilities: BTreeSet::from([
                "self.identity.read".to_owned(),
                "obo:tos>ting:tings.send".to_owned(),
            ]),
            credential: PresentedCredential::Bearer(SecretString::from("fixture-originator-oat")),
            credential_expires_at: OffsetDateTime::now_utc() + time::Duration::hours(1),
        })
    }

    async fn issuer(
        testing: Option<models::OboTestingContext>,
    ) -> Result<(MockServer, Client, models::OboEndpointCatalog)> {
        let server = MockServer::start().await;
        let iam = Client::builder(&server.uri())?
            .credential(Credential::Application {
                app_id: "tos>dm".to_owned(),
                secret: SecretString::from("fixture-dm-secret"),
            })
            .build()?;
        let catalog: models::OboEndpointCatalog = serde_json::from_value(json!({
            "application":{"app_id":AUDIENCE,"org_id":"tos"},
            "endpoints":[{"endpoint_id":ENDPOINT,"path":PATH,"critical":true,
                "metadata":{},"ttl_seconds":60}]
        }))?;
        Mock::given(method("GET"))
            .and(path("/api/v1/obo-access/applications/tos%3Eting/endpoints"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&catalog))
            .expect(1)
            .mount(&server)
            .await;
        let proof = models::OboProofResponse {
            access_proof: "fixture-bound-proof".to_owned(),
            proof_id: Uuid::new_v4(),
            expires_in: 60,
            expires_at: OffsetDateTime::now_utc() + time::Duration::seconds(60),
            testing_context: testing,
        };
        Mock::given(method("POST"))
            .and(path("/api/v1/obo-access/exchanges"))
            .and(header(
                "authorization",
                format!("Basic {}", STANDARD.encode("tos>dm:fixture-dm-secret")),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(&proof))
            .expect(1)
            .mount(&server)
            .await;
        Ok((server, iam, catalog))
    }

    #[tokio::test]
    async fn official_sdk_signs_the_original_body_and_originator() -> Result {
        let (server, iam, catalog) = issuer(None).await?;
        let attempt = Uuid::new_v4().to_string();
        let authority = issue(&iam, "tos>dm", None, &context()?, BODY, &attempt).await?;
        assert_eq!(authority.proof_token.expose_secret(), "fixture-bound-proof");
        assert!(authority.testing.is_none());
        let requests = server
            .received_requests()
            .await
            .ok_or("missing IAM calls")?;
        let exchange = requests
            .iter()
            .find(|request| request.url.path().ends_with("/exchanges"))
            .ok_or("missing exchange")?;
        let body: models::OboExchangeRequest = serde_json::from_slice(&exchange.body)?;
        assert_eq!(body.org_id.as_deref(), Some("tos"));
        assert_eq!(body.subject_token, "fixture-originator-oat");
        assert_eq!(body.audience, AUDIENCE);
        assert_eq!(body.endpoint_id, ENDPOINT);
        assert_eq!(body.metadata, json!({}));
        assert_eq!(body.request.method, "POST");
        assert_eq!(
            body.request.body_sha256,
            silicon_iam_client::api::obo::body_sha256(BODY.as_bytes())
        );
        assert_eq!(exchange.headers["idempotency-key"].to_str()?, attempt);
        let timestamp = exchange.headers["x-obo-timestamp"].to_str()?.parse()?;
        assert_eq!(
            exchange.headers["x-obo-signature"].to_str()?,
            iam.obo().sign_exchange(
                &body,
                &catalog,
                timestamp,
                &Mutation::with_key(IdempotencyKey::parse(&attempt)?)
            )?
        );
        assert_eq!(
            requests.len(),
            2,
            "proof issuance must never refresh a session"
        );
        Ok(())
    }

    #[tokio::test]
    async fn missing_authority_and_changed_bindings_never_reach_iam() -> Result {
        let server = MockServer::start().await;
        let iam = Client::new(&server.uri())?;
        let attempt = Uuid::new_v4().to_string();
        for capability in ["self.identity.read", "obo:tos>ting:tings.send"] {
            let mut missing = context()?;
            missing.capabilities.remove(capability);
            assert!(matches!(
                issue(&iam, "tos>dm", None, &missing, BODY, &attempt).await,
                Err(AppError::Forbidden)
            ));
        }
        let mut expired = context()?;
        expired.credential_expires_at = OffsetDateTime::now_utc() - time::Duration::seconds(1);
        assert!(matches!(
            issue(&iam, "tos>dm", None, &expired, BODY, &attempt).await,
            Err(AppError::Unauthorized)
        ));
        for body in [
            BODY.replace("\"tos\"", "\"other\""),
            BODY.replace("tos>dm.sync.changed", "tos>other.sync.changed"),
            BODY.replace("tos>dm.sync.changed", "tos>dm.unexpected.changed"),
        ] {
            assert!(matches!(
                issue(&iam, "tos>dm", None, &context()?, &body, &attempt).await,
                Err(AppError::Forbidden)
            ));
        }
        assert!(
            server
                .received_requests()
                .await
                .ok_or("missing request log")?
                .is_empty()
        );
        Ok(())
    }

    #[tokio::test]
    async fn unsolicited_audience_testing_authority_is_rejected() -> Result {
        let (server, iam, _) = issuer(Some(models::OboTestingContext {
            app_id: AUDIENCE.to_owned(),
            app_secret: "fixture-ting-test-secret".to_owned(),
            iam_test_key: "a".repeat(32),
        }))
        .await?;
        assert!(matches!(
            issue(
                &iam,
                "tos>dm",
                None,
                &context()?,
                BODY,
                &Uuid::new_v4().to_string()
            )
            .await,
            Err(AppError::Forbidden)
        ));
        assert_eq!(
            server
                .received_requests()
                .await
                .ok_or("missing IAM calls")?
                .len(),
            2
        );
        Ok(())
    }

    #[tokio::test]
    async fn audience_testing_authority_must_match_the_verified_environment() -> Result {
        let expected = Uuid::new_v4();
        let (server, iam, _) = issuer(Some(models::OboTestingContext {
            app_id: AUDIENCE.to_owned(),
            app_secret: "fixture-ting-test-secret".to_owned(),
            iam_test_key: "a".repeat(32),
        }))
        .await?;
        Mock::given(method("GET"))
            .and(path("/api/v1/application/testing-context"))
            .and(header(
                "authorization",
                format!(
                    "Basic {}",
                    STANDARD.encode("tos>ting:fixture-ting-test-secret")
                ),
            ))
            .and(header("x-testing-environment-key", "a".repeat(32)))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "environment_id":Uuid::new_v4(),
                "application":{"app_id":AUDIENCE,"base_url":"https://ting.invalid",
                    "app_scope":{"iam":[],"external":[]},"webhook_scope":[],"testing_idle_days":15}
            })))
            .expect(1)
            .mount(&server)
            .await;
        assert!(matches!(
            issue(
                &iam,
                "tos>dm",
                Some(expected),
                &context()?,
                BODY,
                &Uuid::new_v4().to_string()
            )
            .await,
            Err(AppError::Forbidden)
        ));
        Ok(())
    }
}
