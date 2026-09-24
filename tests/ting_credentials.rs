//! Encrypted originator access-token cache proof against isolated PostgreSQL.

mod support;

use futures::future::try_join_all;
use secrecy::{ExposeSecret as _, SecretString};
use silicon_dm::{
    application::{
        auth::{AuthContext, PresentedCredential},
        commands::{CreateConversationCommand, SendMessageCommand},
    },
    config::DatabaseSettings,
    domain::{ActorRef, ActorType, MessageCreate, OrganizationId},
    infrastructure::{
        postgres::{PostgresStore, TingDeliveryContext},
        ting_credentials::TingCredentialCache,
    },
};
use std::{collections::BTreeSet, error::Error, time::Duration};
use time::OffsetDateTime;
use tokio::time::timeout;
use uuid::Uuid;

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

fn context() -> TingDeliveryContext {
    TingDeliveryContext {
        app_id: "dm".into(),
        testing_environment_id: None,
        testing_generation: None,
    }
}

fn authority(org: &OrganizationId, actor: &ActorRef, token: &str) -> AuthContext {
    AuthContext {
        actor: actor.clone(),
        session_id: Some(Uuid::new_v4()),
        organization_id: org.clone(),
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
    reason = "one fresh database follows encryption, bounded candidates, invalidation and restart"
)]
async fn verified_access_tokens_are_encrypted_bounded_scoped_and_restart_safe() -> TestResult {
    let fixture = support::TestDatabase::start().await?;
    let settings = DatabaseSettings {
        url: SecretString::from(fixture.url.clone()),
        max_connections: 8.try_into()?,
        min_connections: 1,
        acquire_timeout: Duration::from_secs(10),
        statement_timeout: Duration::from_secs(30),
    };
    let store = PostgresStore::connect(&settings).await?;
    store.migrate().await?;
    let org: OrganizationId = "tos".parse()?;
    let other_org: OrganizationId = "other".parse()?;
    let alice = ActorRef {
        actor_type: ActorType::Carbon,
        id: "c:alice".parse()?,
    };
    let bob = ActorRef {
        actor_type: ActorType::Carbon,
        id: "c:bob".parse()?,
    };
    let typed_alice = ActorRef {
        actor_type: ActorType::Silicon,
        id: alice.id.clone(),
    };
    store
        .refresh_directory(&org, &[alice.clone(), bob.clone(), typed_alice.clone()])
        .await?;
    store
        .refresh_directory(&other_org, std::slice::from_ref(&alice))
        .await?;
    let key = SecretString::from("fixture-app-secret-do-not-use-outside-tests");
    let cache = TingCredentialCache::new(store.clone(), &key, context())?;
    let original_token = "oat-fixture-sensitive-alice-original";
    let auth = authority(&org, &alice, original_token);
    assert!(cache.remember(&auth).await?);
    let original = cache
        .candidates(&org, &alice)
        .await?
        .pop()
        .ok_or("candidate missing")?;
    assert_eq!(original.token.expose_secret(), original_token);
    assert_eq!(original.session_id, auth.session_id);
    assert!(!format!("{original:?}").contains(original_token));
    let (cipher, nonce): (Vec<u8>, Vec<u8>) =
        sqlx::query_as("SELECT ciphertext,nonce FROM ting_credentials WHERE actor_id='c:alice'")
            .fetch_one(store.pool())
            .await?;
    assert_ne!(cipher, original_token.as_bytes());
    assert!(
        !cipher
            .windows(original_token.len())
            .any(|bytes| bytes == original_token.as_bytes())
    );
    assert_eq!(nonce.len(), 12);
    assert!(cache.candidates(&org, &typed_alice).await?.is_empty());
    assert!(cache.candidates(&other_org, &alice).await?.is_empty());

    let restarted_store = PostgresStore::connect(&settings).await?;
    let restarted = TingCredentialCache::new(restarted_store.clone(), &key, context())?;
    assert_eq!(
        restarted.candidates(&org, &alice).await?[0]
            .token
            .expose_secret(),
        original_token
    );
    let mut repeated = auth.clone();
    repeated.credential_expires_at += time::Duration::hours(8);
    assert!(restarted.remember(&repeated).await?);
    assert_eq!(
        restarted.candidates(&org, &alice).await?[0].expires_at,
        original.expires_at,
        "cache use cannot extend absolute expiry"
    );

    let mut missing_scope = authority(&org, &bob, "oat-fixture-missing-scope");
    missing_scope.capabilities.remove("obo:ting:tings.send");
    assert!(!cache.remember(&missing_scope).await?);
    let mut expired = authority(&org, &bob, "oat-fixture-expired");
    expired.credential_expires_at = OffsetDateTime::now_utc() - time::Duration::seconds(1);
    assert!(!cache.remember(&expired).await?);
    assert!(cache.candidates(&org, &bob).await?.is_empty());

    let many: Vec<_> = (0..16)
        .map(|index| authority(&org, &alice, &format!("oat-fixture-concurrent-{index}")))
        .collect();
    try_join_all(many.iter().map(|auth| cache.remember(auth))).await?;
    assert_eq!(
        cache.candidates(&org, &alice).await?.len(),
        8,
        "concurrent writes retain a bounded cache"
    );
    let latest = authority(&org, &alice, "oat-fixture-freshest");
    cache.remember(&latest).await?;
    let candidates = cache.candidates(&org, &alice).await?;
    assert_eq!(candidates.len(), 8);
    assert_eq!(candidates[0].token.expose_secret(), "oat-fixture-freshest");
    let newest_digest = candidates[0].digest;
    let older_digest = candidates[1].digest;
    cache.forget(&org, &bob, &newest_digest).await?;
    cache.forget(&other_org, &alice, &newest_digest).await?;
    assert_eq!(cache.candidates(&org, &alice).await?.len(), 8);
    cache.forget(&org, &alice, &older_digest).await?;
    let remaining = cache.candidates(&org, &alice).await?;
    assert_eq!(remaining.len(), 7);
    assert!(
        remaining
            .iter()
            .any(|candidate| candidate.digest == newest_digest)
    );
    assert!(
        remaining
            .iter()
            .all(|candidate| candidate.digest != older_digest)
    );

    // A later verified scope loss removes only the exact token it concerns.
    let mut revoked = latest.clone();
    revoked.capabilities.remove("self.identity.read");
    assert!(!cache.remember(&revoked).await?);
    assert!(
        cache
            .candidates(&org, &alice)
            .await?
            .iter()
            .all(|candidate| candidate.digest != newest_digest)
    );
    assert_transplant_and_expiry_guards(&store, &cache, &key, &org, &other_org, &bob, &typed_alice)
        .await?;
    assert_context_fences(&settings, &store, &key, &org, &alice).await?;
    assert_originator_wakeup(&store, &cache, &org, &alice, &bob).await?;
    restarted_store.pool().close().await;
    store.pool().close().await;
    Ok(())
}

