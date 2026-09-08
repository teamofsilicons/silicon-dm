//! IAM-authenticated request and application-session types.

use std::collections::BTreeSet;

use secrecy::SecretString;
use serde::Serialize;
use uuid::Uuid;

use crate::domain::{ActorId, ActorRef, OrganizationId};

/// The IAM application access token presented by this request.
#[derive(Clone, Debug)]
pub enum PresentedCredential {
    /// Opaque IAM application bearer token, redacted in diagnostic output.
    Bearer(SecretString),
}

/// Authority proven by a fresh IAM application introspection.
#[derive(Clone, Debug)]
pub struct AuthContext {
    /// Represented Carbon or Silicon, using IAM's canonical public identifier.
    pub actor: ActorRef,
    /// IAM's stable principal UUID.
    pub principal_id: Uuid,
    /// IAM session identifier when disclosed by introspection.
    pub session_id: Option<Uuid>,
    /// Active organization selected by the request.
    pub organization_id: OrganizationId,
    /// Current organization role, only when IAM discloses it.
    pub org_role: Option<String>,
    /// Actors this credential may represent on a realtime connection.
    pub represented_actor_ids: BTreeSet<ActorId>,
    /// Effective IAM scopes; undisclosed authority is never inferred.
    pub capabilities: BTreeSet<String>,
    /// Request-scoped token, never persisted by the backend.
    pub credential: PresentedCredential,
}

impl AuthContext {
    /// Whether the principal may represent the requested actor.
    #[must_use]
    pub fn may_represent(&self, actor_id: &ActorId) -> bool {
        self.actor.id == *actor_id || self.represented_actor_ids.contains(actor_id)
    }

    /// Whether IAM granted a named scope.
    #[must_use]
    pub fn has_capability(&self, capability: &str) -> bool {
        self.capabilities.contains(capability)
    }

    /// Whether the disclosed current role permits organization administration.
    #[must_use]
    pub fn is_org_administrator(&self) -> bool {
        matches!(
            self.org_role.as_deref(),
            Some("owner" | "admin" | "org_owner" | "org_admin" | "org_head")
        )
    }
}

/// Application session returned only by login and refresh. Never log this value.
#[derive(Serialize)]
pub struct ApplicationSession {
    /// Opaque IAM application access token.
    pub access_token: String,
    /// Rotating IAM application refresh token; persist before using the access token.
    pub refresh_token: String,
    /// Always `Bearer`.
    pub token_type: &'static str,
    /// Access token lifetime in seconds.
    pub expires_in: i64,
    /// Effective space-separated scopes.
    pub scope: String,
    /// Verified Carbon or Silicon identity.
    pub actor: ActorRef,
    /// Organization to supply as `X-Org-ID` on subsequent requests.
    pub organization_id: OrganizationId,
    /// Organizations explicitly selected in IAM and currently authorized.
    pub organization_ids: Vec<OrganizationId>,
}
