//! A migration hold preserves exact signed bytes without claiming delivery.
use secrecy::SecretString;
use silicon_dm::{
    application::commands::{CreateConversationCommand, SendMessageCommand},
    config::DatabaseSettings,
    domain::{ActorRef, ActorType, MessageCreate, OrganizationId},
    infrastructure::postgres::PostgresStore,
};
use std::{num::NonZeroU32, time::Duration};
use testcontainers::{ImageExt as _, runners::AsyncRunner as _};
use testcontainers_modules::postgres::Postgres;

type Result = std::result::Result<(), Box<dyn std::error::Error + Send + Sync>>;
const HOLD: &str = include_str!("../scripts/hold-legacy-ting.sql");

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one fixture proves transfer, fences, replay and migration preservation"
)]
async fn held_handoffs_preserve_exact_rows_and_fail_closed() -> Result {
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
    let alice = ActorRef {
        actor_type: ActorType::Carbon,
        id: "alice".parse()?,
    };
    let bob = ActorRef {
        actor_type: ActorType::Carbon,
        id: "bobby".parse()?,
    };
    store
        .refresh_directory(&org, &[alice.clone(), bob.clone()])
        .await?;
    let conversation = store
        .create_conversation(CreateConversationCommand {
            organization_id: org.clone(),
            creator: alice.clone(),
            participants: vec![alice.clone(), bob],
            idempotency_key: "hold-chat".parse()?,
        })
        .await?;
    store
        .send_message(SendMessageCommand {
            organization_id: org,
            conversation_id: conversation.id,
            sender: alice,
            content: MessageCreate {
                text: Some("Retain the old tos>dm and alice content verbatim".into()),
                ..MessageCreate::default()
            },
            idempotency_key: "hold-message".parse()?,
        })
        .await?;
    let body = "{ \"type\":\"tos>dm.sync.changed\", \"key\":\"original-key\", \"proof\":\"original.signature.bytes\", \"for\":\"bobby\" }\n";
    sqlx::query("UPDATE dm.ting_handoffs SET request_body=$1")
        .bind(body)
        .execute(store.pool())
        .await?;
    let before: serde_json::Value = sqlx::query_scalar(
        "SELECT jsonb_agg(to_jsonb(h) ORDER BY delivery_id) FROM dm.ting_handoffs h",
    )
    .fetch_one(store.pool())
    .await?;
    let messages: serde_json::Value =
        sqlx::query_scalar("SELECT jsonb_agg(to_jsonb(m) ORDER BY id) FROM dm.messages m")
            .fetch_one(store.pool())
            .await?;
    let manifest: String = sqlx::query_scalar("SELECT 'INSERT INTO cutover_manifest VALUES ' || string_agg(format('(%L::uuid,decode(%L,''hex''))',delivery_id,encode(sha256(convert_to(request_body,'UTF8')),'hex')),',') || ';' FROM dm.ting_handoffs").fetch_one(store.pool()).await?;
    let setup = format!(
        "SET LOCAL dm.cutover.batch='test-hold'; SET LOCAL dm.cutover.backup='verified-fixture-backup'; CREATE TEMP TABLE cutover_manifest(delivery_id uuid PRIMARY KEY,body_sha256 bytea NOT NULL) ON COMMIT DROP; {manifest}"
    );
    for invalid in [
        "DELETE FROM cutover_manifest WHERE delivery_id=(SELECT min(delivery_id::text)::uuid FROM cutover_manifest);",
        "UPDATE cutover_manifest SET body_sha256=decode(repeat('00',32),'hex');",
        "INSERT INTO dm.ting_handoffs SELECT (jsonb_populate_record(NULL::dm.ting_handoffs,to_jsonb(h)||jsonb_build_object('delivery_id',gen_random_uuid(),'request_body',NULL))).* FROM dm.ting_handoffs h LIMIT 1;",
        "UPDATE dm.ting_handoffs SET lease_id=gen_random_uuid(),lease_owner='active-worker',lease_expires_at=clock_timestamp()+interval '1 hour';",
    ] {
        let mut tx = store.pool().begin().await?;
        sqlx::raw_sql(sqlx::AssertSqlSafe(setup.as_str()))
            .execute(&mut *tx)
            .await?;
        sqlx::raw_sql(invalid).execute(&mut *tx).await?;
        assert!(sqlx::raw_sql(HOLD).execute(&mut *tx).await.is_err());
        tx.rollback().await?;
    }
    // Preview does not retain either archive DDL or row transfers.
    let mut preview = store.pool().begin().await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("{setup}{HOLD}")))
        .execute(&mut *preview)
        .await?;
    preview.rollback().await?;
    let unchanged: serde_json::Value = sqlx::query_scalar(
        "SELECT jsonb_agg(to_jsonb(h) ORDER BY delivery_id) FROM dm.ting_handoffs h",
    )
    .fetch_one(store.pool())
    .await?;
    assert_eq!(before, unchanged);
    let mut tx = store.pool().begin().await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("{setup}{HOLD}")))
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    let held: serde_json::Value = sqlx::query_scalar(
        "SELECT jsonb_agg(original_row ORDER BY delivery_id) FROM dm_cutover_hold.ting_handoffs",
    )
    .fetch_one(store.pool())
    .await?;
    assert_eq!(before, held);
    let exact: bool = sqlx::query_scalar(
        "SELECT bool_and(request_bytes=$1 AND state='held') FROM dm_cutover_hold.ting_handoffs",
    )
    .bind(body.as_bytes())
    .fetch_one(store.pool())
    .await?;
    assert!(exact);
    let messages_after: serde_json::Value =
        sqlx::query_scalar("SELECT jsonb_agg(to_jsonb(m) ORDER BY id) FROM dm.messages m")
            .fetch_one(store.pool())
            .await?;
    assert_eq!(messages, messages_after);
    sqlx::raw_sql("CREATE ROLE hold_runtime;")
        .execute(store.pool())
        .await?;
    // No PUBLIC/default access to held original credentials and proofs.
    let allowed: bool =
        sqlx::query_scalar("SELECT has_schema_privilege('hold_runtime','dm_cutover_hold','USAGE')")
            .fetch_one(store.pool())
            .await?;
    assert!(!allowed);
    sqlx::raw_sql("RESET ROLE").execute(store.pool()).await?;
    store.migrate().await?;
    let after: serde_json::Value = sqlx::query_scalar(
        "SELECT jsonb_agg(original_row ORDER BY delivery_id) FROM dm_cutover_hold.ting_handoffs",
    )
    .fetch_one(store.pool())
    .await?;
    assert_eq!(
        before, after,
        "migration cannot transform held signatures or IDs"
    );
    let mut retry = store.pool().begin().await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("{setup}{HOLD}")))
        .execute(&mut *retry)
        .await?;
    retry.commit().await?;
    let active: i64 = sqlx::query_scalar("SELECT count(*) FROM dm.ting_handoffs")
        .fetch_one(store.pool())
        .await?;
    assert_eq!(
        active, 0,
        "held work must never enter automatic new-schema delivery"
    );
    Ok(())
}
