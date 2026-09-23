//! Public-ID cutover preserves conversation UUIDs, deduplication and replay bytes.
use secrecy::{ExposeSecret as _, SecretString};
use silicon_dm::{
    application::{
        auth::{AuthContext, PresentedCredential},
        commands::CreateConversationCommand,
    },
    config::DatabaseSettings,
    domain::{ActorRef, ActorType, OrganizationId},
    infrastructure::{
        postgres::{PostgresStore, TingDeliveryContext},
        ting_credentials::TingCredentialCache,
    },
};
use std::{collections::BTreeSet, num::NonZeroU32, time::Duration};
use testcontainers::{ImageExt as _, runners::AsyncRunner as _};
use testcontainers_modules::postgres::Postgres;
use uuid::Uuid;

type Result = std::result::Result<(), Box<dyn std::error::Error + Send + Sync>>;

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one migration fixture verifies UUID, replay, ciphertext and trigger preservation"
)]
async fn cutover_preserves_conversation_and_exact_replay_receipt() -> Result {
    let container = Postgres::default().with_tag("16-alpine").start().await?;
    let store = PostgresStore::connect(&DatabaseSettings {
        url: SecretString::from(format!(
            "postgres://postgres:postgres@{}:{}/postgres",
            container.get_host().await?,
            container.get_host_port_ipv4(5432).await?
        )),
        max_connections: NonZeroU32::new(4).ok_or("pool size")?,
        min_connections: 1,
        acquire_timeout: Duration::from_secs(10),
        statement_timeout: Duration::from_secs(30),
    })
    .await?;
    let mut connection = store.pool().acquire().await?;
    sqlx::query("SET search_path=public")
        .execute(&mut *connection)
        .await?;
    sqlx::migrate!("./migrations")
        .run_to(35, &mut *connection)
        .await?;
    sqlx::query("SET search_path=dm")
        .execute(&mut *connection)
        .await?;
    drop(connection);
    let org: OrganizationId = "tos".parse()?;
    let old = vec![
        ActorRef {
            actor_type: ActorType::Carbon,
            id: "alice".parse()?,
        },
        ActorRef {
            actor_type: ActorType::Silicon,
            id: "assistant:tos".parse()?,
        },
    ];
    store.refresh_directory(&org, &old).await?;
    // The pre-cutover writer used the same v1 encryption format. Add only the
    // reader compatibility columns temporarily to let this version seed it.
    sqlx::query(
        "ALTER TABLE ting_credentials ADD COLUMN aad_app_id text,ADD COLUMN aad_actor_id text",
    )
    .execute(store.pool())
    .await?;
    let secret = SecretString::from("fixture-app-secret");
    let old_cache = TingCredentialCache::new(
        store.clone(),
        &secret,
        TingDeliveryContext {
            app_id: "tos>dm".into(),
            testing_environment_id: None,
            testing_generation: None,
        },
    )?;
    let token = SecretString::from("oat_pre_cutover_credential");
    let auth = AuthContext {
        actor: old[0].clone(),
        session_id: Some(Uuid::new_v4()),
        organization_id: org.clone(),
        tag_ids: None,
        org_role: None,
        represented_actor_ids: BTreeSet::new(),
        capabilities: BTreeSet::from(["self.identity.read".into(), "obo:ting:tings.send".into()]),
        credential: PresentedCredential::Bearer(token.clone()),
        credential_expires_at: time::OffsetDateTime::now_utc() + time::Duration::hours(1),
    };
    assert!(old_cache.remember(&auth).await?);
    sqlx::query("ALTER TABLE ting_credentials DROP COLUMN aad_app_id,DROP COLUMN aad_actor_id")
        .execute(store.pool())
        .await?;
    let created = store
        .create_conversation(CreateConversationCommand {
            organization_id: org.clone(),
            creator: old[0].clone(),
            participants: old,
            idempotency_key: "before-id-schema".parse()?,
        })
        .await?;
    let receipt_before: serde_json::Value=sqlx::query_scalar("SELECT to_jsonb(r)-'actor_id' FROM idempotency_records r WHERE idempotency_key='before-id-schema'")
        .fetch_one(store.pool()).await?;
    sqlx::raw_sql("CREATE FUNCTION schema_trigger_probe() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$; CREATE TRIGGER schema_disabled AFTER UPDATE ON dm.actor_snapshots FOR EACH ROW EXECUTE FUNCTION schema_trigger_probe(); ALTER TABLE dm.actor_snapshots DISABLE TRIGGER schema_disabled; CREATE TRIGGER schema_replica AFTER UPDATE ON dm.actor_snapshots FOR EACH ROW EXECUTE FUNCTION schema_trigger_probe(); ALTER TABLE dm.actor_snapshots ENABLE REPLICA TRIGGER schema_replica; CREATE TRIGGER schema_always AFTER UPDATE ON dm.actor_snapshots FOR EACH ROW EXECUTE FUNCTION schema_trigger_probe(); ALTER TABLE dm.actor_snapshots ENABLE ALWAYS TRIGGER schema_always; ").execute(store.pool()).await?;
    store.migrate().await?;
    let current = vec![
        ActorRef {
            actor_type: ActorType::Carbon,
            id: "c:alice".parse()?,
        },
        ActorRef {
            actor_type: ActorType::Silicon,
            id: "si:assistant".parse()?,
        },
    ];
    let cache = TingCredentialCache::new(
        store.clone(),
        &secret,
        TingDeliveryContext {
            app_id: "dm".into(),
            testing_environment_id: None,
            testing_generation: None,
        },
    )?;
    let candidates = cache.candidates(&org, &current[0]).await?;
    assert_eq!(candidates.len(), 1);
    assert_eq!(
        candidates[0].token.expose_secret(),
        token.expose_secret(),
        "original AAD must still authenticate the retained token"
    );
    let intruder = ActorRef {
        actor_type: ActorType::Carbon,
        id: "c:intruder".parse()?,
    };
    store
        .refresh_directory(&org, std::slice::from_ref(&intruder))
        .await?;
    sqlx::query("INSERT INTO ting_credentials(app_id,generation,organization_id,actor_kind,actor_id,token_digest,nonce,ciphertext,session_id,expires_at,aad_app_id,aad_actor_id) SELECT app_id,generation,organization_id,actor_kind,'c:intruder',token_digest,nonce,ciphertext,session_id,expires_at,aad_app_id,aad_actor_id FROM ting_credentials WHERE actor_id='c:alice'")
        .execute(store.pool()).await?;
    assert!(
        cache.candidates(&org, &intruder).await?.is_empty(),
        "retained AAD cannot move ciphertext to a different actor"
    );
    let resolved = store
        .create_conversation(CreateConversationCommand {
            organization_id: org,
            creator: current[0].clone(),
            participants: current,
            idempotency_key: "after-id-schema".parse()?,
        })
        .await?;
    assert_eq!(
        created.id, resolved.id,
        "a public rename cannot split an existing conversation"
    );
    let receipt_after: serde_json::Value=sqlx::query_scalar("SELECT to_jsonb(r)-'actor_id' FROM idempotency_records r WHERE idempotency_key='before-id-schema'")
        .fetch_one(store.pool()).await?;
    assert_eq!(
        receipt_before, receipt_after,
        "request digest and exact retained result must not be rewritten"
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM conversations")
        .fetch_one(store.pool())
        .await?;
    assert_eq!(count, 1);
    let address: String =
        sqlx::query_scalar("SELECT public_id FROM conversation_addresses WHERE id=$1")
            .bind(created.id)
            .fetch_one(store.pool())
            .await?;
    assert_eq!(address, "c:alice::si:assistant");
    let modes: Vec<(String,String)> = sqlx::query_as("SELECT tgname,tgenabled::text FROM pg_trigger WHERE tgrelid='dm.actor_snapshots'::regclass AND tgname LIKE 'schema_%' ORDER BY tgname").fetch_all(store.pool()).await?;
    assert_eq!(
        modes,
        vec![
            ("schema_always".into(), "A".into()),
            ("schema_disabled".into(), "D".into()),
            ("schema_replica".into(), "R".into())
        ]
    );
    sqlx::raw_sql("DROP TRIGGER schema_disabled ON dm.actor_snapshots; DROP TRIGGER schema_replica ON dm.actor_snapshots; DROP TRIGGER schema_always ON dm.actor_snapshots; DROP FUNCTION schema_trigger_probe();").execute(store.pool()).await?;
    let triggers:i64=sqlx::query_scalar("SELECT count(*) FROM pg_trigger t JOIN pg_class c ON c.oid=t.tgrelid JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname IN('dm','dm_private') AND NOT t.tgisinternal AND t.tgenabled='D'")
        .fetch_one(store.pool()).await?;
    assert_eq!(triggers, 0);
    let _: Uuid = created.id;
    Ok(())
}
