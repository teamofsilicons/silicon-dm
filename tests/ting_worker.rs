//! Local worker failure-recovery test. The publisher is a fixture, not real Ting.

mod support;

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use secrecy::SecretString;
use silicon_dm::{
    application::commands::{CreateConversationCommand, SendMessageCommand},
    config::{DatabaseSettings, WorkerSettings},
    domain::{ActorRef, ActorType, MessageCreate, MessageStatus, OrganizationId},
    infrastructure::{
        postgres::{PostgresStore, TingDeliveryClaim, TingDeliveryContext},
        ting::{TingAcceptance, TingFailure, TingPublisher},
    },
    worker::TingDeliveryWorker,
};
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Default)]
struct Publisher {
    fail: AtomicBool,
    bodies: Mutex<Vec<String>>,
}

#[async_trait]
impl TingPublisher for Publisher {
    async fn publish(
        &self,
        claim: &TingDeliveryClaim,
    ) -> std::result::Result<TingAcceptance, TingFailure> {
        let body = &claim.request_body;
        self.bodies
            .lock()
            .map_err(|_| TingFailure::Protocol)?
            .push(body.to_owned());
        if self.fail.load(Ordering::SeqCst) {
            return Err(TingFailure::Timeout);
        }
        let value: serde_json::Value =
            serde_json::from_str(body).map_err(|_| TingFailure::Protocol)?;
        let key = value["key"].as_str().ok_or(TingFailure::Protocol)?;
        Ok(TingAcceptance {
            id: format!("fixture-{key}"),
            created_at: OffsetDateTime::UNIX_EPOCH,
            silent: false,
        })
    }
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one fixture follows the same handoff across failure and restart"
)]
async fn worker_recovers_offline_handoffs_after_restart_without_changing_dm_receipts() -> Result {
    let fixture = support::TestDatabase::start().await?;
    let database = DatabaseSettings {
        url: SecretString::from(fixture.url.clone()),
        max_connections: 4.try_into()?,
        min_connections: 1,
        acquire_timeout: Duration::from_secs(5),
        statement_timeout: Duration::from_secs(10),
    };
    let store = PostgresStore::connect(&database).await?;
    store.migrate().await?;
    let alice = ActorRef {
        actor_type: ActorType::Carbon,
        id: "c:alice".parse()?,
    };
    let bob = ActorRef {
        actor_type: ActorType::Carbon,
        id: "c:bob".parse()?,
    };
    let organization: OrganizationId = "tos".parse()?;
    let chat = store
        .create_conversation(CreateConversationCommand {
            organization_id: organization.clone(),
            creator: alice.clone(),
            participants: vec![alice.clone(), bob.clone()],
            idempotency_key: "worker-chat".parse()?,
        })
        .await?;
    let message = store
        .send_message(SendMessageCommand {
            organization_id: organization.clone(),
            conversation_id: chat.id,
            sender: alice.clone(),
            content: MessageCreate {
                text: Some("worker recovery".into()),
                ..MessageCreate::default()
            },
            idempotency_key: "worker-message".parse()?,
        })
        .await?;
    let context = TingDeliveryContext {
        app_id: "dm".into(),
        testing_environment_id: None,
        testing_generation: None,
    };
    let settings = WorkerSettings {
        batch_size: 100.try_into()?,
        poll_interval: Duration::from_millis(10),
        lease_duration: Duration::from_secs(30),
        max_attempts: 1,
        max_retry_delay: Duration::from_secs(30),
    };
    let publisher = Arc::new(Publisher::default());
    publisher.fail.store(true, Ordering::SeqCst);
    let worker = TingDeliveryWorker::new(
        store.clone(),
        publisher.clone(),
        context.clone(),
        Arc::from("before-restart"),
        settings.clone(),
    );
    assert_eq!(worker.process_once().await?, 2);
    let pending: i64 = sqlx::query_scalar("SELECT count(*) FROM ting_handoffs WHERE accepted_at IS NULL AND attempt_count=1 AND last_error_code='ting_acceptance_uncertain'")
        .fetch_one(store.pool()).await?;
    assert_eq!(
        pending, 2,
        "failure stays recoverable even at the legacy attempt limit"
    );
    drop(worker);
    sqlx::query(
        "UPDATE ting_handoffs SET next_attempt_at=clock_timestamp() WHERE accepted_at IS NULL",
    )
    .execute(store.pool())
    .await?;
    publisher.fail.store(false, Ordering::SeqCst);
    let reopened = PostgresStore::connect(&database).await?;
    let worker = TingDeliveryWorker::new(
        reopened,
        publisher.clone(),
        context.clone(),
        Arc::from("after-restart"),
        settings.clone(),
    );
    assert_eq!(worker.process_once().await?, 2);
    assert_eq!(worker.process_once().await?, 0);
    let accepted: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ting_handoffs WHERE accepted_at IS NOT NULL")
            .fetch_one(store.pool())
            .await?;
    assert_eq!(accepted, 2);
    {
        let bodies = publisher
            .bodies
            .lock()
            .map_err(|_| "fixture publisher poisoned")?;
        assert_eq!(bodies.len(), 4);
        for original in &bodies[..2] {
            assert!(
                bodies[2..].contains(original),
                "retry must preserve exact body/key"
            );
        }
    }
    let receipt: String = sqlx::query_scalar("SELECT status::text FROM messages WHERE id=$1")
        .bind(message.id)
        .fetch_one(store.pool())
        .await?;
    assert_eq!(receipt, "sent");
    assert_eq!(message.status, MessageStatus::Sent);
    let acknowledged: i64 =
        sqlx::query_scalar("SELECT count(*) FROM actor_deliveries WHERE acked_at IS NOT NULL")
            .fetch_one(store.pool())
            .await?;
    assert_eq!(
        acknowledged, 0,
        "Ting acceptance is not a client or DM receipt ACK"
    );
    maintenance_preserves_online_devices_and_receipts(&store, &worker).await?;
    run_loop_wakes_for_revisions(
        &store,
        &alice,
        &organization,
        chat.id,
        message.id,
        context.clone(),
        settings.clone(),
    )
    .await?;
    run_loop_polls_without_a_listener(
        &database,
        &alice,
        &organization,
        chat.id,
        message.id,
        context,
        settings,
    )
    .await?;
    delayed_claim_cannot_start_an_expired_send(&database).await?;
    wakeup_follows_source_commit(&store, message.id).await?;
    Ok(())
}

