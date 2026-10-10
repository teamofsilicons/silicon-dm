//! Publishes durable handoffs to Ting with DM's own App verification proof.
//!
//! DM, not the account that caused an event, is the sender, so one App verification proof
//! (receiving app Ting, named by `DM_TING_APP_ID`; scope `tings.send`) covers every send.
//! It is cached and replaced shortly before it expires; one refresh runs at a time.

use std::sync::Arc;

use async_trait::async_trait;
use secrecy::SecretString;
use time::OffsetDateTime;
use tokio::sync::Mutex;

use crate::{
    application::ports::IdentityProvider,
    infrastructure::{
        postgres::TingDeliveryClaim,
        ting::{TingAcceptance, TingFailure, TingPublisher, TingSendAuthority, TingSocket},
    },
};

/// Scope DM asks for to send to Ting.
pub const SEND_SCOPE: &str = "tings.send";
/// Replace the proof when it has less than this left.
const REFRESH_MARGIN: time::Duration = time::Duration::seconds(60);

struct CachedProof {
    token: SecretString,
    expires_at: Option<OffsetDateTime>,
}

/// Sends with DM's App verification proof for Ting.
pub struct ProofTingPublisher {
    identity: Arc<dyn IdentityProvider>,
    socket: TingSocket,
    /// Ting's app id in Silicon Accounts (`DM_TING_APP_ID`).
    ting_app: String,
    proof: Mutex<Option<CachedProof>>,
}

impl ProofTingPublisher {
    /// A publisher over one prewarmed socket; `ting_app` is Ting's app id in Silicon
    /// Accounts (the receiving app of DM's proof).
    #[must_use]
    pub fn new(identity: Arc<dyn IdentityProvider>, socket: TingSocket, ting_app: String) -> Self {
        Self {
            identity,
            socket,
            ting_app,
            proof: Mutex::new(None),
        }
    }

    async fn authority(&self) -> Result<TingSendAuthority, TingFailure> {
        let mut cached = self.proof.lock().await;
        let fresh = cached.as_ref().is_some_and(|proof| {
            proof
                .expires_at
                .is_none_or(|expires| expires - REFRESH_MARGIN > OffsetDateTime::now_utc())
        });
        if !fresh {
            let key = format!("dm-ting-send-{}", uuid::Uuid::now_v7());
            let issued = self
                .identity
                .issue_app_verification(&self.ting_app, &[SEND_SCOPE], &key)
                .await
                .map_err(|error| {
                    tracing::warn!(code = error.code(), "issuing DM's proof for Ting failed");
                    TingFailure::AuthorityUnavailable
                })?;
            *cached = Some(CachedProof {
                token: issued.token,
                expires_at: issued.expires_at,
            });
        }
        let proof = cached.as_ref().ok_or(TingFailure::AuthorityUnavailable)?;
        Ok(TingSendAuthority {
            proof_token: proof.token.clone(),
        })
    }

    /// Forgets the cached proof (after Ting refused it).
    async fn forget(&self) {
        *self.proof.lock().await = None;
    }
}

#[async_trait]
impl TingPublisher for ProofTingPublisher {
    async fn publish(&self, claim: &TingDeliveryClaim) -> Result<TingAcceptance, TingFailure> {
        let authority = self.authority().await?;
        let result = self.socket.send(&claim.request_body, authority).await;
        if let Err(TingFailure::Rejected(code)) = &result
            && matches!(
                code.as_str(),
                "invalid_proof" | "unauthorized" | "proof_expired"
            )
        {
            self.forget().await;
        }
        result
    }
}
