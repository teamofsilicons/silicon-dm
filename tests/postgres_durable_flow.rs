//! PostgreSQL-backed durable messaging integration tests.

use std::{env, error::Error, num::NonZeroU32, time::Duration};

use secrecy::SecretString;
use silicon_dm::{
    AppError,
    application::commands::{
        CreateConversationCommand, OpenRealtimeSessionCommand, RecordReceiptCommand,
        SendMessageCommand,
    },
    config::DatabaseSettings,
    domain::{
        ActorRef, ActorType, Availability, IdempotencyKey, Message, MessageCreate, MessageStatus,
        OrganizationId, PageRequest, ReceiptStatus,
    },
    infrastructure::postgres::PostgresStore,
    realtime::DeliveryPayload,
};
use testcontainers::{ContainerAsync, ImageExt as _, runners::AsyncRunner as _};
use testcontainers_modules::postgres::Postgres;
use time::OffsetDateTime;
use uuid::Uuid;

type TestResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

struct TestDatabase {
    _container: Option<ContainerAsync<Postgres>>,
    store: PostgresStore,
}

struct TenantScope {
    organization_id: OrganizationId,
    sender: ActorRef,
    recipient: ActorRef,
}

impl TestDatabase {
    async fn start() -> TestResult<Self> {
        let (container, database_url) = if let Ok(database_url) = env::var("DM_TEST_DATABASE_URL") {
            (None, database_url)
        } else {
            let container = Postgres::default().with_tag("16-alpine").start().await?;
            let host = container.get_host().await?;
            let port = container.get_host_port_ipv4(5432).await?;
            (
                Some(container),
                format!("postgres://postgres:postgres@{host}:{port}/postgres"),
            )
        };
        let settings = DatabaseSettings {
            url: SecretString::from(database_url),
            max_connections: "8".parse::<NonZeroU32>()?,
            min_connections: 1,
            acquire_timeout: Duration::from_secs(10),
            statement_timeout: Duration::from_secs(30),
        };
        let store = PostgresStore::connect(&settings).await?;
        assert!(store.readiness().await.is_err());
        store.migrate().await?;
        store.readiness().await?;
        Ok(Self {
            _container: container,
            store,
        })
    }
}

#[tokio::test]
async fn durable_conversation_message_receipt_and_ack_flow() -> TestResult<()> {
    let database = TestDatabase::start().await?;
    let scope = TenantScope {
        organization_id: "organization-primary".parse()?,
        sender: ActorRef {
            actor_type: ActorType::Carbon,
            id: "carbon-alice".parse()?,
        },
        recipient: ActorRef {
            actor_type: ActorType::Silicon,
            id: "silicon-bob".parse()?,
        },
    };

    let conversation_id = exercise_exact_participant_idempotency(&database.store, &scope).await?;
    exercise_tenant_isolation(&database.store, &scope, conversation_id).await?;
    let message =
        exercise_message_persistence_and_replay(&database.store, &scope, conversation_id).await?;
    exercise_ack_cursor(&database.store, &scope, &message).await?;
    exercise_monotonic_receipts(&database.store, &scope, &message).await?;
    exercise_concurrent_final_disconnect(&database.store, &scope).await?;

    database.store.pool().close().await;
    Ok(())
}

async fn exercise_concurrent_final_disconnect(
    store: &PostgresStore,
    scope: &TenantScope,
) -> TestResult<()> {
    let first_session = Uuid::now_v7();
    let second_session = Uuid::now_v7();
    for (session_id, consumer_id) in [
        (first_session, "presence-device-1"),
        (second_session, "presence-device-2"),
    ] {
        store
            .open_realtime_session(OpenRealtimeSessionCommand {
                session_id,
                instance_id: "integration-instance".to_owned(),
                consumer_id: consumer_id.to_owned(),
                authenticated_subject: scope.recipient.id.to_string(),
                organization_id: scope.organization_id.clone(),
                actors: vec![scope.recipient.clone()],
                lease_expires_at: OffsetDateTime::now_utc() + time::Duration::minutes(2),
            })
            .await?;
    }

    let (first, second) = tokio::join!(
        store.close_realtime_session(first_session, 1000, "test-close"),
        store.close_realtime_session(second_session, 1000, "test-close"),
    );
    first?;
    second?;
    let presence = store
        .get_presence(&scope.organization_id, &scope.sender, &scope.recipient.id)
        .await?;
    assert_eq!(presence.availability, Availability::Offline);
    assert!(presence.last_seen_at.is_some());
    Ok(())
}

async fn exercise_exact_participant_idempotency(
    store: &PostgresStore,
    scope: &TenantScope,
) -> TestResult<Uuid> {
    let first = store
        .create_conversation(CreateConversationCommand {
            organization_id: scope.organization_id.clone(),
            creator: scope.sender.clone(),
            participants: vec![scope.sender.clone(), scope.recipient.clone()],
            idempotency_key: idempotency_key("conversation-create-primary")?,
        })
        .await?;
    let retry = store
        .create_conversation(CreateConversationCommand {
            organization_id: scope.organization_id.clone(),
            creator: scope.sender.clone(),
            participants: vec![
                scope.recipient.clone(),
                scope.sender.clone(),
                scope.recipient.clone(),
            ],
            idempotency_key: idempotency_key("conversation-create-primary")?,
        })
        .await?;
    let same_exact_set = store
        .create_conversation(CreateConversationCommand {
            organization_id: scope.organization_id.clone(),
            creator: scope.sender.clone(),
            participants: vec![scope.recipient.clone(), scope.sender.clone()],
            idempotency_key: idempotency_key("conversation-create-second-key")?,
        })
        .await?;

    assert_eq!(retry.id, first.id);
    assert_eq!(same_exact_set.id, first.id);
    assert_eq!(first.participants.len(), 2);
    Ok(first.id)
}