async fn wakeup_follows_source_commit(store: &PostgresStore, message: uuid::Uuid) -> Result {
    let mut listener = store
        .subscribe_delivery_wakeups()
        .await?
        .ok_or("missing fixture listener")?;
    let insert = "INSERT INTO actor_deliveries(id,organization_id,target_kind,target_id,delivery_kind,conversation_id,message_id,delivery_revision) SELECT $2,organization_id,target_kind,target_id,delivery_kind,conversation_id,message_id,$3 FROM actor_deliveries WHERE message_id=$1 AND delivery_kind='message' ORDER BY delivery_revision LIMIT 1";
    let mut rollback = store.pool().begin().await?;
    sqlx::query(insert)
        .bind(message)
        .bind(uuid::Uuid::now_v7())
        .bind(1000_i64)
        .execute(&mut *rollback)
        .await?;
    assert!(
        tokio::time::timeout(Duration::from_millis(30), listener.recv())
            .await
            .is_err(),
        "uncommitted events must not wake producers"
    );
    rollback.rollback().await?;
    assert!(
        tokio::time::timeout(Duration::from_millis(30), listener.recv())
            .await
            .is_err(),
        "rolled-back events must not wake producers"
    );
    let mut commit = store.pool().begin().await?;
    sqlx::query(insert)
        .bind(message)
        .bind(uuid::Uuid::now_v7())
        .bind(1001_i64)
        .execute(&mut *commit)
        .await?;
    commit.commit().await?;
    let notification = tokio::time::timeout(Duration::from_secs(2), listener.recv()).await??;
    assert_eq!(notification.channel(), "dm_delivery");
    assert_eq!(
        notification.payload(),
        "dm",
        "wakeup contains only the data-plane name"
    );
    Ok(())
}

