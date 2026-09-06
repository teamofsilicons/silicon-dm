//! External service interfaces used by application workflows.

use async_trait::async_trait;
use secrecy::SecretString;

use crate::{
    AppResult,
    application::auth::{ApplicationSession, AuthContext},
    domain::{ActorId, ActorRef, Gif, OrganizationId},
};

/// Parsed inbound authentication material.
pub enum AuthenticationRequest<'a> {
    /// End-user bearer token.
    Bearer {
        /// Token value.
        token: &'a SecretString,
        /// Selected organization.
        organization_id: &'a OrganizationId,
    },
}

/// Identity and authorization operations owned by Silicon IAM.
#[async_trait]
pub trait IdentityProvider: Send + Sync {
    /// Authenticates and authorizes a normal API request.
    async fn authenticate(&self, request: AuthenticationRequest<'_>) -> AppResult<AuthContext>;

    /// Exchanges a single-use IAM SLT using the backend's application secret.
    async fn login(
        &self,
        slt: &SecretString,
        idempotency_key: &str,
    ) -> AppResult<ApplicationSession>;

    /// Rotates an existing application session, preserving retry identity.
    async fn refresh(
        &self,
        token: &SecretString,
        idempotency_key: &str,
    ) -> AppResult<ApplicationSession>;

    /// Revokes an application access token or an entire refresh-token family.
    async fn logout(&self, token: &SecretString, idempotency_key: &str) -> AppResult<()>;

    /// Authenticates exact webhook bytes and binds the event to this IAM plane.
    ///
    /// # Errors
    /// Rejects invalid signatures, expired deliveries, and mismatched IAM planes.
    fn verify_webhook(
        &self,
        headers: &http::HeaderMap,
        body: &[u8],
    ) -> AppResult<silicon_iam_client::models::WebhookEvent>;

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

/// Giphy operations required by DM.
#[async_trait]
pub trait GifProvider: Send + Sync {
    /// Returns current safe trending results.
    async fn trending(&self) -> AppResult<Vec<Gif>>;

    /// Searches safe provider results.
    async fn search(&self, query: &str) -> AppResult<Vec<Gif>>;
}