async fn exercise_tenant_isolation(
    store: &PostgresStore,
    scope: &TenantScope,
    primary_conversation_id: Uuid,
) -> TestResult<()> {
    let other_organization: OrganizationId = "organization-secondary".parse()?;
    let other = store
        .create_conversation(CreateConversationCommand {
            organization_id: other_organization.clone(),
            creator: scope.sender.clone(),
            participants: vec![scope.sender.clone(), scope.recipient.clone()],
            idempotency_key: idempotency_key("conversation-create-primary")?,
        })
        .await?;

    assert_ne!(other.id, primary_conversation_id);
    assert!(matches!(
        store
            .get_conversation(&other_organization, &scope.sender, primary_conversation_id)
            .await,
        Err(AppError::NotFound)
    ));
    assert!(
        store
            .replay_deliveries(&other_organization, &scope.recipient, 0, 10)
            .await?
            .is_empty()
    );
    Ok(())
}

async fn exercise_message_persistence_and_replay(
    store: &PostgresStore,
    scope: &TenantScope,
    conversation_id: Uuid,
) -> TestResult<Message> {
    let content = MessageCreate {
        text: Some("a durable hello".to_owned()),
        ..MessageCreate::default()
    };
    let first = store
        .send_message(SendMessageCommand {
            organization_id: scope.organization_id.clone(),
            conversation_id,
            sender: scope.sender.clone(),
            content: content.clone(),
            voice_duration_milliseconds: None,
            idempotency_key: idempotency_key("message-create-primary")?,
        })
        .await?;
    let retry = store
        .send_message(SendMessageCommand {
            organization_id: scope.organization_id.clone(),
            conversation_id,
            sender: scope.sender.clone(),
            content,
            voice_duration_milliseconds: None,
            idempotency_key: idempotency_key("message-create-primary")?,
        })
        .await?;

    assert_eq!(retry.id, first.id);
    assert_eq!(first.status, MessageStatus::Sent);
    let history = store
        .list_messages(
            &scope.organization_id,
            &scope.sender,
            conversation_id,
            &PageRequest::default(),
            false,
        )
        .await?;
    assert_eq!(history.items.len(), 1);
    assert_eq!(history.items[0].id, first.id);

    let deliveries = store
        .replay_deliveries(&scope.organization_id, &scope.recipient, 0, 10)
        .await?;
    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0].sequence, 1);
    assert_eq!(deliveries[0].target, scope.recipient);
    assert!(matches!(
        &deliveries[0].payload,
        DeliveryPayload::Message { message } if message.id == first.id
    ));
    assert_eq!(
        store
            .delivery_high_watermark(&scope.organization_id, &scope.recipient)
            .await?,
        1
    );
    Ok(first)
}

async fn exercise_ack_cursor(
    store: &PostgresStore,
    scope: &TenantScope,
    message: &Message,
) -> TestResult<()> {
    const CONSUMER_ID: &str = "integration-device-001";

    store
        .acknowledge_deliveries(&scope.organization_id, &scope.recipient, CONSUMER_ID, 1)
        .await?;
    store
        .acknowledge_deliveries(&scope.organization_id, &scope.recipient, CONSUMER_ID, 0)
        .await?;
    assert_eq!(
        store
            .acknowledged_through(&scope.organization_id, &scope.recipient, CONSUMER_ID)
            .await?,
        1
    );

    let retained = store
        .replay_deliveries(&scope.organization_id, &scope.recipient, 0, 10)
        .await?;
    assert_eq!(retained.len(), 1);
    assert!(matches!(
        &retained[0].payload,
        DeliveryPayload::Message { message: delivered } if delivered.id == message.id
    ));
    Ok(())
}

async fn exercise_monotonic_receipts(
    store: &PostgresStore,
    scope: &TenantScope,
    message: &Message,
) -> TestResult<()> {
    let delivered = store
        .record_receipt(receipt_command(scope, message, ReceiptStatus::Delivered))
        .await?;
    let read = store
        .record_receipt(receipt_command(scope, message, ReceiptStatus::Read))
        .await?;
    let downgraded = store
        .record_receipt(receipt_command(scope, message, ReceiptStatus::Delivered))
        .await?;

    assert_eq!(delivered.status, MessageStatus::Delivered);
    assert_eq!(read.status, MessageStatus::Read);
    assert_eq!(downgraded.status, MessageStatus::Read);

    let sender_deliveries = store
        .replay_deliveries(&scope.organization_id, &scope.sender, 0, 10)
        .await?;
    assert_eq!(sender_deliveries.len(), 2);
    assert_eq!(sender_deliveries[0].sequence, 1);
    assert_eq!(sender_deliveries[1].sequence, 2);
    assert!(matches!(
        &sender_deliveries[0].payload,
        DeliveryPayload::Receipt {
            message_id,
            status: MessageStatus::Delivered,
        } if *message_id == message.id
    ));
    assert!(matches!(
        &sender_deliveries[1].payload,
        DeliveryPayload::Receipt {
            message_id,
            status: MessageStatus::Read,
        } if *message_id == message.id
    ));
    Ok(())
}

fn receipt_command(
    scope: &TenantScope,
    message: &Message,
    status: ReceiptStatus,
) -> RecordReceiptCommand {
    RecordReceiptCommand {
        organization_id: scope.organization_id.clone(),
        conversation_id: message.conversation_id,
        message_id: message.id,
        recipient: scope.recipient.clone(),
        device_id: "integration-device-001".to_owned(),
        status,
    }
}

fn idempotency_key(value: &str) -> TestResult<IdempotencyKey> {
    Ok(value.parse()?)
}
