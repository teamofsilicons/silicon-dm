//! Automatic Ting recipient enrollment for every DM member.
//!
//! Receiving DM updates through Ting is not a member decision: consenting to DM
//! at IAM login covers it. Ting still requires each registration to be proven by
//! the recipient's own session, so DM enrolls a member whenever it holds one of
//! their verified sessions: login, any authenticated request, or a cached access
//! token found by the worker's sweep. Enrollment never stores tokens or proofs.

use std::{
    collections::HashSet,
    sync::{Arc, LazyLock, Mutex, MutexGuard, PoisonError},
    time::Duration,
};

use uuid::Uuid;

use crate::{
    AppError, AppResult,
    application::{
        auth::AuthContext,
        ports::{AuthenticationRequest, IdentityProvider},
    },
    config::TingSettings,
    domain::{ActorId, ActorRef, ActorType, OrganizationId},
    infrastructure::{
        postgres::{PostgresStore, TingDeliveryContext},
        ting_credentials::TingCredentialCache,
    },
};

const REQUIRED_SCOPES: [&str; 2] = ["self.identity.read", "obo:ting:subscriptions.register"];
/// Ting OBO endpoints DM declares in its IAM application scope. Consenting to DM
/// at login grants them; members never approve Ting separately.
pub const TING_OBO_SCOPES: [&str; 2] = ["obo:ting:subscriptions.register", "obo:ting:tings.send"];

/// Whether this session predates DM's Ting permissions and must sign in again once.
/// IAM refresh only reissues the scopes originally consented to, so such a session
/// never gains them; a new login presents DM's current permissions for consent.
#[must_use]
pub fn reconsent_required(context: &AuthContext) -> bool {
    !TING_OBO_SCOPES
        .iter()
        .all(|scope| context.has_capability(scope))
}
/// Failed or uncertain attempts wait this long before another session retries.
const RETRY_COOLDOWN_SECONDS: f64 = 300.0;
/// Actors examined per worker sweep; the sweep runs every maintenance tick.
const SWEEP_BATCH: i64 = 50;
/// Bounds the in-process memo; clearing it only costs one indexed lookup per actor.
const MEMO_LIMIT: usize = 100_000;

/// Enrolled actors seen by this process, so ordinary requests skip the database.
static ENROLLED: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Mutex::default);

/// The memo holds no invariants a panicking holder could break.
fn enrolled() -> MutexGuard<'static, HashSet<String>> {
    ENROLLED.lock().unwrap_or_else(PoisonError::into_inner)
}

fn remember(key: String) {
    let mut enrolled = enrolled();
    if enrolled.len() >= MEMO_LIMIT {
        enrolled.clear();
    }
    enrolled.insert(key);
}

/// Outcome of one enrollment check; none of these fail the caller's request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Enrollment {
    /// Ting holds an active grant for this member.
    Enrolled,
    /// Another attempt is running or failed within the cooldown.
    Deferred,
    /// The session lacks the Ting registration scope; a later session may have it.
    MissingScope,
}

/// Enrolls members with Ting using their own verified IAM sessions.
#[derive(Clone)]
pub struct TingAutoEnrollment {
    store: PostgresStore,
    identity: Arc<dyn IdentityProvider>,
    settings: TingSettings,
    context: TingDeliveryContext,
}

impl TingAutoEnrollment {
    /// Binds enrollment to one DM data plane and its identity adapter.
    #[must_use]
    pub fn new(
        store: PostgresStore,
        identity: Arc<dyn IdentityProvider>,
        settings: TingSettings,
        context: TingDeliveryContext,
    ) -> Self {
        Self {
            store,
            identity,
            settings,
            context,
        }
    }

    fn generation(&self) -> i64 {
        self.context.testing_generation.unwrap_or(0)
    }

    fn memo_key(&self, org: &OrganizationId, actor: &ActorRef) -> String {
        format!(
            "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
            self.context
                .testing_environment_id
                .map(|id| id.simple().to_string())
                .unwrap_or_default(),
            self.context.app_id,
            self.generation(),
            org.as_str(),
            actor.actor_type.as_str(),
            actor.id.as_str()
        )
    }

