//! Real PostgreSQL upgrade and failure-recovery proof for the Ting producer outbox.
//! Uses disposable Docker PostgreSQL unless `DM_TEST_DATABASE_URL` names a fresh native fixture.

mod support;

use secrecy::SecretString;
use serde_json::{Value, json};
use silicon_dm::{
    application::commands::{CreateConversationCommand, RecordReceiptCommand, SendMessageCommand},
    config::DatabaseSettings,
    domain::{ActorRef, ActorType, MessageCreate, MessageStatus, OrganizationId, ReceiptStatus},
    infrastructure::postgres::{PostgresStore, TingDeliveryContext},
};
use std::{error::Error, time::Duration};
use time::OffsetDateTime;
use uuid::Uuid;

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one isolated upgrade fixture follows the same handoff across crashes, edits and receipts"
)]
async fn ting_handoffs_preserve_pending_events_and_separate_acceptance_from_receipts() -> TestResult
{
    let fixture = support::TestDatabase::start().await?;
    let settings = DatabaseSettings {
        url: SecretString::from(fixture.url.clone()),
        max_connections: 6.try_into()?,
        min_connections: 1,
        acquire_timeout: Duration::from_secs(10),
        statement_timeout: Duration::from_secs(30),
    };
    let store = PostgresStore::connect(&settings).await?;
    let mut connection = store.pool().acquire().await?;
    sqlx::query("SET search_path=public")
        .execute(&mut *connection)
        .await?;
    sqlx::migrate!("./migrations")
        .run_to(27, &mut *connection)
        .await?;
    sqlx::query("SET search_path=dm")
        .execute(&mut *connection)
        .await?;
    drop(connection);

    let org: OrganizationId = "tos".parse()?;
    let author = ActorRef {
        actor_type: ActorType::Carbon,
        id: "alice".parse()?,
    };
    let recipient = ActorRef {
        actor_type: ActorType::Silicon,
        id: "cos:tos".parse()?,
    };
    let conversation = store
        .create_conversation(CreateConversationCommand {
            organization_id: org.clone(),
            creator: author.clone(),
            participants: vec![author.clone(), recipient.clone()],
            idempotency_key: "ting-chat".parse()?,
        })
        .await?;
    let message = store
        .send_message(SendMessageCommand {
            organization_id: org.clone(),
            conversation_id: conversation.id,
            sender: author.clone(),
            content: serde_json::from_value(
                json!({"message":"private original", "recipient_id":"planner@cos:tos"}),
            )?,
            idempotency_key: "ting-original".parse()?,
        })
        .await?;
    store
        .acknowledge_deliveries(&org, &author, "old-websocket-device", 1)
        .await?;
    let legacy_pending: Vec<Uuid> =
        sqlx::query_scalar("SELECT id FROM actor_deliveries WHERE acked_at IS NULL ORDER BY id")
            .fetch_all(store.pool())
            .await?;
    assert_eq!(legacy_pending.len(), 1);
    store.migrate().await?;
    let migrated: Vec<Uuid> =
        sqlx::query_scalar("SELECT delivery_id FROM ting_handoffs ORDER BY delivery_id")
            .fetch_all(store.pool())
            .await?;
    assert_eq!(
        migrated, legacy_pending,
        "old pending events survive with their original IDs"
    );
    assert_handoff_rollback(&store, legacy_pending[0]).await?;

    let context = TingDeliveryContext {
        app_id: "tos>dm".into(),
        testing_environment_id: None,
        testing_generation: None,
    };
    let (left, right) = tokio::join!(
        store.claim_ting_deliveries("worker-left", 1, Duration::from_secs(60), &context),
        store.claim_ting_deliveries("worker-right", 1, Duration::from_secs(60), &context),
    );
    let mut claimed = left?;
    claimed.extend(right?);
    assert_eq!(
        claimed.len(),
        1,
        "concurrent publishers must not share a live lease"
    );
    let original = claimed.pop().ok_or("missing claim")?;
    assert_eq!(original.originator, Some(author.clone()));
    let body: Value = serde_json::from_str(&original.request_body)?;
    assert_eq!(body["key"], original.delivery_id.to_string());
    assert_eq!(body["type"], "tos>dm.sync.changed");
    assert_eq!(body["for"], "cos:tos");
    assert_eq!(body["data"]["conversation_id"], "alice::cos:tos");
    assert_eq!(body["data"]["message_id"], "000");
    assert_eq!(body["data"]["event"], "message.created");
    assert_eq!(body["metadata"]["isi"], "planner");
    assert!(body["metadata"]["testing_generation"].is_null());
    assert!(!original.request_body.contains("private original"));
    assert!(store.ting_delivery_authorized(&original).await?);
    let mut wrong_user = original.clone();
    wrong_user.originator = Some(recipient.clone());
    assert!(
        !store.ting_delivery_authorized(&wrong_user).await?,
        "a cached recipient login cannot replace the actor that caused this event"
    );
    assert!(
        sqlx::query("UPDATE ting_handoffs SET originator_kind=$2::text::actor_kind,originator_id=$3 WHERE delivery_id=$1")
            .bind(original.delivery_id).bind(recipient.actor_type.as_str()).bind(recipient.id.as_str())
            .execute(store.pool()).await.is_err(),
        "event provenance cannot be rewritten to another logged-in actor"
    );
    assert!(
        sqlx::query(
            "UPDATE ting_handoffs SET originator_kind=NULL,originator_id=NULL WHERE delivery_id=$1"
        )
        .bind(original.delivery_id)
        .execute(store.pool())
        .await
        .is_err(),
        "event provenance cannot be erased"
    );

    // A membership change after claiming must stop the network send without losing work.
    sqlx::query("UPDATE actor_snapshots SET status='suspended',iam_version=iam_version+1 WHERE actor_id='cos:tos'")
        .execute(store.pool()).await?;
    assert!(!store.ting_delivery_authorized(&original).await?);
    store
        .retry_ting_delivery(
            original.delivery_id,
            original.lease_id,
            OffsetDateTime::now_utc(),
            "permission_changed",
        )
        .await?;
    assert!(
        store
            .claim_ting_deliveries("worker-paused", 100, Duration::from_secs(60), &context)
            .await?
            .is_empty()
    );
    sqlx::query("UPDATE actor_snapshots SET status='active',iam_version=iam_version+1 WHERE actor_id='cos:tos'")
        .execute(store.pool()).await?;

    // An edit creates its own immutable event; it cannot reserialize the uncertain original send.
    let edited: MessageCreate = serde_json::from_value(json!({"message":"private edited"}))?;
    store
        .revise_message(
            &org,
            &author,
            conversation.id,
            message.id,
            &"ting-edit".parse()?,
            Some(edited),
        )
        .await?;
    let reopened = PostgresStore::connect(&settings).await?;
    let claims = reopened
        .claim_ting_deliveries(
            "worker-after-restart",
            100,
            Duration::from_secs(60),
            &context,
        )
        .await?;
    assert_eq!(claims.len(), 3);
    assert!(
        claims
            .iter()
            .all(|claim| claim.originator.as_ref() == Some(&author))
    );
    let retried = claims
        .iter()
        .find(|claim| claim.delivery_id == original.delivery_id)
        .ok_or("missing retried event")?;
    assert_eq!(retried.request_body, original.request_body);
    assert_eq!(retried.attempt_count, 1);
    assert_ne!(retried.lease_id, original.lease_id);
    let accepted_time = OffsetDateTime::now_utc().replace_nanosecond(0)?;
    assert!(
        store
            .accept_ting_delivery(
                original.delivery_id,
                original.lease_id,
                "stale-acceptance",
                accepted_time,
                false
            )
            .await
            .is_err()
    );
    assert!(
        sqlx::query("UPDATE ting_handoffs SET request_body='{}' WHERE delivery_id=$1")
            .bind(original.delivery_id)
            .execute(store.pool())
            .await
            .is_err(),
        "database forbids rewriting a prepared request"
    );

    for claim in &claims {
        store
            .accept_ting_delivery(
                claim.delivery_id,
                claim.lease_id,
                &format!("ting-{}", claim.delivery_id),
                accepted_time,
                false,
            )
            .await?;
    }
    store
        .accept_ting_delivery(
            retried.delivery_id,
            retried.lease_id,
            &format!("ting-{}", retried.delivery_id),
            accepted_time,
            false,
        )
        .await?;
    assert!(
        store
            .claim_ting_deliveries("worker-done", 100, Duration::from_secs(60), &context)
            .await?
            .is_empty()
    );
    let loaded = store
        .get_message(&org, &author, conversation.id, message.id)
        .await?;
    assert_eq!(
        loaded.status,
        MessageStatus::Sent,
        "Ting acceptance is not a DM receipt"
    );
    let acked: bool =
        sqlx::query_scalar("SELECT acked_at IS NOT NULL FROM actor_deliveries WHERE id=$1")
            .bind(original.delivery_id)
            .fetch_one(store.pool())
            .await?;
    assert!(!acked, "Ting acceptance is not the old DM websocket ACK");

    // Explicit receipts still produce their own source events and monotonic DM state.
    store
        .record_receipt(RecordReceiptCommand {
            organization_id: org.clone(),
            conversation_id: conversation.id,
            message_id: message.id,
            recipient: recipient.clone(),
            device_id: "ting-destination".into(),
            status: ReceiptStatus::Delivered,
        })
        .await?;
    let receipt = store
        .claim_ting_deliveries("receipt-worker", 100, Duration::from_secs(60), &context)
        .await?;
    assert_eq!(receipt.len(), 1);
    assert_eq!(receipt[0].originator, Some(recipient.clone()));
    assert_ne!(receipt[0].originator.as_ref(), Some(&receipt[0].target));
    let receipt_body: Value = serde_json::from_str(&receipt[0].request_body)?;
    assert_eq!(receipt_body["data"]["event"], "message.delivered");
    assert_eq!(receipt_body["for"], "alice");
    assert_eq!(
        store
            .get_message(&org, &author, conversation.id, message.id)
            .await?
            .status,
        MessageStatus::Delivered
    );

    // Lease expiry recovers a crash; saturation can never dead-letter or fail a message.
    sqlx::query("UPDATE ting_handoffs SET lease_expires_at=clock_timestamp()-interval '1 second',attempt_count=9223372036854775807 WHERE delivery_id=$1")
        .bind(receipt[0].delivery_id).execute(store.pool()).await?;
    let recovered = store
        .claim_ting_deliveries("receipt-recovered", 100, Duration::from_secs(60), &context)
        .await?;
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].request_body, receipt[0].request_body);
    assert!(
        store
            .retry_ting_delivery(
                receipt[0].delivery_id,
                receipt[0].lease_id,
                OffsetDateTime::now_utc(),
                "stale"
            )
            .await
            .is_err()
    );
    store
        .retry_ting_delivery(
            recovered[0].delivery_id,
            recovered[0].lease_id,
            OffsetDateTime::now_utc(),
            "upstream_unavailable",
        )
        .await?;
    let attempts: i64 =
        sqlx::query_scalar("SELECT attempt_count FROM ting_handoffs WHERE delivery_id=$1")
            .bind(recovered[0].delivery_id)
            .fetch_one(store.pool())
            .await?;
    assert_eq!(attempts, i64::MAX);
    assert_eq!(
        store
            .get_message(&org, &author, conversation.id, message.id)
            .await?
            .status,
        MessageStatus::Delivered
    );

    // Delete produces a fresh content-free reference.
    store
        .revise_message(
            &org,
            &author,
            conversation.id,
            message.id,
            &"ting-delete".parse()?,
            None,
        )
        .await?;
    let delete_events: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ting_handoffs WHERE event='message.deleted'")
            .fetch_one(store.pool())
            .await?;
    assert_eq!(delete_events, 2);
    let delete_origins: Vec<String> =
        sqlx::query_scalar("SELECT originator_id FROM ting_handoffs WHERE event='message.deleted'")
            .fetch_all(store.pool())
            .await?;
    assert!(delete_origins.iter().all(|id| id == author.id.as_str()));
    let mismatch = TingDeliveryContext {
        app_id: "tos>dm".into(),
        testing_environment_id: Some(Uuid::new_v4()),
        testing_generation: Some(1),
    };
    assert!(
        store
            .claim_ting_deliveries("wrong-context", 10, Duration::from_secs(60), &mismatch)
            .await
            .is_err()
    );
    assert!(
        store
            .claim_ting_deliveries("unbounded", 101, Duration::from_secs(60), &context)
            .await
            .is_err()
    );
    assert_generation_fencing(&settings, &store, original.delivery_id).await?;
    assert_explicit_originator(&store, &org, &author, conversation.id).await?;
    reopened.pool().close().await;
    store.pool().close().await;
    Ok(())
}

