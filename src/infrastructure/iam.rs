//! Silicon IAM integration through the official, version-negotiating SDK.

use std::{collections::BTreeSet, sync::Arc};

use async_trait::async_trait;
use secrecy::{ExposeSecret as _, SecretString};
use silicon_iam_client::{
    Client, Credential, EnvironmentKey, IdempotencyKey, Mutation, WebhookSecret,
    WebhookSecretKeyring, WebhookVerifier, models,
};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{
    AppError, AppResult,
    application::{
        auth::{ApplicationSession, AuthContext, PresentedCredential},
        ports::{AuthenticationRequest, IdentityProvider},
    },
    config::IamSettings,
    domain::{ActorId, ActorRef, ActorType, OrganizationId},
    infrastructure::postgres::PostgresStore,
};

/// Official SDK client bound permanently to one application and IAM plane.
#[derive(Clone)]
pub struct IamClient {
    client: Client,
    app_id: String,
    environment_id: Option<Uuid>,
    verifier: Arc<WebhookVerifier>,
    directory: Option<PostgresStore>,
}

impl IamClient {
    /// Builds a production IAM adapter from validated backend settings.
    ///
    /// # Errors
    /// Rejects invalid IAM URLs, credentials, or webhook signer configuration.
    pub fn new(settings: &IamSettings) -> AppResult<Self> {
        Self::build(
            settings,
            &settings.app_id,
            settings.app_secret.clone(),
            None,
        )
    }

    /// Builds an isolated adapter using exclusively the imported test application.
    /// Every SDK request, including version negotiation, carries the IAM test key.
    ///
    /// # Errors
    /// Rejects invalid environment, application, or signer configuration.
    #[allow(clippy::needless_pass_by_value)] // Credentials are deliberately owned at the configuration boundary.
    pub fn for_environment(
        settings: &IamSettings,
        app_id: &str,
        app_secret: SecretString,
        iam_key: SecretString,
        iam_environment_id: Uuid,
    ) -> AppResult<Self> {
        if iam_environment_id.is_nil() || app_id != settings.app_id {
            return Err(AppError::validation(
                "import the canonical DM application into the selected IAM testing environment",
            ));
        }
        let key = EnvironmentKey::new(iam_key.expose_secret().to_owned()).map_err(|_| {
            AppError::validation("IAM testing key must be exactly 32 alphanumeric characters")
        })?;
        Self::build(
            settings,
            app_id,
            app_secret,
            Some((key, iam_environment_id)),
        )
    }

    fn build(
        settings: &IamSettings,
        app_id: &str,
        secret: SecretString,
        environment: Option<(EnvironmentKey, Uuid)>,
    ) -> AppResult<Self> {
        let mut builder = Client::builder(settings.base_url.as_str())
            .map_err(|_| dependency_unavailable())?
            .credential(Credential::Application {
                app_id: app_id.to_owned(),
                secret,
            })
            .timeout(settings.request_timeout)
            .user_agent(format!("silicon-dm/{}", env!("CARGO_PKG_VERSION")));
        let environment_id = environment.as_ref().map(|(_, id)| *id);
        if let Some((key, _)) = environment {
            builder = builder.environment(key);
        }
        let keyring = WebhookSecretKeyring::new(
            settings.webhook_key_version,
            WebhookSecret::new(settings.webhook_secret.expose_secret().to_owned())
                .map_err(|_| AppError::validation("IAM webhook secret is invalid"))?,
        )
        .map_err(|_| AppError::validation("IAM webhook key version must be positive"))?;
        Ok(Self {
            client: builder.build().map_err(|_| dependency_unavailable())?,
            app_id: app_id.to_owned(),
            environment_id,
            verifier: Arc::new(WebhookVerifier::new(keyring)),
            directory: None,
        })
    }

    /// Attaches the database permanently scoped to this DM environment.
    #[must_use]
    pub fn with_directory(mut self, store: PostgresStore) -> Self {
        self.directory = Some(store);
        self
    }

