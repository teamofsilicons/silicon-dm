//! The authenticated caller of one request.

use std::collections::BTreeSet;

use secrecy::SecretString;
use time::OffsetDateTime;

use crate::{
    AppError, AppResult,
    domain::{ActorRef, ActorType},
};

/// How the caller proved who it is.
#[derive(Clone, Debug)]
pub enum Credential {
    /// A Silicon Accounts access token issued to DM (`aud` = DM's app id). Kept only for
    /// the length of the request, to mint a User verification proof for Ting enrolment.
    AccessToken(SecretString),
    /// A User verification proof another app presented to act for the account.
    Proof {
        /// The app that obtained the account's agreement and issued the proof.
        issuing_app: String,
        /// Scopes the proof carries (DM honours the `dm.*` ones it documents).
        scopes: BTreeSet<String>,
    },
}

/// Authority of one request, verified with Silicon Accounts.
#[derive(Clone, Debug)]
pub struct AuthContext {
    /// The account acting (storage key = its Accounts uuid).
    pub actor: ActorRef,
    /// Its current public id (`c:saket`), for messages and logs.
    pub public_id: String,
    /// Membership id with DM (`dm:{uuid}`), when the token carried one.
    pub membership_id: Option<String>,
    /// Refresh-token family of the sign-in, when the token carried one.
    pub token_family: Option<String>,
    /// Scopes of the access token (profile fields the account shared with DM).
    pub scopes: BTreeSet<String>,
    /// How the caller authenticated.
    pub credential: Credential,
    /// When the token was issued, when known.
    pub issued_at: Option<OffsetDateTime>,
    /// When the credential stops being valid.
    pub expires_at: OffsetDateTime,
}

impl AuthContext {
    /// Whether the caller is a Silicon.
    #[must_use]
    pub fn is_silicon(&self) -> bool {
        self.actor.actor_type == ActorType::Silicon
    }

    /// The app acting for the account through a proof, if any.
    #[must_use]
    pub fn via_app(&self) -> Option<&str> {
        match &self.credential {
            Credential::Proof { issuing_app, .. } => Some(issuing_app),
            Credential::AccessToken(_) => None,
        }
    }

    /// The caller's own DM access token (absent for proofs).
    #[must_use]
    pub fn access_token(&self) -> Option<&SecretString> {
        match &self.credential {
            Credential::AccessToken(token) => Some(token),
            Credential::Proof { .. } => None,
        }
    }

    /// Refuses a route that an app acting through a proof may not use.
    ///
    /// # Errors
    /// `proof_not_accepted` when the request came with a proof.
    pub fn require_own_session(&self, what: &str) -> AppResult<()> {
        match &self.credential {
            Credential::AccessToken(_) => Ok(()),
            Credential::Proof { issuing_app, .. } => Err(AppError::forbidden(
                "proof_not_accepted",
                format!(
                    "{what} needs the account's own DM sign-in; {issuing_app} is acting through a User verification proof, which DM does not accept here."
                ),
            )),
        }
    }
}