async fn assert_explicit_originator(
    store: &PostgresStore,
    org: &OrganizationId,
    sender: &ActorRef,
    conversation: Uuid,
) -> TestResult {
    let initiator = ActorRef {
        actor_type: ActorType::Carbon,
        id: "delegate".parse()?,
    };
    // The API authenticates and authorizes representation before this store call.
    let delegated = store
        .send_message_as(
            SendMessageCommand {
                organization_id: org.clone(),
                conversation_id: conversation,
                sender: sender.clone(),
                content: serde_json::from_value(json!({"message":"represented sender"}))?,
                idempotency_key: "delegated-origin".parse()?,
            },
            &initiator,
        )
        .await?;
    let origins: Vec<(String,String)> = sqlx::query_as(
        "SELECT h.originator_kind::text,h.originator_id FROM ting_handoffs h JOIN actor_deliveries d ON d.id=h.delivery_id WHERE d.message_id=$1"
    ).bind(delegated.id).fetch_all(store.pool()).await?;
    assert_eq!(origins.len(), 2);
    assert!(
        origins
            .iter()
            .all(|(kind, id)| kind == "carbon" && id == initiator.id.as_str())
    );
    let stored_sender: String = sqlx::query_scalar("SELECT sender_id FROM messages WHERE id=$1")
        .bind(delegated.id)
        .fetch_one(store.pool())
        .await?;
    assert_eq!(
        stored_sender,
        sender.id.as_str(),
        "provenance does not alter the message sender"
    );
    // Every pooled connection must lose the transaction-local override.
    let mut connections = Vec::new();
    for _ in 0..6 {
        let mut connection = store.pool().acquire().await?;
        let clear: bool = sqlx::query_scalar(
            "SELECT NULLIF(current_setting('dm.ting_originator',true),'') IS NULL",
        )
        .fetch_one(&mut *connection)
        .await?;
        assert!(clear, "delegated provenance cannot leak to another request");
        connections.push(connection);
    }
    drop(connections);
    let direct = store
        .send_message(SendMessageCommand {
            organization_id: org.clone(),
            conversation_id: conversation,
            sender: sender.clone(),
            content: serde_json::from_value(json!({"message":"own subsequent message"}))?,
            idempotency_key: "subsequent-origin".parse()?,
        })
        .await?;
    let origins: Vec<String> = sqlx::query_scalar(
        "SELECT h.originator_id FROM ting_handoffs h JOIN actor_deliveries d ON d.id=h.delivery_id WHERE d.message_id=$1"
    ).bind(direct.id).fetch_all(store.pool()).await?;
    assert_eq!(origins.len(), 2);
    assert!(origins.iter().all(|id| id == sender.id.as_str()));
    assert_unattributed_failure(store, direct.id).await
}