#[allow(
    clippy::too_many_arguments,
    reason = "the shared fixture identities make cross-scope ciphertext substitution explicit"
)]
async fn assert_transplant_and_expiry_guards(
    store: &PostgresStore,
    cache: &TingCredentialCache,
    key: &SecretString,
    org: &OrganizationId,
    other_org: &OrganizationId,
    bob: &ActorRef,
    typed_alice: &ActorRef,
) -> TestResult {
    let token = "oat-fixture-bob-aad-sensitive";
    let auth = authority(org, bob, token);
    cache.remember(&auth).await?;
    let digest = cache.candidates(org, bob).await?[0].digest;
    // Reuse valid ciphertext under another typed identity and organization.
    sqlx::query("INSERT INTO ting_credentials(app_id,generation,organization_id,actor_kind,actor_id,token_digest,nonce,ciphertext,session_id,expires_at) SELECT app_id,generation,organization_id,'silicon',$1,token_digest,nonce,ciphertext,session_id,expires_at FROM ting_credentials WHERE token_digest=$2")
        .bind(typed_alice.id.as_str()).bind(digest.as_slice()).execute(store.pool()).await?;
    assert!(cache.candidates(org, typed_alice).await?.is_empty());
    sqlx::query("INSERT INTO ting_credentials(app_id,generation,organization_id,actor_kind,actor_id,token_digest,nonce,ciphertext,session_id,expires_at) SELECT app_id,generation,$1,'carbon','c:alice',token_digest,nonce,ciphertext,session_id,expires_at FROM ting_credentials WHERE organization_id=$2 AND actor_id=$3 AND token_digest=$4")
        .bind(other_org.as_str()).bind(org.as_str()).bind(bob.id.as_str()).bind(digest.as_slice()).execute(store.pool()).await?;
    let alice = ActorRef {
        actor_type: ActorType::Carbon,
        id: "c:alice".parse()?,
    };
    assert!(cache.candidates(other_org, &alice).await?.is_empty());
    sqlx::query("UPDATE ting_credentials SET session_id=$2 WHERE organization_id=$3 AND actor_id=$4 AND token_digest=$1")
        .bind(digest.as_slice()).bind(Uuid::new_v4()).bind(org.as_str()).bind(bob.id.as_str()).execute(store.pool()).await?;
    assert!(
        cache.candidates(org, bob).await?.is_empty(),
        "session metadata is authenticated"
    );
    cache.remember(&auth).await?;
    sqlx::query("UPDATE ting_credentials SET expires_at=expires_at+interval '1 hour' WHERE organization_id=$2 AND actor_id=$3 AND token_digest=$1")
        .bind(digest.as_slice()).bind(org.as_str()).bind(bob.id.as_str()).execute(store.pool()).await?;
    assert!(
        cache.candidates(org, bob).await?.is_empty(),
        "expiry metadata is authenticated"
    );
    cache.remember(&auth).await?;

    let rotated_key = SecretString::from("different-fixture-app-secret");
    let rotated = TingCredentialCache::new(store.clone(), &rotated_key, context())?;
    assert!(
        rotated.candidates(org, bob).await?.is_empty(),
        "wrong key fails closed"
    );
    rotated.remember(&auth).await?;
    assert_eq!(
        rotated.candidates(org, bob).await?[0].token.expose_secret(),
        token,
        "fresh verified authority repairs an unreadable cache row"
    );
    assert!(cache.candidates(org, bob).await?.is_empty());
    let restored = TingCredentialCache::new(store.clone(), key, context())?;
    restored.remember(&auth).await?;
    sqlx::query("UPDATE ting_credentials SET expires_at=clock_timestamp()-interval '1 second' WHERE actor_id=$1")
        .bind(bob.id.as_str()).execute(store.pool()).await?;
    assert!(cache.candidates(org, bob).await?.is_empty());
    let retained: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ting_credentials WHERE actor_id=$1")
            .bind(bob.id.as_str())
            .fetch_one(store.pool())
            .await?;
    assert_eq!(retained, 0, "expired ciphertext is pruned");
    Ok(())
}