    /// Enrolls the session's own actor unless Ting already holds its grant.
    ///
    /// # Errors
    /// Reports storage failures. IAM and Ting failures are recorded on the
    /// enrollment row and retried after the cooldown by a later session.
    pub async fn ensure(&self, context: &AuthContext) -> AppResult<Enrollment> {
        if !REQUIRED_SCOPES
            .iter()
            .all(|scope| context.has_capability(scope))
        {
            return Ok(Enrollment::MissingScope);
        }
        let key = self.memo_key(&context.organization_id, &context.actor);
        if enrolled().contains(&key) {
            return Ok(Enrollment::Enrolled);
        }
        // Claim the attempt: a new row, or an unenrolled row past its cooldown.
        let claimed: Option<bool> = sqlx::query_scalar(
            "INSERT INTO ting_automatic_enrollments(app_id,generation,organization_id,actor_kind,actor_id) \
             VALUES($1,$2,$3,$4::text::actor_kind,$5) \
             ON CONFLICT (app_id,generation,organization_id,actor_kind,actor_id) DO UPDATE \
             SET attempted_at=clock_timestamp(),last_error=NULL \
             WHERE ting_automatic_enrollments.enrolled_at IS NULL \
             AND ting_automatic_enrollments.attempted_at <= clock_timestamp()-make_interval(secs=>$6) \
             RETURNING true",
        )
        .bind(&self.context.app_id)
        .bind(self.generation())
        .bind(context.organization_id.as_str())
        .bind(context.actor.actor_type.as_str())
        .bind(context.actor.id.as_str())
        .bind(RETRY_COOLDOWN_SECONDS)
        .fetch_optional(self.store.pool())
        .await?;
        if claimed.is_none() {
            let enrolled: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM ting_automatic_enrollments WHERE app_id=$1 AND generation=$2 \
                 AND organization_id=$3 AND actor_kind=$4::text::actor_kind AND actor_id=$5 AND enrolled_at IS NOT NULL)",
            )
            .bind(&self.context.app_id)
            .bind(self.generation())
            .bind(context.organization_id.as_str())
            .bind(context.actor.actor_type.as_str())
            .bind(context.actor.id.as_str())
            .fetch_one(self.store.pool())
            .await?;
            if enrolled {
                remember(key);
                return Ok(Enrollment::Enrolled);
            }
            return Ok(Enrollment::Deferred);
        }
        let outcome = self
            .identity
            .register_ting_delivery(context, &self.settings, &Uuid::new_v4().to_string())
            .await;
        match outcome {
            Ok(subscription) => {
                let id = subscription["id"]
                    .as_str()
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| {
                        AppError::internal(anyhow::anyhow!("Ting subscription result has no ID"))
                    })?;
                sqlx::query(
                    "UPDATE ting_automatic_enrollments SET enrolled_at=clock_timestamp(),subscription_id=$6,last_error=NULL \
                     WHERE app_id=$1 AND generation=$2 AND organization_id=$3 AND actor_kind=$4::text::actor_kind AND actor_id=$5",
                )
                .bind(&self.context.app_id)
                .bind(self.generation())
                .bind(context.organization_id.as_str())
                .bind(context.actor.actor_type.as_str())
                .bind(context.actor.id.as_str())
                .bind(id)
                .execute(self.store.pool())
                .await?;
                remember(key);
                tracing::info!(
                    actor_kind = context.actor.actor_type.as_str(),
                    "DM enrolled a member with Ting"
                );
                Ok(Enrollment::Enrolled)
            }
            Err(error) => {
                sqlx::query(
                    "UPDATE ting_automatic_enrollments SET last_error=$6 \
                     WHERE app_id=$1 AND generation=$2 AND organization_id=$3 AND actor_kind=$4::text::actor_kind AND actor_id=$5 \
                     AND enrolled_at IS NULL",
                )
                .bind(&self.context.app_id)
                .bind(self.generation())
                .bind(context.organization_id.as_str())
                .bind(context.actor.actor_type.as_str())
                .bind(context.actor.id.as_str())
                .bind(error.code())
                .execute(self.store.pool())
                .await?;
                tracing::warn!(
                    code = error.code(),
                    "Automatic Ting enrollment will retry with a later session"
                );
                Ok(Enrollment::Deferred)
            }
        }
    }

    async fn defer(&self, org: &OrganizationId, actor: &ActorRef, code: &str) -> AppResult<()> {
        sqlx::query(
            "INSERT INTO ting_automatic_enrollments(app_id,generation,organization_id,actor_kind,actor_id,last_error) \
             VALUES($1,$2,$3,$4::text::actor_kind,$5,$6) \
             ON CONFLICT (app_id,generation,organization_id,actor_kind,actor_id) DO UPDATE \
             SET attempted_at=clock_timestamp(),last_error=EXCLUDED.last_error \
             WHERE ting_automatic_enrollments.enrolled_at IS NULL",
        )
        .bind(&self.context.app_id)
        .bind(self.generation())
        .bind(org.as_str())
        .bind(actor.actor_type.as_str())
        .bind(actor.id.as_str())
        .bind(code)
        .execute(self.store.pool())
        .await?;
        Ok(())
    }

    /// Forgets an enrollment Ting no longer honours, so the next session re-enrolls.
    ///
    /// # Errors
    /// Reports storage failures.
    pub async fn forget(&self, org: &OrganizationId, actor: &ActorRef) -> AppResult<()> {
        enrolled().remove(&self.memo_key(org, actor));
        sqlx::query(
            "DELETE FROM ting_automatic_enrollments WHERE app_id=$1 AND generation=$2 \
             AND organization_id=$3 AND actor_kind=$4::text::actor_kind AND actor_id=$5",
        )
        .bind(&self.context.app_id)
        .bind(self.generation())
        .bind(org.as_str())
        .bind(actor.actor_type.as_str())
        .bind(actor.id.as_str())
        .execute(self.store.pool())
        .await?;
        Ok(())
    }

    /// Backfills members DM already holds a cached, unexpired session for.
    /// Members with no live session enroll on their next DM request instead.
    ///
    /// # Errors
    /// Reports storage failures; per-member IAM or Ting failures only defer that member.
    pub async fn sweep(&self, credentials: &TingCredentialCache) -> AppResult<usize> {
        let actors: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT DISTINCT c.organization_id,c.actor_kind::text,c.actor_id FROM ting_credentials c \
             WHERE c.app_id=$1 AND c.generation=$2 AND c.expires_at>clock_timestamp() \
             AND NOT EXISTS (SELECT 1 FROM ting_automatic_enrollments e WHERE e.app_id=c.app_id \
               AND e.generation=c.generation AND e.organization_id=c.organization_id \
               AND e.actor_kind=c.actor_kind AND e.actor_id=c.actor_id \
               AND (e.enrolled_at IS NOT NULL OR e.attempted_at>clock_timestamp()-make_interval(secs=>$3))) \
             ORDER BY 1,2,3 LIMIT $4",
        )
        .bind(&self.context.app_id)
        .bind(self.generation())
        .bind(RETRY_COOLDOWN_SECONDS)
        .bind(SWEEP_BATCH)
        .fetch_all(self.store.pool())
        .await?;
        let mut enrolled = 0;
        for (org, kind, id) in actors {
            let (Ok(org), Ok(actor_type), Ok(id)) = (
                org.parse::<OrganizationId>(),
                kind.parse::<ActorType>(),
                id.parse::<ActorId>(),
            ) else {
                continue;
            };
            let actor = ActorRef { actor_type, id };
            let mut outcome = None;
            for candidate in credentials.candidates(&org, &actor).await? {
                let Ok(context) = self
                    .identity
                    .authenticate(AuthenticationRequest::Bearer {
                        token: &candidate.token,
                        organization_id: &org,
                    })
                    .await
                else {
                    continue;
                };
                if context.actor != actor || context.organization_id != org {
                    continue;
                }
                let result = self.ensure(&context).await?;
                outcome = Some(result);
                // An older cached token may predate the scope; try the next one.
                if result != Enrollment::MissingScope {
                    break;
                }
            }
            match outcome {
                Some(Enrollment::Enrolled) => enrolled += 1,
                // Without a usable session, wait out the cooldown so these members
                // cannot fill every sweep batch ahead of enrollable ones.
                Some(Enrollment::MissingScope) | None => {
                    self.defer(&org, &actor, "ting_registration_scope_missing")
                        .await?;
                }
                Some(Enrollment::Deferred) => {}
            }
        }
        Ok(enrolled)
    }
}

