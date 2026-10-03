//! Connects durable handoffs to the initiating account's current IAM authority.

use std::sync::Arc;

use async_trait::async_trait;

use crate::{
    AppError,
    application::ports::IdentityProvider,
    infrastructure::{
        postgres::TingDeliveryClaim,
        ting::{TingAcceptance, TingFailure, TingPublisher, TingSocket},
        ting_credentials::TingCredentialCache,
    },
};

/// Each attempt revalidates only credentials belonging to its immutable originator.
/// The broker rotates only dedicated OBO credentials. It never rotates an
/// ordinary login or borrows another account's grant.
pub struct AuthenticatedTingPublisher {
    credentials: super::ting_authorization::TingAuthorization,
    identity: Arc<dyn IdentityProvider>,
    socket: TingSocket,
}

impl AuthenticatedTingPublisher {
    /// Constructs the publisher from one selected application's data/auth plane.
    #[must_use]
    pub fn new(
        credentials: TingCredentialCache,
        identity: Arc<dyn IdentityProvider>,
        socket: TingSocket,
    ) -> Self {
        Self {
            credentials: super::ting_authorization::TingAuthorization(credentials),
            identity,
            socket,
        }
    }
}

#[async_trait]
impl TingPublisher for AuthenticatedTingPublisher {
    async fn publish(&self, claim: &TingDeliveryClaim) -> Result<TingAcceptance, TingFailure> {
        let originator = claim
            .originator
            .as_ref()
            .ok_or(TingFailure::OriginatorAuthenticationRequired)?;
        let body: serde_json::Value =
            serde_json::from_str(&claim.request_body).map_err(|_| TingFailure::InvalidRequest)?;
        if body["org_id"] != claim.organization_id.as_str()
            || body["type"] != format!("{}.sync.changed", self.credentials.0.context.app_id)
        {
            return Err(TingFailure::InvalidRequest);
        }
        let authority = self
            .credentials
            .authority(
                self.identity.as_ref(),
                &claim.organization_id,
                originator,
                "tings.send",
            )
            .await
            .map_err(|error| match error {
                AppError::TingAuthorizationRequired
                | AppError::Unauthorized
                | AppError::Forbidden => TingFailure::OriginatorAuthenticationRequired,
                _ => TingFailure::AuthorityUnavailable,
            })?;
        self.socket.send(&claim.request_body, authority).await
    }
}
