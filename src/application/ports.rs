//! External service interfaces used by application workflows.

use async_trait::async_trait;
use secrecy::SecretString;
use time::OffsetDateTime;
use url::Url;
use uuid::Uuid;

use crate::{
    AppResult,
    application::auth::{AuthContext, DelegatedCredential},
    domain::{ActorId, ActorRef, Gif, OrganizationId},
};

const BRIEFCASE_TEMPORARY_URL_ACTION: &str = "briefcase.file.temporary_url";

/// Exact audience, action, and optional resource for one IAM child exchange.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DelegationRequest {
    audience: String,
    action: &'static str,
    resource: Option<String>,
}

impl DelegationRequest {
    /// Creates the scope for a Briefcase temporary-download URL.
    #[must_use]
    pub fn briefcase_temporary_url(audience: &str, entry_id: Uuid) -> Self {
        Self {
            audience: audience.to_owned(),
            action: BRIEFCASE_TEMPORARY_URL_ACTION,
            resource: Some(entry_id.hyphenated().to_string()),
        }
    }

    /// Returns the target IAM application audience.
    #[must_use]
    pub fn audience(&self) -> &str {
        &self.audience
    }

    /// Returns the single delegated action.
    #[must_use]
    pub const fn action(&self) -> &'static str {
        self.action
    }

    /// Returns the target-service resource binding, when required.
    #[must_use]
    pub fn resource(&self) -> Option<&str> {
        self.resource.as_deref()
    }
}

/// Parsed inbound authentication material.
pub enum AuthenticationRequest<'a> {
    /// End-user bearer token.
    Bearer {
        /// Token value.
        token: &'a SecretString,
        /// Selected organization.
        organization_id: &'a OrganizationId,
    },
    /// On-behalf-of proof.
    Obo {
        /// Proof value.
        proof: &'a SecretString,
        /// Originating application.
        app_id: &'a str,
        /// Selected organization.
        organization_id: &'a OrganizationId,
        /// DM action being attempted.
        action: &'a str,
        /// Optional resource binding.
        resource: Option<&'a str>,
    },
}

/// Identity and authorization operations owned by Silicon IAM.
#[async_trait]
pub trait IdentityProvider: Send + Sync {
    /// Authenticates and authorizes a normal API request.
    async fn authenticate(&self, request: AuthenticationRequest<'_>) -> AppResult<AuthContext>;

    /// Exchanges the actor's bearer grant for a provider-scoped OBO proof.
    async fn exchange_actor_credential(
        &self,
        context: &AuthContext,
        request: &DelegationRequest,
    ) -> AppResult<DelegatedCredential>;

    /// Resolves and authorizes active actors for one conversation.
    async fn authorize_participants(
        &self,
        context: &AuthContext,
        actor_ids: &[ActorId],
    ) -> AppResult<Vec<ActorRef>>;

    /// Verifies that the caller may observe another actor's presence.
    async fn authorize_presence(
        &self,
        context: &AuthContext,
        actor_id: &ActorId,
    ) -> AppResult<ActorRef>;
}

/// Temporary Briefcase URL result.
#[derive(Clone, Debug)]
pub struct TemporaryAttachmentUrl {
    /// Expiring CDN URL.
    pub url: Url,
    /// Expiry instant.
    pub expires_at: OffsetDateTime,
}

/// Briefcase operations required by DM.
#[async_trait]
pub trait AttachmentProvider: Send + Sync {
    /// Validates a permanent Briefcase URL and requests an expiring URL.
    async fn temporary_url(
        &self,
        permanent_url: &Url,
        organization_id: &OrganizationId,
        credential: &DelegatedCredential,
    ) -> AppResult<TemporaryAttachmentUrl>;
}

/// Giphy operations required by DM.
#[async_trait]
pub trait GifProvider: Send + Sync {
    /// Returns current safe trending results.
    async fn trending(&self) -> AppResult<Vec<Gif>>;

    /// Searches safe provider results.
    async fn search(&self, query: &str) -> AppResult<Vec<Gif>>;
}