/// Runs enrollment after the request has its answer; never delays or fails it.
pub fn spawn_ensure(enrollment: TingAutoEnrollment, context: AuthContext) {
    tokio::spawn(async move {
        match tokio::time::timeout(Duration::from_secs(30), enrollment.ensure(&context)).await {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => tracing::warn!(
                code = error.code(),
                "Automatic Ting enrollment will retry on the next DM request"
            ),
            Err(_) => {
                tracing::warn!("Automatic Ting enrollment timed out; the next DM request retries");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use secrecy::SecretString;
    use time::OffsetDateTime;

    use super::{TING_OBO_SCOPES, reconsent_required};
    use crate::{
        application::auth::{AuthContext, PresentedCredential},
        domain::{ActorRef, ActorType},
    };

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

    fn session(scopes: &[&str]) -> TestResult<AuthContext> {
        let actor = ActorRef {
            actor_type: ActorType::Carbon,
            id: "c:saket".parse()?,
        };
        Ok(AuthContext {
            represented_actor_ids: BTreeSet::from([actor.id.clone()]),
            actor,
            session_id: None,
            organization_id: "tos".parse()?,
            org_role: None,
            tag_ids: None,
            capabilities: scopes.iter().map(|scope| (*scope).to_owned()).collect(),
            credential: PresentedCredential::Bearer(SecretString::from("fixture".to_owned())),
            credential_expires_at: OffsetDateTime::now_utc(),
        })
    }

    #[test]
    fn sessions_without_both_ting_permissions_must_sign_in_again() -> TestResult {
        assert!(!reconsent_required(&session(&[
            "self.identity.read",
            TING_OBO_SCOPES[0],
            TING_OBO_SCOPES[1],
        ])?));
        assert!(reconsent_required(&session(&["self.identity.read"])?));
        assert!(reconsent_required(&session(&[
            "self.identity.read",
            "obo:ting:tings.send",
        ])?));
        assert!(reconsent_required(&session(&[
            "self.identity.read",
            "obo:ting:subscriptions.register",
        ])?));
        Ok(())
    }
}
