//! Authenticated request context.

use std::{collections::BTreeSet, fmt};

use secrecy::SecretString;

use crate::domain::{ActorId, ActorRef, OrganizationId};

/// Short-lived, audience-bound credential minted for one downstream service.
#[derive(Clone)]
pub struct DelegatedCredential {
    proof: SecretString,
    issuer_app_id: String,
}

impl DelegatedCredential {
    /// Creates a credential after the IAM adapter has validated its response.
    #[must_use]
    pub(crate) fn new(proof: SecretString, issuer_app_id: String) -> Self {
        Self {
            proof,
            issuer_app_id,
        }
    }

    /// Borrows the audience-bound proof.
    #[must_use]
    pub(crate) const fn proof(&self) -> &SecretString {
        &self.proof
    }

    /// Returns the application that exchanged the proof.
    #[must_use]
    pub(crate) fn issuer_app_id(&self) -> &str {
        &self.issuer_app_id
    }
}

impl fmt::Debug for DelegatedCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DelegatedCredential")
            .field("proof", &"[REDACTED]")
            .field("issuer_app_id", &self.issuer_app_id)
            .finish()
    }
}

/// Credential type used for one incoming request.
#[derive(Clone, Debug)]
pub enum PresentedCredential {
    /// Opaque IAM bearer token.
    Bearer(SecretString),
    /// Audience-bound IAM OBO proof and originating application.
    Obo {
        /// Proof value.
        proof: SecretString,
        /// Originating application identifier.
        app_id: String,
    },
    /// Opaque service token accepted only by internal routes.
    Service(SecretString),
}

/// Authority proven by IAM for one request.
#[derive(Clone, Debug)]
pub struct AuthContext {
    /// Represented actor.
    pub actor: ActorRef,
    /// Active organization selected by the request.
    pub organization_id: OrganizationId,
    /// Actors this credential may represent on a realtime connection.
    pub represented_actor_ids: BTreeSet<ActorId>,
    /// IAM capabilities/scopes.
    pub capabilities: BTreeSet<String>,
    /// Original credential for narrowly scoped downstream delegation.
    pub credential: PresentedCredential,
}

impl AuthContext {
    /// Returns whether the principal may represent the requested actor.
    #[must_use]
    pub fn may_represent(&self, actor_id: &ActorId) -> bool {
        self.actor.id == *actor_id || self.represented_actor_ids.contains(actor_id)
    }

    /// Returns whether IAM granted a named capability.
    #[must_use]
    pub fn has_capability(&self, capability: &str) -> bool {
        self.capabilities.contains(capability)
    }
}

/// IAM-verified service principal for an internal route.
#[derive(Clone, Debug)]
pub struct ServiceContext {
    /// Stable service/application identifier.
    pub service_id: String,
    /// Granted scopes.
    pub capabilities: BTreeSet<String>,
    /// Request-scoped service bearer used only for IAM authorization lookups.
    pub credential: SecretString,
}