async fn assert_unattributed_failure(store: &PostgresStore, message: Uuid) -> TestResult {
    let delivery: Uuid =
        sqlx::query_scalar("SELECT id FROM actor_deliveries WHERE message_id=$1 LIMIT 1")
            .bind(message)
            .fetch_one(store.pool())
            .await?;
    sqlx::query("UPDATE actor_deliveries SET lease_owner='legacy-worker',lease_started_at=clock_timestamp(),lease_expires_at=clock_timestamp()+interval '1 minute' WHERE id=$1")
        .bind(delivery).execute(store.pool()).await?;
    store
        .dead_letter_delivery(delivery, "legacy-worker", "legacy_failure")
        .await?;
    let failure_origins: Vec<(Option<String>,Option<String>)> = sqlx::query_as(
        "SELECT h.originator_kind::text,h.originator_id FROM ting_handoffs h JOIN actor_deliveries d ON d.id=h.delivery_id WHERE d.message_id=$1 AND h.event='message.failed'"
    ).bind(message).fetch_all(store.pool()).await?;
    assert_eq!(
        failure_origins,
        vec![(None, None)],
        "autonomous failures never borrow the sender's login"
    );
    Ok(())
}

async fn assert_generation_fencing(
    settings: &DatabaseSettings,
    production: &PostgresStore,
    source: Uuid,
) -> TestResult {
    let environment = Uuid::new_v4();
    let schema = format!("dm_test_{}", environment.simple());
    // Isolate the producer table and reuse read-only authorization fixtures. Actual
    // sandbox provisioning exercises these tables through the shared migrator.
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "CREATE SCHEMA {schema}; CREATE TABLE {schema}.ting_handoffs (LIKE dm.ting_handoffs INCLUDING ALL);\
         CREATE VIEW {schema}.organization_snapshots AS SELECT * FROM dm.organization_snapshots;\
         CREATE VIEW {schema}.actor_snapshots AS SELECT * FROM dm.actor_snapshots;\
         CREATE VIEW {schema}.effective_conversation_participants AS SELECT * FROM dm.effective_conversation_participants"
    ))).execute(production.pool()).await?;
    let scoped = PostgresStore::connect_schema(settings, &schema).await?;
    for (index, generation) in [(0, 1), (1, 2)] {
        let event = Uuid::now_v7();
        sqlx::query("INSERT INTO ting_handoffs(delivery_id,organization_id,target_kind,target_id,conversation_id,public_conversation_id,message_sequence,delivery_sequence,event,routing_address,created_at) SELECT $2,organization_id,target_kind,target_id,conversation_id,public_conversation_id,message_sequence,delivery_sequence,event,routing_address,created_at FROM dm.ting_handoffs WHERE delivery_id=$1")
            .bind(source).bind(event).execute(scoped.pool()).await?;
        let context = TingDeliveryContext {
            app_id: "tos>dm".into(),
            testing_environment_id: Some(environment),
            testing_generation: Some(generation),
        };
        let claims = scoped
            .claim_ting_deliveries("sandbox-worker", 100, Duration::from_secs(60), &context)
            .await?;
        assert_eq!(
            claims.len(),
            1,
            "a pending old generation cannot block or join a new generation claim"
        );
        assert_eq!(claims[0].delivery_id, event);
        let body: Value = serde_json::from_str(&claims[0].request_body)?;
        assert_eq!(
            body["metadata"]["testing_environment_id"],
            environment.to_string()
        );
        assert_eq!(body["metadata"]["testing_generation"], generation);
        scoped
            .retry_ting_delivery(
                event,
                claims[0].lease_id,
                OffsetDateTime::now_utc(),
                "still_pending",
            )
            .await?;
        let retained: i64 =
            sqlx::query_scalar("SELECT count(*) FROM ting_handoffs WHERE accepted_at IS NULL")
                .fetch_one(scoped.pool())
                .await?;
        assert_eq!(retained, index + 1);
    }
    scoped.pool().close().await;
    Ok(())
}

async fn assert_handoff_rollback(store: &PostgresStore, source: Uuid) -> TestResult {
    let rolled_back_id = Uuid::now_v7();
    let mut transaction = store.pool().begin().await?;
    sqlx::query("INSERT INTO actor_deliveries(id,organization_id,target_kind,target_id,delivery_kind,conversation_id,message_id,delivery_revision) SELECT $2,organization_id,target_kind,target_id,delivery_kind,conversation_id,message_id,delivery_revision+1000 FROM actor_deliveries WHERE id=$1")
        .bind(source).bind(rolled_back_id).execute(&mut *transaction).await?;
    let pending: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM ting_handoffs WHERE delivery_id=$1)")
            .bind(rolled_back_id)
            .fetch_one(&mut *transaction)
            .await?;
    assert!(
        pending,
        "the handoff must exist within the source transaction"
    );
    transaction.rollback().await?;
    let orphaned: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM ting_handoffs WHERE delivery_id=$1)")
            .bind(rolled_back_id)
            .fetch_one(store.pool())
            .await?;
    assert!(
        !orphaned,
        "rolling back the source transaction rolls back its handoff"
    );
    Ok(())
}