    /// Proves that the key names the requested live environment and its app secret works.
    /// No fallback to the production credential or production IAM is possible.
    ///
    /// # Errors
    /// Rejects a mismatched environment or unavailable IAM application credential.
    pub async fn validate_environment(&self) -> AppResult<()> {
        let expected = self
            .environment_id
            .ok_or_else(|| AppError::validation("IAM testing environment required"))?;
        let environment = self
            .client
            .environments()
            .current()
            .await
            .map_err(map_error)?;
        if environment.id != expected {
            return Err(AppError::Forbidden);
        }
        let application = self
            .client
            .applications()
            .discover_base_url(&self.app_id)
            .await
            .map_err(map_error)?;
        if application.app_id != self.app_id {
            return Err(AppError::Forbidden);
        }
        Ok(())
    }

    async fn bearer_context(
        &self,
        token: &SecretString,
        organization_id: &OrganizationId,
    ) -> AppResult<AuthContext> {
        validate_token(token.expose_secret(), "oat_")?;
        let inspected = self
            .client
            .oauth()
            .introspect(
                &models::TokenIntrospectionRequest {
                    token: token.expose_secret().to_owned(),
                    token_type_hint: Some(
                        models::TokenIntrospectionRequestTokenTypeHint::AccessToken,
                    ),
                },
                Some(organization_id.as_str()),
            )
            .await
            .map_err(map_error)?;
        if !inspected.active {
            return Err(AppError::Unauthorized);
        }
        let snapshot = inspected.authorization.ok_or_else(dependency_unavailable)?;
        let actor_type = match snapshot.actor_type {
            models::ApplicationAuthorizationActorType::Carbon => ActorType::Carbon,
            models::ApplicationAuthorizationActorType::Silicon => ActorType::Silicon,
            models::ApplicationAuthorizationActorType::Other(_) => {
                return Err(AppError::Unauthorized);
            }
        };
        let expected_type = match actor_type {
            ActorType::Carbon => models::TokenIntrospectionActorType::Carbon,
            ActorType::Silicon => models::TokenIntrospectionActorType::Silicon,
        };
        let snapshot_scopes: BTreeSet<String> = snapshot.scopes.iter().cloned().collect();
        let introspected_scopes: BTreeSet<String> = inspected
            .scope
            .as_deref()
            .unwrap_or_default()
            .split_ascii_whitespace()
            .map(str::to_owned)
            .collect();
        if inspected.audience.as_deref() != Some(self.app_id.as_str())
            || inspected.client_id.as_deref() != Some(self.app_id.as_str())
            || inspected.org_id.as_deref() != Some(organization_id.as_str())
            || inspected.principal_id != Some(snapshot.principal_id)
            || inspected.actor_type != Some(expected_type)
            || inspected.membership_id != Some(snapshot.membership_id)
            || inspected.authorization_epoch != Some(snapshot.authorization_epoch)
            || inspected
                .expires_at
                .is_none_or(|expires| expires <= OffsetDateTime::now_utc().unix_timestamp())
            || snapshot.org_id != organization_id.as_str()
            || snapshot.audience != self.app_id
            || snapshot.testing_environment_id != self.environment_id
            || snapshot.principal_id.is_nil()
            || snapshot.membership_id.is_nil()
            || snapshot.organization_id.is_nil()
            || snapshot.membership_version < 1
            || snapshot.authorization_epoch < 0
            || snapshot_scopes != introspected_scopes
        {
            return Err(AppError::Unauthorized);
        }
        let actor = actor_ref(actor_type, &snapshot.public_id)?;
        self.directory
            .as_ref()
            .ok_or_else(dependency_unavailable)?
            .project_iam_authorization(&snapshot, &actor)
            .await?;
        Ok(AuthContext {
            actor,
            principal_id: snapshot.principal_id,
            session_id: inspected.session_id,
            organization_id: organization_id.clone(),
            org_role: snapshot.org_role,
            represented_actor_ids: BTreeSet::new(),
            capabilities: snapshot_scopes,
            credential: PresentedCredential::Bearer(token.clone()),
        })
    }

