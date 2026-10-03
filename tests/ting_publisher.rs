//! Originator authority selection with real PostgreSQL and a local Ting socket fixture.

mod support;

use async_trait::async_trait;
use secrecy::{ExposeSecret as _, SecretString};

use silicon_dm::{
    AppError, AppResult,
    application::{
        auth::{ApplicationSession, AuthContext, PresentedCredential},
        commands::{CreateConversationCommand, SendMessageCommand},
        ports::{AuthenticationRequest, IdentityProvider},
    },
    config::{DatabaseSettings, TingSettings},
    domain::{ActorId, ActorRef, ActorType, MessageCreate, OrganizationId},
    infrastructure::{
        postgres::{PostgresStore, TingDeliveryContext},
        ting::{TingFailure, TingPublisher, TingSendAuthority, TingSocket},
        ting_credentials::TingCredentialCache,
        ting_publisher::AuthenticatedTingPublisher,
    },
};
use std::{
    collections::{BTreeSet, HashMap},
    error::Error,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, AtomicUsize, Ordering},
    },
    time::Duration,
};
use time::OffsetDateTime;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

#[derive(Default)]
struct Identity {
    tokens: Mutex<HashMap<String, AuthContext>>,
    authenticated: Mutex<Vec<String>>,
    proofs: Mutex<Vec<(String, String, ActorRef)>>,
    mode: AtomicU8,
    refresh_calls: AtomicUsize,
}

impl Identity {
    fn register(&self, context: AuthContext) -> TestResult {
        let PresentedCredential::Bearer(token) = &context.credential;
        self.tokens
            .lock()
            .map_err(|_| "token fixture lock")?
            .insert(token.expose_secret().to_owned(), context);
        Ok(())
    }
}

