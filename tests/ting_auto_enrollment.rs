//! Automatic Ting enrollment proof against isolated PostgreSQL.

mod support;

use async_trait::async_trait;
use secrecy::{ExposeSecret as _, SecretString};
use serde_json::{Value, json};
use silicon_dm::{
    AppError, AppResult,
    application::{
        auth::{ApplicationSession, AuthContext, PresentedCredential},
        ports::{AuthenticationRequest, IdentityProvider},
    },
    config::{DatabaseSettings, TingSettings},
    domain::{ActorId, ActorRef, ActorType, OrganizationId},
    infrastructure::{
        postgres::{PostgresStore, TingDeliveryContext},
        ting::TingSendAuthority,
        ting_auto_enrollment::{Enrollment, TingAutoEnrollment},
        ting_credentials::TingCredentialCache,
    },
};
use std::{
    collections::{BTreeSet, HashMap},
    error::Error,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use time::OffsetDateTime;
use uuid::Uuid;

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

const SEND: &str = "obo:ting:tings.send";
const REGISTER: &str = "obo:ting:subscriptions.register";
const IDENTITY: &str = "self.identity.read";

#[derive(Default)]
struct Identity {
    tokens: Mutex<HashMap<String, AuthContext>>,
    registrations: Mutex<Vec<ActorRef>>,
    reject: AtomicBool,
}

impl Identity {
    fn register_token(&self, context: &AuthContext) -> TestResult {
        let PresentedCredential::Bearer(token) = &context.credential;
        self.tokens
            .lock()
            .map_err(|_| "token fixture lock")?
            .insert(token.expose_secret().to_owned(), context.clone());
        Ok(())
    }

    fn registrations(&self) -> Vec<ActorRef> {
        self.registrations
            .lock()
            .map_or_else(|_| Vec::new(), |registrations| registrations.clone())
    }
}

#[async_trait]
impl IdentityProvider for Identity {
    async fn authenticate(&self, request: AuthenticationRequest<'_>) -> AppResult<AuthContext> {
        let AuthenticationRequest::Bearer {
            token,
            organization_id,
        } = request;
        let context = self
            .tokens
            .lock()
            .map_err(|_| AppError::Unauthorized)?
            .get(token.expose_secret())
            .cloned()
            .ok_or(AppError::Unauthorized)?;
        if context.organization_id != *organization_id {
            return Err(AppError::Forbidden);
        }
        Ok(context)
    }
    async fn register_ting_delivery(
        &self,
        context: &AuthContext,
        _: &TingSettings,
        _: &str,
    ) -> AppResult<Value> {
        // Hold the attempt open so concurrent first contacts overlap.
        tokio::time::sleep(Duration::from_millis(50)).await;
        if self.reject.load(Ordering::SeqCst) {
            return Err(AppError::DependencyUnavailable { dependency: "ting" });
        }
        let mut registrations = self.registrations.lock().map_err(|_| AppError::Forbidden)?;
        registrations.push(context.actor.clone());
        Ok(
            json!({"id":format!("sub_{}",registrations.len()),"app_id":"dm","for":context.actor.id,"active":true}),
        )
    }
    async fn issue_ting_send_proof(
        &self,
        _: &AuthContext,
        _: &str,
        _: &str,
    ) -> AppResult<TingSendAuthority> {
        Err(AppError::Forbidden)
    }
    async fn login(&self, _: &SecretString, _: &str) -> AppResult<ApplicationSession> {
        Err(AppError::Forbidden)
    }
    async fn refresh(&self, _: &SecretString, _: &str) -> AppResult<ApplicationSession> {
        Err(AppError::Forbidden)
    }
    async fn logout(&self, _: &SecretString, _: &str) -> AppResult<()> {
        Err(AppError::Forbidden)
    }
    fn verify_webhook(
        &self,
        _: &http::HeaderMap,
        _: &[u8],
    ) -> AppResult<silicon_iam_client::models::WebhookEvent> {
        Err(AppError::Unauthorized)
    }
    async fn authorize_participants(
        &self,
        _: &AuthContext,
        _: &[ActorId],
    ) -> AppResult<Vec<ActorRef>> {
        Err(AppError::Forbidden)
    }
    async fn authorize_presence(&self, _: &AuthContext, _: &ActorId) -> AppResult<ActorRef> {
        Err(AppError::Forbidden)
    }
}

fn session(org: &OrganizationId, actor: &ActorRef, token: &str, scopes: &[&str]) -> AuthContext {
    AuthContext {
        actor: actor.clone(),
        session_id: Some(Uuid::new_v4()),
        organization_id: org.clone(),
        org_role: None,
        tag_ids: None,
        represented_actor_ids: BTreeSet::from([actor.id.clone()]),
        capabilities: scopes.iter().map(|scope| (*scope).to_owned()).collect(),
        credential: PresentedCredential::Bearer(SecretString::from(token.to_owned())),
        credential_expires_at: OffsetDateTime::now_utc() + time::Duration::hours(1),
    }
}

fn actor(kind: ActorType, id: &str) -> TestResult<ActorRef> {
    Ok(ActorRef {
        actor_type: kind,
        id: id.parse()?,
    })
}

async fn enrollment_row(
    store: &PostgresStore,
    org: &OrganizationId,
    actor: &ActorRef,
) -> TestResult<Option<(Option<String>, Option<String>)>> {
    Ok(sqlx::query_as(
        "SELECT subscription_id,last_error FROM dm.ting_automatic_enrollments \
         WHERE app_id='dm' AND generation=0 AND organization_id=$1 AND actor_kind=$2::text::dm.actor_kind AND actor_id=$3",
    )
    .bind(org.as_str())
    .bind(actor.actor_type.as_str())
    .bind(actor.id.as_str())
    .fetch_optional(store.pool())
    .await?)
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one fresh database follows first contact, cooldown, re-enrollment and backfill"
)]
async fn members_are_enrolled_with_ting_on_contact_and_by_backfill() -> TestResult {
    let fixture = support::TestDatabase::start().await?;
    let store = PostgresStore::connect(&DatabaseSettings {
        url: SecretString::from(fixture.url.clone()),
        max_connections: 8.try_into()?,
        min_connections: 1,
        acquire_timeout: Duration::from_secs(10),
        statement_timeout: Duration::from_secs(30),
    })
    .await?;
    store.migrate().await?;
    let context = TingDeliveryContext {
        app_id: "dm".into(),
        testing_environment_id: None,
        testing_generation: None,
    };
    let identity = Arc::new(Identity::default());
    let enrollment = TingAutoEnrollment::new(
        store.clone(),
        identity.clone(),
        TingSettings {
            base_url: "http://127.0.0.1:9".parse()?,
            request_timeout: Duration::from_secs(1),
        },
        context.clone(),
    );
    let org: OrganizationId = "auto-enroll".parse()?;
    let saket = actor(ActorType::Carbon, "c:saket")?;
    let chef = actor(ActorType::Silicon, "chef")?;
    let shubham = actor(ActorType::Carbon, "c:shubham")?;
    let legacy = actor(ActorType::Carbon, "c:legacy")?;

    // First contact enrolls once; concurrent requests share one registration.
    let first = session(&org, &saket, "saket-1", &[IDENTITY, REGISTER]);
    let (a, b) = tokio::join!(enrollment.ensure(&first), enrollment.ensure(&first));
    assert!(
        [a?, b?].contains(&Enrollment::Enrolled),
        "one concurrent contact must complete enrollment"
    );
    assert_eq!(identity.registrations(), vec![saket.clone()]);
    assert_eq!(enrollment.ensure(&first).await?, Enrollment::Enrolled);
    assert_eq!(
        identity.registrations().len(),
        1,
        "enrolled members are not registered again"
    );
    assert_eq!(
        enrollment_row(&store, &org, &saket).await?,
        Some((Some("sub_1".into()), None))
    );

    // A session without the registration scope cannot prove enrollment.
    let unscoped = session(&org, &chef, "chef-1", &[IDENTITY, SEND]);
    assert_eq!(
        enrollment.ensure(&unscoped).await?,
        Enrollment::MissingScope
    );
    assert_eq!(enrollment_row(&store, &org, &chef).await?, None);

    // Failures are recorded and wait out the cooldown instead of retrying per request.
    identity.reject.store(true, Ordering::SeqCst);
    let scoped_chef = session(&org, &chef, "chef-2", &[IDENTITY, REGISTER]);
    assert_eq!(enrollment.ensure(&scoped_chef).await?, Enrollment::Deferred);
    assert_eq!(
        enrollment_row(&store, &org, &chef).await?,
        Some((None, Some("dependency_unavailable".into())))
    );
    identity.reject.store(false, Ordering::SeqCst);
    assert_eq!(enrollment.ensure(&scoped_chef).await?, Enrollment::Deferred);
    assert_eq!(
        identity.registrations().len(),
        1,
        "cooldown suppresses immediate retries"
    );
    sqlx::query(
        "UPDATE dm.ting_automatic_enrollments SET attempted_at=attempted_at-interval '10 minutes'",
    )
    .execute(store.pool())
    .await?;
    assert_eq!(enrollment.ensure(&scoped_chef).await?, Enrollment::Enrolled);
    assert_eq!(identity.registrations().last(), Some(&chef));

    // Ting reporting the grant missing makes the next session enroll again.
    enrollment.forget(&org, &saket).await?;
    assert_eq!(enrollment_row(&store, &org, &saket).await?, None);
    assert_eq!(enrollment.ensure(&first).await?, Enrollment::Enrolled);
    assert_eq!(identity.registrations().last(), Some(&saket));

    // Backfill: members with a cached live session are enrolled by the sweep.
    store
        .refresh_directory(&org, &[shubham.clone(), legacy.clone()])
        .await?;
    let credentials = TingCredentialCache::new(
        store.clone(),
        &SecretString::from("fixture-encryption-material".to_owned()),
        context,
    )?;
    let cached = session(&org, &shubham, "shubham-1", &[IDENTITY, SEND, REGISTER]);
    identity.register_token(&cached)?;
    assert!(credentials.remember(&cached).await?);
    // A cached token predating the registration scope cannot enroll its member.
    let old = session(&org, &legacy, "legacy-1", &[IDENTITY, SEND]);
    identity.register_token(&old)?;
    assert!(credentials.remember(&old).await?);
    let before = identity.registrations().len();
    assert_eq!(enrollment.sweep(&credentials).await?, 1);
    assert_eq!(identity.registrations().len(), before + 1);
    assert_eq!(identity.registrations().last(), Some(&shubham));
    assert_eq!(
        enrollment_row(&store, &org, &legacy).await?,
        Some((None, Some("ting_registration_scope_missing".into()))),
        "unenrollable members wait out the cooldown instead of filling every sweep"
    );
    assert_eq!(
        enrollment.sweep(&credentials).await?,
        0,
        "a second sweep is a no-op"
    );
    assert_eq!(identity.registrations().len(), before + 1);
    Ok(())
}