async fn maintenance_preserves_online_devices_and_receipts(
    store: &PostgresStore,
    worker: &TingDeliveryWorker,
) -> Result {
    sqlx::query("INSERT INTO client_presence_leases(organization_id,actor_kind,actor_id,device_id,heartbeat_at,lease_expires_at,activity,activity_expires_at) VALUES('tos','carbon','c:alice','expired',clock_timestamp()-interval '2 hours',clock_timestamp()-interval '1 hour','typing',clock_timestamp()-interval '119 minutes'),('tos','carbon','c:alice','active',clock_timestamp(),clock_timestamp()+interval '1 hour',NULL,NULL),('tos','carbon','c:bob','expired',clock_timestamp()-interval '2 hours',clock_timestamp()-interval '1 hour',NULL,NULL)")
        .execute(store.pool()).await?;
    sqlx::query("INSERT INTO contract_versions(family,version,status,introduced_at,deprecated_at,last_request_at) VALUES('http',999,'deprecated',clock_timestamp()-interval '10 days',clock_timestamp()-interval '8 days',clock_timestamp()-interval '8 days')")
        .execute(store.pool()).await?;
    sqlx::query("INSERT INTO idempotency_records(organization_id,actor_kind,actor_id,operation,idempotency_key,request_hash,status,response_status,created_at,expires_at) VALUES('tos','carbon','c:alice','worker.maintenance','maintenance-expired',decode(repeat('00',32),'hex'),'completed',200,clock_timestamp()-interval '2 days',clock_timestamp()-interval '1 day')")
        .execute(store.pool()).await?;
    worker.maintain_once().await?;
    let closed: i64 = sqlx::query_scalar("SELECT count(*) FROM client_presence_leases WHERE disconnected_at IS NOT NULL AND activity IS NULL AND activity_expires_at IS NULL")
        .fetch_one(store.pool()).await?;
    assert_eq!(closed, 2);
    let online_last_seen: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM actor_presence_state WHERE actor_id='c:alice' AND last_seen_at IS NOT NULL)")
        .fetch_one(store.pool()).await?;
    assert!(
        !online_last_seen,
        "expiring one device does not mark an online actor as last seen"
    );
    let offline_last_seen: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM actor_presence_state p JOIN client_presence_leases l USING(organization_id,actor_kind,actor_id) WHERE p.actor_id='c:bob' AND p.last_seen_at=l.lease_expires_at)")
        .fetch_one(store.pool()).await?;
    assert!(
        offline_last_seen,
        "last device expiry is persisted before any later heartbeat"
    );
    let expired_key: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM idempotency_records WHERE idempotency_key='maintenance-expired')")
        .fetch_one(store.pool()).await?;
    assert!(!expired_key);
    let live_keys: i64 = sqlx::query_scalar("SELECT count(*) FROM idempotency_records")
        .fetch_one(store.pool())
        .await?;
    assert_eq!(live_keys, 2);
    let sunset: bool = sqlx::query_scalar("SELECT status='sunset' AND sunset_at IS NOT NULL FROM contract_versions WHERE family='http' AND version=999")
        .fetch_one(store.pool()).await?;
    assert!(sunset);
    let sources: i64 =
        sqlx::query_scalar("SELECT count(*) FROM actor_deliveries WHERE acked_at IS NULL")
            .fetch_one(store.pool())
            .await?;
    assert_eq!(
        sources, 2,
        "Ting acceptance does not authorize deleting HTTP sync source rows"
    );
    assert_eq!(
        store.expire_http_presence_leases().await?,
        0,
        "expiration is idempotent"
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn run_loop_wakes_for_revisions(
    store: &PostgresStore,
    actor: &ActorRef,
    org: &OrganizationId,
    conversation: uuid::Uuid,
    message: uuid::Uuid,
    context: TingDeliveryContext,
    mut settings: WorkerSettings,
) -> Result {
    settings.poll_interval = Duration::from_secs(3600);
    settings.batch_size = 2.try_into()?;
    store
        .revise_message(
            org,
            actor,
            conversation,
            message,
            &"listener-startup".parse()?,
            Some(MessageCreate {
                text: Some("seed the immediate startup poll".into()),
                ..MessageCreate::default()
            }),
        )
        .await?;
    let cancellation = CancellationToken::new();
    let _cancel_on_exit = cancellation.clone().drop_guard();
    let publisher = Arc::new(Publisher::default());
    let worker = TingDeliveryWorker::new(
        store.clone(),
        publisher.clone(),
        context,
        Arc::from("listener"),
        settings,
    );
    let token = cancellation.clone();
    let task = tokio::spawn(async move { worker.run(token).await });
    // Exactly one full startup batch commits before the revision below. The next
    // scheduled poll is an hour away, so receipt of the edit requires LISTEN.
    wait_for_bodies(store, &publisher, 2).await?;
    store
        .revise_message(
            org,
            actor,
            conversation,
            message,
            &"listener-edit".parse()?,
            Some(MessageCreate {
                text: Some("edit must wake producer".into()),
                ..MessageCreate::default()
            }),
        )
        .await?;
    wait_for_bodies(store, &publisher, 4).await?;
    cancellation.cancel();
    tokio::time::timeout(Duration::from_secs(2), task).await???;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn run_loop_polls_without_a_listener(
    database: &DatabaseSettings,
    actor: &ActorRef,
    org: &OrganizationId,
    conversation: uuid::Uuid,
    message: uuid::Uuid,
    context: TingDeliveryContext,
    mut settings: WorkerSettings,
) -> Result {
    let mut single = database.clone();
    single.max_connections = 1.try_into()?;
    let store = PostgresStore::connect(&single).await?;
    assert!(store.subscribe_delivery_wakeups().await?.is_none());
    settings.poll_interval = Duration::from_millis(25);
    let publisher = Arc::new(Publisher::default());
    let worker = TingDeliveryWorker::new(
        store.clone(),
        publisher.clone(),
        context,
        Arc::from("poll-only"),
        settings,
    );
    let cancellation = CancellationToken::new();
    let _cancel_on_exit = cancellation.clone().drop_guard();
    let token = cancellation.clone();
    let task = tokio::spawn(async move { worker.run(token).await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    store
        .revise_message(
            org,
            actor,
            conversation,
            message,
            &"poll-only-edit".parse()?,
            Some(MessageCreate {
                text: Some("poll when no connection is available for listen".into()),
                ..MessageCreate::default()
            }),
        )
        .await?;
    wait_for_bodies(&store, &publisher, 2).await?;
    cancellation.cancel();
    tokio::time::timeout(Duration::from_secs(2), task).await???;
    store.pool().close().await;
    Ok(())
}

async fn wait_for_bodies(store: &PostgresStore, publisher: &Publisher, expected: usize) -> Result {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let bodies = publisher.bodies.lock().map_err(|_| "fixture publisher poisoned")?.clone();
            if bodies.len() >= expected {
                let mut ids = Vec::new();
                for body in bodies {
                    let parsed:serde_json::Value = serde_json::from_str(&body)?;
                    ids.push(parsed["key"].as_str().ok_or("missing fixture key")?.parse::<uuid::Uuid>()?);
                }
                let accepted:i64 = sqlx::query_scalar("SELECT count(*) FROM ting_handoffs WHERE delivery_id=ANY($1) AND accepted_at IS NOT NULL")
                    .bind(ids).fetch_one(store.pool()).await?;
                if usize::try_from(accepted)? >= expected {
                    return Ok::<_, Box<dyn std::error::Error + Send + Sync>>(());
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?
}

async fn delayed_claim_cannot_start_an_expired_send(database: &DatabaseSettings) -> Result {
    let mut database = database.clone();
    database.max_connections = 1.try_into()?;
    let store = PostgresStore::connect(&database).await?;
    let actor = ActorRef {
        actor_type: ActorType::Carbon,
        id: "deadline_alice".parse()?,
    };
    let other = ActorRef {
        actor_type: ActorType::Carbon,
        id: "deadline_bob".parse()?,
    };
    let org: OrganizationId = "deadline_test".parse()?;
    let conversation = store
        .create_conversation(CreateConversationCommand {
            organization_id: org.clone(),
            creator: actor.clone(),
            participants: vec![actor.clone(), other],
            idempotency_key: "deadline-chat".parse()?,
        })
        .await?;
    store
        .send_message(SendMessageCommand {
            organization_id: org,
            conversation_id: conversation.id,
            sender: actor,
            content: MessageCreate {
                text: Some("deadline".into()),
                ..MessageCreate::default()
            },
            idempotency_key: "deadline-message".parse()?,
        })
        .await?;
    let publisher = Arc::new(Publisher::default());
    let context = TingDeliveryContext {
        app_id: "dm".into(),
        testing_environment_id: None,
        testing_generation: None,
    };
    let settings = WorkerSettings {
        batch_size: 1.try_into()?,
        poll_interval: Duration::from_millis(10),
        lease_duration: Duration::from_millis(750),
        max_attempts: 1,
        max_retry_delay: Duration::from_secs(30),
    };
    let mut sandbox_context = context.clone();
    sandbox_context.testing_environment_id = Some(uuid::Uuid::new_v4());
    sandbox_context.testing_generation = Some(1);
    let unfenced = TingDeliveryWorker::new(
        store.clone(),
        publisher.clone(),
        sandbox_context,
        Arc::from("unfenced"),
        settings.clone(),
    );
    assert!(
        unfenced.process_once().await.is_err(),
        "sandbox workers require a live lifecycle fence"
    );
    assert!(
        unfenced.maintain_once().await.is_err(),
        "maintenance cannot outlive a sandbox generation either"
    );
    let held = store.pool().acquire().await?;
    let worker = TingDeliveryWorker::new(
        store.clone(),
        publisher.clone(),
        context,
        Arc::from("delayed-claim"),
        settings,
    );
    let task = tokio::spawn(async move { worker.process_once().await });
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    drop(held);
    assert_eq!(task.await??, 1);
    assert!(
        publisher
            .bodies
            .lock()
            .map_err(|_| "fixture publisher poisoned")?
            .is_empty(),
        "no proof or send may be issued after the attempt budget expired during DB acquisition"
    );
    let pending: i64 = sqlx::query_scalar("SELECT count(*) FROM ting_handoffs WHERE organization_id='deadline_test' AND accepted_at IS NULL AND attempt_count=1 AND last_error_code='ting_acceptance_uncertain'")
        .fetch_one(store.pool()).await?;
    assert_eq!(pending, 1);
    Ok(())
}
