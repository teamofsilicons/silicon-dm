//! Connects durable handoffs to the initiating account's current IAM authority.

use std::sync::Arc;

use async_trait::async_trait;
use uuid::Uuid;

use crate::{
    AppError,
    application::ports::{AuthenticationRequest, IdentityProvider},
    infrastructure::{
        postgres::TingDeliveryClaim,
        ting::{TingAcceptance, TingFailure, TingPublisher, TingSocket},
        ting_credentials::TingCredentialCache,
    },
};

/// Each attempt revalidates only credentials belonging to its immutable originator.
/// Clients retain exclusive refresh-token ownership; this publisher never rotates
/// a session or borrows another logged-in account's authority.
pub struct AuthenticatedTingPublisher {
    credentials: TingCredentialCache,
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
            credentials,
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
        let candidates = self
            .credentials
            .candidates(&claim.organization_id, originator)
            .await
            .map_err(|_| TingFailure::AuthorityUnavailable)?;
        for candidate in candidates {
            let context = match self
                .identity
                .authenticate(AuthenticationRequest::Bearer {
                    token: &candidate.token,
                    organization_id: &claim.organization_id,
                })
                .await
            {
                Ok(context)
                    if context.actor == *originator
                        && context.organization_id == claim.organization_id =>
                {
                    context
                }
                Ok(_) | Err(AppError::Unauthorized | AppError::Forbidden) => {
                    self.credentials
                        .forget(&claim.organization_id, originator, &candidate.digest)
                        .await
                        .map_err(|_| TingFailure::AuthorityUnavailable)?;
                    continue;
                }
                Err(_) => return Err(TingFailure::AuthorityUnavailable),
            };
            let authority = match self
                .identity
                .issue_ting_send_proof(&context, &claim.request_body, &Uuid::new_v4().to_string())
                .await
            {
                Ok(authority) => authority,
                Err(AppError::Unauthorized | AppError::Forbidden) => {
                    self.credentials
                        .forget(&claim.organization_id, originator, &candidate.digest)
                        .await
                        .map_err(|_| TingFailure::AuthorityUnavailable)?;
                    continue;
                }
                Err(_) => return Err(TingFailure::AuthorityUnavailable),
            };
            // No in-memory resend: an uncertain acceptance keeps the unchanged
            // durable body/key for a later attempt with a fresh one-use proof.
            return self.socket.send(&claim.request_body, authority).await;
        }
        Err(TingFailure::OriginatorAuthenticationRequired)
    }
}