    async fn validated_session(
        &self,
        response: models::OAuthTokenResponse,
    ) -> AppResult<ApplicationSession> {
        validate_token(&response.access_token, "oat_")?;
        validate_token(&response.refresh_token, "ort_")?;
        if response.token_type.as_str() != Some("Bearer") || response.expires_in <= 0 {
            return Err(dependency_unavailable());
        }
        // IAM owns organization consent. An unscoped token carries the selected
        // memberships in introspection, rather than a singular token org_id.
        let grants = self
            .client
            .oauth()
            .authorizations(&response.access_token)
            .await
            .map_err(map_error)?
            .ok_or(AppError::Unauthorized)?;
        let mut organization_ids = Vec::new();
        for grant in grants {
            if grant.audience != self.app_id
                || grant.principal_id != response.actor.principal_id
                || grant.testing_environment_id != self.environment_id
                || iam_actor(&response.actor)?
                    != actor_ref(
                        match grant.actor_type {
                            models::ApplicationAuthorizationActorType::Carbon => ActorType::Carbon,
                            models::ApplicationAuthorizationActorType::Silicon => {
                                ActorType::Silicon
                            }
                            models::ApplicationAuthorizationActorType::Other(_) => {
                                return Err(AppError::Unauthorized);
                            }
                        },
                        &grant.public_id,
                    )?
            {
                return Err(AppError::Unauthorized);
            }
            let org: OrganizationId = grant.org_id.parse().map_err(|_| dependency_unavailable())?;
            if !organization_ids.contains(&org) {
                organization_ids.push(org);
            }
        }
        organization_ids.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        let organization_id = organization_ids.first().cloned().ok_or_else(|| {
            AppError::validation("Select at least one active organization in IAM and sign in again")
        })?;
        let context = self
            .bearer_context(
                &SecretString::from(response.access_token.clone()),
                &organization_id,
            )
            .await?;
        let response_actor = iam_actor(&response.actor)?;
        if response_actor != context.actor || response.actor.principal_id != context.principal_id {
            return Err(AppError::Unauthorized);
        }
        Ok(ApplicationSession {
            access_token: response.access_token,
            refresh_token: response.refresh_token,
            token_type: "Bearer",
            expires_in: response.expires_in,
            scope: context
                .capabilities
                .into_iter()
                .collect::<Vec<_>>()
                .join(" "),
            actor: context.actor,
            organization_id,
            organization_ids,
        })
    }

    async fn visible_participants(
        &self,
        context: &AuthContext,
        requested: &[ActorId],
    ) -> AppResult<Vec<ActorRef>> {
        let requested: BTreeSet<ActorId> = requested.iter().cloned().collect();
        if requested.iter().all(|id| *id == context.actor.id) {
            return Ok(if requested.is_empty() {
                Vec::new()
            } else {
                vec![context.actor.clone()]
            });
        }
        self.directory
            .as_ref()
            .ok_or_else(dependency_unavailable)?
            .resolve_iam_participants(&context.organization_id, &requested)
            .await
    }
}

#[async_trait]
impl IdentityProvider for IamClient {
    async fn authenticate(&self, request: AuthenticationRequest<'_>) -> AppResult<AuthContext> {
        let AuthenticationRequest::Bearer {
            token,
            organization_id,
        } = request;
        self.bearer_context(token, organization_id).await
    }

    async fn login(&self, slt: &SecretString, key: &str) -> AppResult<ApplicationSession> {
        // SLT is the field/credential purpose, not its wire prefix. IAM currently
        // issues oac_ authorization-code tokens; the SDK delegates that opaque
        // contract to IAM instead of imposing a guessed local prefix.
        validate_opaque_token(slt.expose_secret())?;
        let response = self
            .client
            .oauth()
            .login(&self.app_id, slt.expose_secret(), &mutation("login", key)?)
            .await
            .map_err(map_error)?;
        self.validated_session(response).await
    }

    async fn refresh(&self, token: &SecretString, key: &str) -> AppResult<ApplicationSession> {
        validate_token(token.expose_secret(), "ort_")?;
        let response = self
            .client
            .oauth()
            .refresh(
                &self.app_id,
                token.expose_secret(),
                &mutation("refresh", key)?,
            )
            .await
            .map_err(map_error)?;
        self.validated_session(response).await
    }