#[async_trait]
impl IdentityProvider for Identity {
    async fn authenticate(&self, request: AuthenticationRequest<'_>) -> AppResult<AuthContext> {
        let AuthenticationRequest::Bearer {
            token,
            organization_id,
        } = request;
        self.authenticated
            .lock()
            .map_err(|_| AppError::Unauthorized)?
            .push(token.expose_secret().to_owned());
        match self.mode.load(Ordering::SeqCst) {
            1 => return Err(AppError::DependencyUnavailable { dependency: "iam" }),
            2 => return Err(AppError::Unauthorized),
            _ => {}
        }
        let mut context = self
            .tokens
            .lock()
            .map_err(|_| AppError::Unauthorized)?
            .get(token.expose_secret())
            .cloned()
            .ok_or(AppError::Unauthorized)?;
        if context.organization_id != *organization_id {
            return Err(AppError::Forbidden);
        }
        match self.mode.load(Ordering::SeqCst) {
            3 => context.actor.id = "c:bob".parse().map_err(|_| AppError::Unauthorized)?,
            4 => {
                context.organization_id =
                    "wrong-org".parse().map_err(|_| AppError::Unauthorized)?;
            }
            _ => {}
        }
        Ok(context)
    }
    async fn issue_ting_send_proof(
        &self,
        context: &AuthContext,
        body: &str,
        attempt: &str,
    ) -> AppResult<TingSendAuthority> {
        if !context.has_capability("self.identity.read")
            || !context.has_capability("obo:ting:tings.send")
        {
            return Err(AppError::Forbidden);
        }
        let mut proofs = self.proofs.lock().map_err(|_| AppError::Unauthorized)?;
        proofs.push((body.to_owned(), attempt.to_owned(), context.actor.clone()));
        Ok(TingSendAuthority {
            proof_token: SecretString::from(format!("local-fixture-proof-{}", proofs.len())),
            testing: None,
        })
    }
    async fn login(&self, _: &SecretString, _: &str) -> AppResult<ApplicationSession> {
        Err(AppError::Forbidden)
    }
    async fn refresh(&self, _: &SecretString, _: &str) -> AppResult<ApplicationSession> {
        self.refresh_calls.fetch_add(1, Ordering::SeqCst);
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

fn auth(org: &OrganizationId, actor: &ActorRef, token: &str) -> AuthContext {
    AuthContext {
        organization_id: org.clone(),
        actor: actor.clone(),
        session_id: Some(Uuid::new_v4()),
        org_role: None,
        tag_ids: None,
        represented_actor_ids: BTreeSet::from([actor.id.clone()]),
        capabilities: BTreeSet::from(["self.identity.read".into(), "obo:ting:tings.send".into()]),
        credential: PresentedCredential::Bearer(SecretString::from(token.to_owned())),
        credential_expires_at: OffsetDateTime::now_utc() + time::Duration::hours(1),
    }
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one fixture follows immutable originator authority through outages, revocation and ambiguous sends"
)]
async fn legacy_login_and_recipient_tokens_never_become_delegated_send_authority() -> TestResult {
    let fixture = support::TestDatabase::start().await?;
    let database = DatabaseSettings {
        url: SecretString::from(fixture.url.clone()),
        max_connections: 6.try_into()?,
        min_connections: 1,
        acquire_timeout: Duration::from_secs(5),
        statement_timeout: Duration::from_secs(30),
    };
    let store = PostgresStore::connect(&database).await?;
    store.migrate().await?;
    let org: OrganizationId = "tos".parse()?;
    let alice = ActorRef {
        actor_type: ActorType::Carbon,
        id: "c:alice".parse()?,
    };
    let bob = ActorRef {
        actor_type: ActorType::Carbon,
        id: "c:bob".parse()?,
    };
    let chat = store
        .create_conversation(CreateConversationCommand {
            organization_id: org.clone(),
            creator: alice.clone(),
            participants: vec![alice.clone(), bob.clone()],
            idempotency_key: "publisher-chat".parse()?,
        })
        .await?;
    store
        .send_message(SendMessageCommand {
            organization_id: org.clone(),
            conversation_id: chat.id,
            sender: alice.clone(),
            content: MessageCreate {
                text: Some("originator ownership".into()),
                ..MessageCreate::default()
            },
            idempotency_key: "publisher-message".parse()?,
        })
        .await?;
    let context = TingDeliveryContext {
        app_id: "dm".into(),
        testing_environment_id: None,
        testing_generation: None,
    };
    let claim = store
        .claim_ting_deliveries("publisher-fixture", 2, Duration::from_secs(60), &context)
        .await?
        .into_iter()
        .find(|claim| claim.target == bob)
        .ok_or("recipient handoff missing")?;
    assert_eq!(claim.originator.as_ref(), Some(&alice));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let settings = TingSettings {
        base_url: format!("http://{}", listener.local_addr()?).parse()?,
        request_timeout: Duration::from_secs(2),
    };
    let cancellation = CancellationToken::new();
    let socket = TingSocket::start(&settings, cancellation.clone());
    let key = SecretString::from("publisher-cache-fixture-app-secret");
    let cache = TingCredentialCache::new(store.clone(), &key, context.clone())?;
    let identity = Arc::new(Identity::default());
    let publisher = AuthenticatedTingPublisher::new(
        TingCredentialCache::new(store.clone(), &key, context)?,
        identity.clone(),
        socket,
    );

    let recipient_auth = auth(&org, &bob, "oat-recipient-must-not-be-borrowed");
    identity.register(recipient_auth.clone())?;
    cache.remember(&recipient_auth).await?;
    let mut unattributed = claim.clone();
    unattributed.originator = None;
    assert!(matches!(
        publisher.publish(&unattributed).await,
        Err(TingFailure::OriginatorAuthenticationRequired)
    ));
    assert!(matches!(
        publisher.publish(&claim).await,
        Err(TingFailure::OriginatorAuthenticationRequired)
    ));
    assert!(
        identity
            .authenticated
            .lock()
            .map_err(|_| "auth lock")?
            .is_empty()
    );

    let fresh = auth(&org, &alice, "oat-originator-with-legacy-consent");
    identity.register(fresh.clone())?;
    cache.remember(&fresh).await?;
    assert!(matches!(
        publisher.publish(&claim).await,
        Err(TingFailure::OriginatorAuthenticationRequired)
    ));
    assert!(
        identity
            .authenticated
            .lock()
            .map_err(|_| "auth lock")?
            .is_empty()
    );
    assert!(identity.proofs.lock().map_err(|_| "proof lock")?.is_empty());
    assert_eq!(identity.refresh_calls.load(Ordering::SeqCst), 0);
    cancellation.cancel();
    Ok(())
}