async fn assert_context_fences(
    settings: &DatabaseSettings,
    production: &PostgresStore,
    key: &SecretString,
    org: &OrganizationId,
    actor: &ActorRef,
) -> TestResult {
    let environment = Uuid::new_v4();
    let schema = format!("dm_test_{}", environment.simple());
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}; CREATE TYPE {schema}.actor_kind AS ENUM ('carbon','silicon'); CREATE TABLE {schema}.ting_credentials (LIKE dm.ting_credentials INCLUDING ALL); ALTER TABLE {schema}.ting_credentials ALTER COLUMN actor_kind TYPE {schema}.actor_kind USING actor_kind::text::{schema}.actor_kind; CREATE TABLE {schema}.ting_handoffs (LIKE dm.ting_handoffs INCLUDING ALL); ALTER TABLE {schema}.ting_handoffs ALTER COLUMN target_kind TYPE {schema}.actor_kind USING target_kind::text::{schema}.actor_kind, ALTER COLUMN originator_kind TYPE {schema}.actor_kind USING originator_kind::text::{schema}.actor_kind")))
        .execute(production.pool()).await?;
    let scoped = PostgresStore::connect_schema(settings, &schema).await?;
    let mut selected = context();
    selected.testing_environment_id = Some(environment);
    selected.testing_generation = Some(1);
    let wrong_plane = TingCredentialCache::new(production.clone(), key, selected.clone())?;
    assert!(wrong_plane.candidates(org, actor).await.is_err());
    let first = TingCredentialCache::new(scoped.clone(), key, selected.clone())?;
    let auth = authority(org, actor, "oat-fixture-sandbox-1");
    first.remember(&auth).await?;
    assert_eq!(first.candidates(org, actor).await?.len(), 1);
    selected.testing_generation = Some(2);
    let second = TingCredentialCache::new(scoped.clone(), key, selected)?;
    assert!(second.candidates(org, actor).await?.is_empty());
    sqlx::query("INSERT INTO ting_credentials(app_id,generation,organization_id,actor_kind,actor_id,token_digest,nonce,ciphertext,session_id,expires_at) SELECT app_id,2,organization_id,actor_kind,actor_id,token_digest,nonce,ciphertext,session_id,expires_at FROM ting_credentials WHERE generation=1")
        .execute(scoped.pool()).await?;
    assert!(
        second.candidates(org, actor).await?.is_empty(),
        "generation ciphertext transplant must fail authentication"
    );
    assert_eq!(first.candidates(org, actor).await?.len(), 1);
    second.remember(&auth).await?;
    assert_eq!(second.candidates(org, actor).await?.len(), 1);
    let prod = TingCredentialCache::new(production.clone(), key, context())?;
    assert!(
        prod.candidates(org, actor)
            .await?
            .iter()
            .all(|candidate| candidate.token.expose_secret() != "oat-fixture-sandbox-1")
    );
    scoped.pool().close().await;
    Ok(())
}