    async fn logout(&self, token: &SecretString, key: &str) -> AppResult<()> {
        let hint = if token.expose_secret().starts_with("ort_") {
            validate_token(token.expose_secret(), "ort_")?;
            models::OAuthRevocationRequestTokenTypeHint::RefreshToken
        } else {
            validate_token(token.expose_secret(), "oat_")?;
            models::OAuthRevocationRequestTokenTypeHint::AccessToken
        };
        self.client
            .oauth()
            .revoke(
                &models::OAuthRevocationRequest {
                    token: token.expose_secret().to_owned(),
                    token_type_hint: Some(hint),
                },
                &mutation("logout", key)?,
            )
            .await
            .map_err(map_error)
    }

    fn verify_webhook(
        &self,
        headers: &http::HeaderMap,
        body: &[u8],
    ) -> AppResult<models::WebhookEvent> {
        let delivery = self
            .verifier
            .verify(headers, body)
            .map_err(|_| AppError::Unauthorized)?;
        match self.client.environment() {
            Some(key) => delivery
                .verify_testing_environment(key)
                .map_err(|_| AppError::Unauthorized)?,
            // This distinct internal result means the signature was authentic,
            // but routing must select the matching testing plane.
            None if delivery.is_testing() => return Err(AppError::Forbidden),
            None => {}
        }
        Ok(delivery.event().clone())
    }

    async fn authorize_participants(
        &self,
        context: &AuthContext,
        actor_ids: &[ActorId],
    ) -> AppResult<Vec<ActorRef>> {
        self.visible_participants(context, actor_ids).await
    }

    async fn authorize_presence(
        &self,
        context: &AuthContext,
        actor_id: &ActorId,
    ) -> AppResult<ActorRef> {
        self.visible_participants(context, std::slice::from_ref(actor_id))
            .await?
            .pop()
            .ok_or(AppError::Forbidden)
    }
}

fn mutation(operation: &str, key: &str) -> AppResult<Mutation> {
    // Stable namespace and digest preserve DM's idempotency grammar without exposing tokens.
    let digest = blake3::hash(format!("dm-auth:{operation}:{key}").as_bytes()).to_hex();
    let key = IdempotencyKey::parse(digest.as_str())
        .map_err(|_| AppError::validation("invalid idempotency key"))?;
    Ok(Mutation::with_key(key))
}

fn validate_token(value: &str, prefix: &str) -> AppResult<()> {
    if !value.starts_with(prefix)
        || value.len() <= prefix.len()
        || value.len() > 8192
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(AppError::Unauthorized);
    }
    Ok(())
}

fn validate_opaque_token(value: &str) -> AppResult<()> {
    if value.is_empty() || value.len() > 8192 || !value.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err(AppError::Unauthorized);
    }
    Ok(())
}

fn actor_ref(actor_type: ActorType, public_id: &str) -> AppResult<ActorRef> {
    if public_id.trim() != public_id {
        return Err(dependency_unavailable());
    }
    Ok(ActorRef {
        actor_type,
        id: public_id.parse().map_err(|_| dependency_unavailable())?,
    })
}

fn iam_actor(actor: &models::ActorRef) -> AppResult<ActorRef> {
    let actor_type = match actor.type_field {
        models::ActorRefType::Carbon => ActorType::Carbon,
        models::ActorRefType::Silicon => ActorType::Silicon,
        _ => return Err(AppError::Forbidden),
    };
    actor_ref(actor_type, &actor.public_id)
}

#[allow(clippy::needless_pass_by_value)] // Function adapter consumes map_err's owned SDK error.
fn map_error(error: silicon_iam_client::Error) -> AppError {
    match error.api().map(|error| error.status) {
        Some(400 | 401 | 410) => AppError::Unauthorized,
        Some(403 | 404) => AppError::Forbidden,
        Some(409) => AppError::conflict(
            "IAM rejected a reused or conflicting operation; retry the original request with its original idempotency key",
        ),
        Some(422) => AppError::validation("IAM rejected the request"),
        Some(429) => AppError::RateLimited,
        _ => {
            // SDK failures may carry provider text; never log it or tokens.
            tracing::warn!(dependency = "iam", "IAM request could not be verified");
            dependency_unavailable()
        }
    }
}

fn dependency_unavailable() -> AppError {
    AppError::DependencyUnavailable { dependency: "iam" }
}
