//! Preserve deployed revision data while moving to message content history.
use secrecy::SecretString;
use serde_json::{Value, json};
use silicon_dm::{
    application::commands::{CreateConversationCommand, SendMessageCommand},
    config::DatabaseSettings,
    domain::{ActorRef, ActorType, MessageCreate, OrganizationId},
    infrastructure::postgres::PostgresStore,
};
use std::time::Duration;
use testcontainers::{ImageExt as _, runners::AsyncRunner as _};
use testcontainers_modules::postgres::Postgres;
use uuid::Uuid;
type Result = std::result::Result<(), Box<dyn std::error::Error + Send + Sync>>;
#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one upgrade fixture preserves accepted old revisions and deletion"
)]
async fn upgrade_preserves_edits_and_keeps_deletion_out_of_history() -> Result {
    let container = Postgres::default().with_tag("16-alpine").start().await?;
    let store = PostgresStore::connect(&DatabaseSettings {
        url: SecretString::from(format!(
            "postgres://postgres:postgres@{}:{}/postgres",
            container.get_host().await?,
            container.get_host_port_ipv4(5432).await?
        )),
        max_connections: 4.try_into()?,
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
        .run_to(24, &mut *connection)
        .await?;
    sqlx::query("SET search_path=dm")
        .execute(&mut *connection)
        .await?;
    drop(connection);
    // A read-only compatibility view lets current hydration read unedited legacy rows.
    sqlx::query("CREATE VIEW dm.message_history AS SELECT NULL::uuid AS message_id,'[]'::jsonb AS history,NULL::jsonb AS content,NULL::timestamptz AS updated_at,NULL::timestamptz AS deleted_at WHERE false").execute(store.pool()).await?;
    let org: OrganizationId = "tos".parse()?;
    let author = ActorRef {
        id: "c:alice".parse()?,
        actor_type: ActorType::Carbon,
    };
    let other = ActorRef {
        id: "si:cos".parse()?,
        actor_type: ActorType::Silicon,
    };
    let conversation = store
        .create_conversation(CreateConversationCommand {
            organization_id: org.clone(),
            creator: author.clone(),
            participants: vec![author.clone(), other],
            idempotency_key: "migration-chat".parse()?,
        })
        .await?;
    let content: MessageCreate = serde_json::from_value(json!({"message":"original"}))?;
    let message = store
        .send_message(SendMessageCommand {
            organization_id: org.clone(),
            conversation_id: conversation.id,
            sender: author.clone(),
            content,
            idempotency_key: "migration-message".parse()?,
        })
        .await?;
    // Insert the same revision and full outbox rows produced by the released backend.
    for (version, text) in [
        (2, Some("edited once")),
        (3, Some("edited twice")),
        (4, None),
    ] {
        let mut tx = store.pool().begin().await?;
        let content = text
            .map(|text| serde_json::from_value::<MessageCreate>(json!({"message":text})))
            .transpose()?;
        sqlx::query("INSERT INTO message_revisions(id,message_id,version,content,deleted_at) VALUES($1,$2,$3,$4,CASE WHEN $4::jsonb IS NULL THEN clock_timestamp() ELSE NULL END)").bind(Uuid::now_v7()).bind(message.id).bind(version).bind(content.map(sqlx::types::Json)).execute(&mut *tx).await?;
        for (kind, id) in [("carbon", "c:alice"), ("silicon", "si:cos")] {
            sqlx::query("INSERT INTO actor_deliveries(id,organization_id,target_kind,target_id,sequence,delivery_kind,conversation_id,message_id,delivery_revision) VALUES($1,'tos',$2::text::actor_kind,$3,1,'message',$4,$5,$6)").bind(Uuid::now_v7()).bind(kind).bind(id).bind(conversation.id).bind(message.id).bind(version).execute(&mut *tx).await?;
        }
        tx.commit().await?;
    }
    sqlx::query("DROP VIEW dm.message_history")
        .execute(store.pool())
        .await?;
    store.migrate().await?;
    let (content, history, deleted): (Value, Value, bool) = sqlx::query_as(
        "SELECT content,history,deleted_at IS NOT NULL FROM message_history WHERE message_id=$1",
    )
    .bind(message.id)
    .fetch_one(store.pool())
    .await?;
    assert!(deleted);
    assert_eq!(history.as_array().ok_or("history")?.len(), 2);
    assert!(history[0]["content"].is_null());
    assert_eq!(history[1]["content"]["message"], "edited once");
    assert_eq!(content["message"], "edited twice");
    let old: Option<String> =
        sqlx::query_scalar("SELECT to_regclass('dm.message_revisions')::text")
            .fetch_one(store.pool())
            .await?;
    assert!(old.is_none());
    let loaded = store
        .get_message(&org, &author, conversation.id, message.id)
        .await?;
    assert!(loaded.deleted_at.is_some());
    assert_eq!(loaded.history[0]["content"]["message"], "original");
    assert!(
        store
            .revise_message(
                &org,
                &author,
                conversation.id,
                message.id,
                &"cannot-resurrect".parse()?,
                Some(serde_json::from_value(json!({"message":"no"}))?)
            )
            .await
            .is_err()
    );
    Ok(())
}