async fn assert_originator_wakeup(
    store: &PostgresStore,
    cache: &TingCredentialCache,
    org: &OrganizationId,
    alice: &ActorRef,
    bob: &ActorRef,
) -> TestResult {
    let chat = store
        .create_conversation(CreateConversationCommand {
            organization_id: org.clone(),
            creator: alice.clone(),
            participants: vec![alice.clone(), bob.clone()],
            idempotency_key: "credential-wake-chat".parse()?,
        })
        .await?;
    for (sender, key) in [
        (alice, "credential-wake-alice"),
        (bob, "credential-wake-bob"),
    ] {
        store
            .send_message(SendMessageCommand {
                organization_id: org.clone(),
                conversation_id: chat.id,
                sender: sender.clone(),
                content: MessageCreate {
                    text: Some("wake owned delivery".into()),
                    ..MessageCreate::default()
                },
                idempotency_key: key.parse()?,
            })
            .await?;
    }
    sqlx::query("UPDATE ting_handoffs SET next_attempt_at=clock_timestamp()+interval '1 day' WHERE accepted_at IS NULL")
        .execute(store.pool()).await?;
    let mut listener = store
        .subscribe_delivery_wakeups()
        .await?
        .ok_or("listener unavailable")?;
    let auth = authority(org, alice, "oat-fixture-wake-alice");
    cache.remember(&auth).await?;
    let notice = timeout(Duration::from_secs(2), listener.recv()).await??;
    assert_eq!(notice.channel(), "dm_delivery");
    assert!(!notice.payload().contains("oat-fixture"));
    let due:Vec<String>=sqlx::query_scalar("SELECT originator_id FROM ting_handoffs WHERE next_attempt_at<=clock_timestamp() ORDER BY delivery_id")
        .fetch_all(store.pool()).await?;
    assert_eq!(
        due,
        vec![alice.id.to_string(), alice.id.to_string()],
        "recipient credentials cannot wake another originator's work"
    );
    Ok(())
}
