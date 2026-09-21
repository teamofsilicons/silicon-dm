//! PostgreSQL-backed durable messaging integration tests.

use std::{env, error::Error, num::NonZeroU32, time::Duration};

use secrecy::SecretString;
use serde::Serialize;
use silicon_dm::{
    AppError,
    application::commands::{
        CreateConversationCommand, OpenRealtimeSessionCommand, PutDraftCommand, PutDraftOutcome,
        RecordReceiptCommand, SendMessageCommand,
    },
    config::DatabaseSettings,
    domain::{
        ActorRef, ActorType, Availability, DraftInput, IdempotencyKey, Message, MessageCreate,
        MessageStatus, OrganizationId, PageRequest, ReceiptStatus, VoiceAttachment,
    },
    infrastructure::postgres::PostgresStore,
    realtime::DeliveryPayload,
};
use sqlx::migrate::Migrator;
use testcontainers::{ContainerAsync, ImageExt as _, runners::AsyncRunner as _};
use testcontainers_modules::postgres::Postgres;
use time::OffsetDateTime;
use uuid::Uuid;

type TestResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

#[tokio::test]
async fn canonical_iam_snapshots_and_tombstones_preserve_existing_directory_rows() -> TestResult<()>
{
    use serde_json::json;
    use silicon_iam_client::models::{ApplicationAuthorization, WebhookEvent};

    let database = TestDatabase::start().await?;
    let store = &database.store;
    let org: OrganizationId = "canonical-cutover".parse()?;
    let actor = ActorRef {
        actor_type: ActorType::Carbon,
        id: "alice".parse()?,
    };
    let iam_org = Uuid::new_v4();
    let old_membership = Uuid::new_v4();
    store
        .refresh_directory(&org, std::slice::from_ref(&actor))
        .await?;
    // This row was written by the previously deployed SDK1 consumer.
    sqlx::query("INSERT INTO iam_membership_projections(membership_id,principal_id,iam_organization_id,organization_id,actor_kind,actor_id,iam_version,authorization_epoch,status) VALUES($1,$2,$3,$4,'carbon','alice',1,1,'active')")
        .bind(old_membership).bind(Uuid::new_v4()).bind(iam_org).bind(org.as_str())
        .execute(store.pool()).await?;
    let snapshot = |membership: &str| -> Result<ApplicationAuthorization, serde_json::Error> {
        serde_json::from_value(json!({
            "actor_type":"carbon","public_id":"alice","organization_id":iam_org,
            "org_id":org,"membership_id":membership,"membership_version":1,
            "authorization_epoch":1,"audience":"tos>dm","testing_environment_id":null,
            "scopes":[],"org_role":"member","tags":[]
        }))
    };
    for membership in [old_membership.to_string(), format!("alice[{org}]")] {
        store
            .project_iam_authorization(&snapshot(&membership)?, &actor)
            .await?;
    }
    let rows: Vec<Uuid> = sqlx::query_scalar(
        "SELECT membership_id FROM iam_membership_projections WHERE organization_id=$1",
    )
    .bind(org.as_str())
    .fetch_all(store.pool())
    .await?;
    assert_eq!(
        rows,
        vec![old_membership],
        "identity cutover must retain the row"
    );

    // New IAM no longer discloses a principal UUID, including scoped removals.
    let event: WebhookEvent = serde_json::from_value(json!({
        "spec_version":"1.0","event_id":Uuid::new_v4(),
        "event_type":"organization.membership.removed.v1","occurred_at":"2026-09-21T00:00:00Z",
        "organization_id":iam_org,"aggregate":{},"data":{"current":{"members":[{
            "resource":{"type":"organization_membership","id":old_membership,
                "membership_id":format!("alice[{org}]"),"principal_type":"carbon","status":"removed","version":2},
            "authorization":"removed"
        }]}}
    }))?;
    let mut transaction = store.pool().begin().await?;
    PostgresStore::project_iam_webhook(&mut transaction, &event).await?;
    transaction.commit().await?;
    assert!(
        store
            .project_iam_authorization(&snapshot(&format!("alice[{org}]"))?, &actor)
            .await
            .is_err()
    );
    let status: String =
        sqlx::query_scalar("SELECT status FROM iam_membership_projections WHERE membership_id=$1")
            .bind(old_membership)
            .fetch_one(store.pool())
            .await?;
    assert_eq!(status, "removed");
    Ok(())
}

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
        // Seed the legacy schema using the same public migration journal as
        // PostgresStore::migrate, then return the connection to its DM scope.
        let mut migration_connection = store.pool().acquire().await?;
        sqlx::query("SET search_path = public")
            .execute(&mut *migration_connection)
            .await?;
        let seeded = MIGRATOR.run_to(3, &mut *migration_connection).await;
        let restored = sqlx::query("SET search_path = dm")
            .execute(&mut *migration_connection)
            .await;
        if restored.is_err() {
            migration_connection.close_on_drop();
        }
        seeded?;
        restored?;
        drop(migration_connection);
        exercise_product_contract_upgrade(&store).await?;
        store.readiness().await?;
        Ok(Self {
            _container: container,
            store,
        })
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the chronological v3-to-v4 fixture keeps legacy seeding, migration, and post-upgrade invariants auditable in one flow"
)]
async fn exercise_product_contract_upgrade(store: &PostgresStore) -> TestResult<()> {
    const ORGANIZATION_ID: &str = "organization-legacy-hook";
    const SENDER_ID: &str = "carbon-legacy-message";
    const TARGET_ID: &str = "silicon-legacy-hook";

    let organization_id: OrganizationId = ORGANIZATION_ID.parse()?;
    let sender = ActorRef {
        actor_type: ActorType::Carbon,
        id: SENDER_ID.parse()?,
    };
    let target = ActorRef {
        actor_type: ActorType::Silicon,
        id: TARGET_ID.parse()?,
    };
    store
        .refresh_directory(&organization_id, &[sender.clone(), target.clone()])
        .await?;
    // Seed the historical schema directly: current store readers require current migrations.
    let conversation_id = Uuid::now_v7();
    let mut tx = store.pool().begin().await?;
    sqlx::query("INSERT INTO conversations(id,organization_id,participant_set_hash,created_by_kind,created_by_id) VALUES($1,$2,$3,'carbon',$4)")
        .bind(conversation_id).bind(ORGANIZATION_ID).bind([1_u8;32].as_slice()).bind(SENDER_ID).execute(&mut *tx).await?;
    for member in [&sender, &target] {
        sqlx::query("INSERT INTO conversation_participants(conversation_id,organization_id,actor_kind,actor_id) VALUES($1,$2,$3::text::actor_kind,$4)")
            .bind(conversation_id).bind(ORGANIZATION_ID).bind(member.actor_type.as_str()).bind(member.id.as_str()).execute(&mut *tx).await?;
    }
    tx.commit().await?;

    let legacy_message_id = Uuid::now_v7();
    let legacy_voice_url = "https://media.example/legacy.ogg";
    let legacy_voice_hash = legacy_voice_content_hash(legacy_voice_url)?;
    let mut transaction = store.pool().begin().await?;
    sqlx::query(
        r#"
        INSERT INTO dm.messages (
            id,
            conversation_id,
            organization_id,
            sender_kind,
            sender_id,
            sequence,
            status,
            voice_transcript,
            transcription_result,
            content_hash
        )
        VALUES ($1, $2, $3, 'carbon', $4, 1, 'waiting', $5, 'succeeded', $6)
        "#,
    )
    .bind(legacy_message_id)
    .bind(conversation_id)
    .bind(ORGANIZATION_ID)
    .bind(SENDER_ID)
    .bind("historical provider transcript")
    .bind(&legacy_voice_hash)
    .execute(&mut *transaction)
    .await?;
    sqlx::query(
        r#"
        INSERT INTO dm.message_attachments (
            message_id,
            conversation_id,
            organization_id,
            position,
            attachment_kind,
            permanent_url,
            duration_milliseconds
        )
        VALUES ($1, $2, $3, 100, 'voice', $4, NULL)
        "#,
    )
    .bind(legacy_message_id)
    .bind(conversation_id)
    .bind(ORGANIZATION_ID)
    .bind(legacy_voice_url)
    .execute(&mut *transaction)
    .await?;
    sqlx::query(
        r#"
        INSERT INTO dm.actor_deliveries (
            id,
            organization_id,
            target_kind,
            target_id,
            sequence,
            delivery_kind,
            conversation_id,
            message_id
        )
        VALUES ($1, $2, 'silicon', $3, 1, 'message', $4, $5)
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(ORGANIZATION_ID)
    .bind(TARGET_ID)
    .bind(conversation_id)
    .bind(legacy_message_id)
    .execute(&mut *transaction)
    .await?;
    sqlx::query(
        r#"
        INSERT INTO dm.drafts (
            conversation_id,
            organization_id,
            actor_kind,
            actor_id,
            text_content,
            content_hash
        )
        VALUES ($1, $2, 'carbon', $3, NULL, $4)
        "#,
    )
    .bind(conversation_id)
    .bind(ORGANIZATION_ID)
    .bind(SENDER_ID)
    .bind(&legacy_voice_hash)
    .execute(&mut *transaction)
    .await?;
    sqlx::query(
        r#"
        INSERT INTO dm.draft_attachments (
            conversation_id,
            organization_id,
            actor_kind,
            actor_id,
            position,
            attachment_kind,
            permanent_url,
            duration_milliseconds
        )
        VALUES ($1, $2, 'carbon', $3, 100, 'voice', $4, NULL)
        "#,
    )
    .bind(conversation_id)
    .bind(ORGANIZATION_ID)
    .bind(SENDER_ID)
    .bind(legacy_voice_url)
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;

    let historical_event_id = Uuid::now_v7();
    let historical_delivery_id = Uuid::now_v7();
    insert_legacy_system_event(
        store,
        ORGANIZATION_ID,
        TARGET_ID,
        historical_event_id,
        historical_delivery_id,
        "legacy.historical.v1",
    )
    .await?;
    let outstanding_event_id = Uuid::now_v7();
    let outstanding_delivery_id = Uuid::now_v7();
    insert_legacy_system_event(
        store,
        ORGANIZATION_ID,
        TARGET_ID,
        outstanding_event_id,
        outstanding_delivery_id,
        "legacy.outstanding.v1",
    )
    .await?;

    sqlx::query(
        r#"
        UPDATE dm.actor_deliveries
        SET acked_at = transaction_timestamp(),
            retain_until = transaction_timestamp() + interval '30 days'
        WHERE id = $1
        "#,
    )
    .bind(historical_delivery_id)
    .execute(store.pool())
    .await?;
    sqlx::query(
        r#"
        UPDATE dm.actor_deliveries
        SET attempt_count = attempt_count + 1,
            lease_owner = 'legacy-hook-worker',
            lease_started_at = transaction_timestamp(),
            lease_expires_at = transaction_timestamp() + interval '5 minutes',
            last_attempt_at = transaction_timestamp(),
            last_error_code = 'legacy_hook_retry'
        WHERE id = $1
        "#,
    )
    .bind(outstanding_delivery_id)
    .execute(store.pool())
    .await?;

    let historical_before = sqlx::query_as::<
        _,
        (
            Option<OffsetDateTime>,
            Option<OffsetDateTime>,
            Option<OffsetDateTime>,
            Option<String>,
        ),
    >(
        r#"
        SELECT acked_at, retain_until, dead_lettered_at, last_error_code
        FROM dm.actor_deliveries
        WHERE id = $1
        "#,
    )
    .bind(historical_delivery_id)
    .fetch_one(store.pool())
    .await?;

    store.migrate().await?;

    let legacy_hash_versions = sqlx::query_as::<_, (i16, i16)>(
        r#"
        SELECT message.content_hash_version, draft.content_hash_version
        FROM dm.messages AS message
        JOIN dm.drafts AS draft
          ON draft.conversation_id = message.conversation_id
         AND draft.organization_id = message.organization_id
        WHERE message.id = $1
        "#,
    )
    .bind(legacy_message_id)
    .fetch_one(store.pool())
    .await?;
    assert_eq!(legacy_hash_versions, (1, 1));

    let legacy_messages = store
        .list_messages(
            &organization_id,
            &sender,
            conversation_id,
            &PageRequest::default(),
            true,
        )
        .await?;
    let legacy_message = legacy_messages
        .items
        .iter()
        .find(|message| message.id == legacy_message_id)
        .ok_or("legacy voice message was not hydrated")?;
    assert_eq!(
        legacy_message
            .voice
            .as_ref()
            .and_then(|voice| voice.duration_milliseconds),
        None
    );
    let legacy_draft = store
        .get_draft(&organization_id, &sender, conversation_id)
        .await?;
    assert_eq!(
        legacy_draft
            .voice
            .as_ref()
            .and_then(|voice| voice.duration_milliseconds),
        None
    );

    let retained_event_types = sqlx::query_scalar::<_, String>(
        r#"
        SELECT event_type
        FROM dm.system_events
        WHERE event_id IN ($1, $2)
        ORDER BY event_type
        "#,
    )
    .bind(historical_event_id)
    .bind(outstanding_event_id)
    .fetch_all(store.pool())
    .await?;
    assert_eq!(
        retained_event_types,
        vec![
            "legacy.historical.v1".to_owned(),
            "legacy.outstanding.v1".to_owned(),
        ]
    );
    let historical_after = sqlx::query_as::<
        _,
        (
            Option<OffsetDateTime>,
            Option<OffsetDateTime>,
            Option<OffsetDateTime>,
            Option<String>,
        ),
    >(
        r#"
        SELECT acked_at, retain_until, dead_lettered_at, last_error_code
        FROM dm.actor_deliveries
        WHERE id = $1
        "#,
    )
    .bind(historical_delivery_id)
    .fetch_one(store.pool())
    .await?;
    assert_eq!(historical_after, historical_before);

    let retired = sqlx::query_as::<_, (i32, Option<String>, bool, bool, bool, bool)>(
        r#"
        SELECT
            attempt_count,
            last_error_code,
            dead_lettered_at IS NOT NULL,
            lease_owner IS NULL,
            lease_started_at IS NULL,
            lease_expires_at IS NULL
        FROM dm.actor_deliveries
        WHERE id = $1
        "#,
    )
    .bind(outstanding_delivery_id)
    .fetch_one(store.pool())
    .await?;
    assert_eq!(retired.0, 1);
    assert_eq!(retired.1.as_deref(), Some("delivery_kind_retired"));
    assert!(retired.2);
    assert!(retired.3);
    assert!(retired.4);
    assert!(retired.5);

    let replayed = store
        .replay_deliveries(&organization_id, &target, 0, 10)
        .await?;
    assert_eq!(replayed.len(), 1);
    assert!(matches!(
        &replayed[0].payload,
        DeliveryPayload::Message { message } if message.id == legacy_message_id
    ));

    sqlx::query("UPDATE dm.messages SET status = 'sent' WHERE id = $1")
        .bind(legacy_message_id)
        .execute(store.pool())
        .await?;
    let legacy_message_state = sqlx::query_as::<_, (String, String)>(
        r#"
        SELECT status::text, transcription_result::text
        FROM dm.messages
        WHERE id = $1
        "#,
    )
    .bind(legacy_message_id)
    .fetch_one(store.pool())
    .await?;
    assert_eq!(legacy_message_state.0, "sent");
    assert_eq!(legacy_message_state.1, "succeeded");

    let outstanding_sequence =
        sqlx::query_scalar::<_, i64>("SELECT sequence FROM dm.actor_deliveries WHERE id = $1")
            .bind(outstanding_delivery_id)
            .fetch_one(store.pool())
            .await?;
    store
        .acknowledge_deliveries(
            &organization_id,
            &target,
            "legacy-upgrade-consumer",
            outstanding_sequence,
        )
        .await?;
    assert_eq!(
        store
            .acknowledged_through(&organization_id, &target, "legacy-upgrade-consumer")
            .await?,
        outstanding_sequence
    );
    let historical_after_ack = sqlx::query_as::<
        _,
        (
            Option<OffsetDateTime>,
            Option<OffsetDateTime>,
            Option<OffsetDateTime>,
            Option<String>,
        ),
    >(
        r#"
        SELECT acked_at, retain_until, dead_lettered_at, last_error_code
        FROM dm.actor_deliveries
        WHERE id = $1
        "#,
    )
    .bind(historical_delivery_id)
    .fetch_one(store.pool())
    .await?;
    assert_eq!(historical_after_ack, historical_before);

    let mut transaction = store.pool().begin().await?;
    let result = sqlx::query(
        r#"
        INSERT INTO dm.messages (
            id,
            conversation_id,
            organization_id,
            sender_kind,
            sender_id,
            sequence,
            status,
            text_content,
            transcription_result,
            content_hash
        )
        VALUES ($1, $2, $3, 'carbon', $4, 1, 'sent', 'invalid legacy result', 'succeeded', $5)
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(conversation_id)
    .bind(ORGANIZATION_ID)
    .bind(SENDER_ID)
    .bind(vec![1_u8; 32])
    .execute(&mut *transaction)
    .await;
    let Err(error) = result else {
        return Err("v4 accepted a provider transcription result on a new message".into());
    };
    assert_eq!(
        error
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::constraint),
        Some("messages_new_transcription_result_not_applicable")
    );
    transaction.rollback().await?;

    let next_event_id = Uuid::now_v7();
    let mut transaction = store.pool().begin().await?;
    sqlx::query(
        r#"
        INSERT INTO dm.system_events (
            event_id,
            organization_id,
            target_silicon_id,
            event_type,
            payload
        )
        VALUES ($1, $2, $3, 'retired.test.v1', '{}'::jsonb)
        "#,
    )
    .bind(next_event_id)
    .bind(ORGANIZATION_ID)
    .bind(TARGET_ID)
    .execute(&mut *transaction)
    .await?;
    let result = sqlx::query(
        r#"
        INSERT INTO dm.actor_deliveries (
            id,
            organization_id,
            target_kind,
            target_id,
            sequence,
            delivery_kind,
            system_event_id
        )
        VALUES ($1, $2, 'silicon', $3, 1, 'system_event', $4)
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(ORGANIZATION_ID)
    .bind(TARGET_ID)
    .bind(next_event_id)
    .execute(&mut *transaction)
    .await;
    let Err(error) = result else {
        return Err("the retirement constraint accepted a new system-event delivery".into());
    };
    assert_eq!(
        error
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::constraint),
        Some("actor_deliveries_no_new_system_events")
    );
    transaction.rollback().await?;

    let upgraded_voice_message = store
        .send_message(SendMessageCommand {
            organization_id: organization_id.clone(),
            conversation_id,
            sender: sender.clone(),
            content: MessageCreate {
                voice: Some(VoiceAttachment {
                    permanent_url: legacy_voice_url.parse()?,
                    name: None,
                    content_type: None,
                    size: None,
                    duration_milliseconds: Some(42_000),
                }),
                voice_transcript: Some("client-owned replacement transcript".to_owned()),
                ..MessageCreate::default()
            },
            idempotency_key: idempotency_key("message-upgrade-legacy-draft")?,
        })
        .await?;
    assert_eq!(
        sqlx::query_scalar::<_, i16>("SELECT content_hash_version FROM dm.messages WHERE id = $1")
            .bind(upgraded_voice_message.id)
            .fetch_one(store.pool())
            .await?,
        3
    );
    assert!(matches!(
        store
            .get_draft(&organization_id, &sender, conversation_id)
            .await,
        Err(AppError::NotFound)
    ));
    Ok(())
}

fn legacy_voice_content_hash(permanent_url: &str) -> TestResult<Vec<u8>> {
    #[derive(Serialize)]
    struct LegacyVoice<'a> {
        permanent_url: &'a str,
    }

    #[derive(Serialize)]
    struct LegacyContent<'a> {
        text: Option<&'a str>,
        attachments: &'a [()],
        voice: Option<LegacyVoice<'a>>,
        gif: Option<()>,
    }

    let mut hasher = blake3::Hasher::new();
    serde_json::to_writer(
        &mut hasher,
        &LegacyContent {
            text: None,
            attachments: &[],
            voice: Some(LegacyVoice { permanent_url }),
            gif: None,
        },
    )?;
    Ok(hasher.finalize().as_bytes().to_vec())
}

async fn insert_legacy_system_event(
    store: &PostgresStore,
    organization_id: &str,
    target_id: &str,
    event_id: Uuid,
    delivery_id: Uuid,
    event_type: &str,
) -> TestResult<()> {
    let mut transaction = store.pool().begin().await?;
    sqlx::query(
        r#"
        INSERT INTO dm.system_events (
            event_id,
            organization_id,
            target_silicon_id,
            event_type,
            payload
        )
        VALUES ($1, $2, $3, $4, '{}'::jsonb)
        "#,
    )
    .bind(event_id)
    .bind(organization_id)
    .bind(target_id)
    .bind(event_type)
    .execute(&mut *transaction)
    .await?;
    sqlx::query(
        r#"
        INSERT INTO dm.actor_deliveries (
            id,
            organization_id,
            target_kind,
            target_id,
            sequence,
            delivery_kind,
            system_event_id
        )
        VALUES ($1, $2, 'silicon', $3, 1, 'system_event', $4)
        "#,
    )
    .bind(delivery_id)
    .bind(organization_id)
    .bind(target_id)
    .bind(event_id)
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;
    Ok(())
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
    exercise_voice_draft_and_message(&database.store, &scope, conversation_id).await?;
    exercise_concurrent_final_disconnect(&database.store, &scope).await?;

    exercise_isi_routing(&database.store).await?;
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
            idempotency_key: idempotency_key("message-create-primary")?,
        })
        .await?;
    let retry = store
        .send_message(SendMessageCommand {
            organization_id: scope.organization_id.clone(),
            conversation_id,
            sender: scope.sender.clone(),
            content,
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

async fn exercise_voice_draft_and_message(
    store: &PostgresStore,
    scope: &TenantScope,
    conversation_id: Uuid,
) -> TestResult<()> {
    let voice = VoiceAttachment {
        permanent_url: "https://media.example/voice/integration.ogg".parse()?,
        name: Some("integration.ogg".to_owned()),
        content_type: Some("audio/ogg".to_owned()),
        size: Some(8_192),
        duration_milliseconds: Some(42_000),
    };
    let transcript = "client supplied transcript".to_owned();
    let draft_input = DraftInput {
        metadata: serde_json::Map::new(),
        reply_to_message_id: None,
        message_content: None,
        attachments: Vec::new(),
        voice: Some(voice.clone()),
        voice_transcript: Some(transcript.clone()),
        gif: None,
    };
    let saved = store
        .put_draft(PutDraftCommand {
            organization_id: scope.organization_id.clone(),
            conversation_id,
            actor: scope.sender.clone(),
            expected_version: None,
            input: draft_input,
        })
        .await?;
    let PutDraftOutcome::Saved(saved) = saved else {
        return Err("initial voice draft unexpectedly conflicted".into());
    };
    assert_eq!(saved.voice_transcript.as_deref(), Some(transcript.as_str()));
    assert_eq!(
        saved
            .voice
            .as_ref()
            .and_then(|attachment| attachment.duration_milliseconds),
        Some(42_000)
    );

    let message = store
        .send_message(SendMessageCommand {
            organization_id: scope.organization_id.clone(),
            conversation_id,
            sender: scope.sender.clone(),
            content: MessageCreate {
                metadata: serde_json::Map::new(),
                reply_to_message_id: None,
                sender_id: None,
                recipient_id: None,
                text: None,
                attachments: Vec::new(),
                voice: Some(voice),
                voice_transcript: Some(transcript.clone()),
                gif: None,
            },
            idempotency_key: idempotency_key("message-create-voice")?,
        })
        .await?;
    assert_eq!(
        message.voice_transcript.as_deref(),
        Some(transcript.as_str())
    );
    assert_eq!(
        message
            .voice
            .as_ref()
            .and_then(|attachment| attachment.duration_milliseconds),
        Some(42_000)
    );
    assert!(matches!(
        store
            .get_draft(&scope.organization_id, &scope.sender, conversation_id)
            .await,
        Err(AppError::NotFound)
    ));
    Ok(())
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
    // The original message is copied to sender devices before its two receipts.
    // The voice-message exercise runs after this receipt exercise.
    assert_eq!(sender_deliveries.len(), 3);
    assert_eq!(sender_deliveries[0].sequence, 1);
    assert_eq!(sender_deliveries[1].sequence, 2);
    assert_eq!(sender_deliveries[2].sequence, 3);
    assert!(matches!(
        &sender_deliveries[0].payload,
        DeliveryPayload::Message { message: original } if original.id == message.id
    ));
    assert!(matches!(
        &sender_deliveries[1].payload,
        DeliveryPayload::Receipt {
            message_id,
            status: MessageStatus::Delivered,
        } if *message_id == message.id
    ));
    assert!(matches!(
        &sender_deliveries[2].payload,
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

#[allow(
    clippy::too_many_lines,
    reason = "one lifecycle verifies routing through acceptance, replay, edits and bundling in the same conversation"
)]
async fn exercise_isi_routing(store: &PostgresStore) -> TestResult<()> {
    let org: OrganizationId = "isi-org".parse()?;
    let sender = ActorRef {
        actor_type: ActorType::Silicon,
        id: "writer:tos".parse()?,
    };
    let recipient = ActorRef {
        actor_type: ActorType::Silicon,
        id: "cos:tos".parse()?,
    };
    let conversation = store
        .create_conversation(CreateConversationCommand {
            organization_id: org.clone(),
            creator: sender.clone(),
            participants: vec![sender.clone(), recipient.clone()],
            idempotency_key: idempotency_key("isi-conversation")?,
        })
        .await?;
    let content = MessageCreate {
        sender_id: Some("compose@writer:tos".parse()?),
        recipient_id: Some("deliberate@cos:tos".parse()?),
        text: Some("ISI round trip".into()),
        metadata: serde_json::from_value(serde_json::json!({"keep":{"nested":true}}))?,
        ..MessageCreate::default()
    };
    let command = |content: MessageCreate, key: &str| -> TestResult<SendMessageCommand> {
        Ok(SendMessageCommand {
            organization_id: org.clone(),
            conversation_id: conversation.id,
            sender: sender.clone(),
            content,
            idempotency_key: idempotency_key(key)?,
        })
    };
    let message = store
        .send_message(command(content.clone(), "isi-message-key")?)
        .await?;
    assert_eq!(message.sender, sender);
    assert_eq!(message.sender_id, content.sender_id);
    assert_eq!(message.recipient_id, content.recipient_id);
    assert_eq!(message.metadata, content.metadata);
    assert_eq!(
        store
            .send_message(command(content.clone(), "isi-message-key")?)
            .await?
            .id,
        message.id
    );
    let mut changed = content.clone();
    changed.recipient_id = Some("review@cos:tos".parse()?);
    assert!(
        store
            .send_message(command(changed, "isi-message-key")?)
            .await
            .is_err()
    );
    let mut changed = content.clone();
    changed.sender_id = Some("review@writer:tos".parse()?);
    assert!(
        store
            .send_message(command(changed, "isi-message-key")?)
            .await
            .is_err()
    );
    let mut invalid = content.clone();
    invalid.sender_id = Some("compose@cos:tos".parse()?);
    assert!(matches!(
        store
            .send_message(command(invalid, "isi-impersonation")?)
            .await,
        Err(AppError::Forbidden)
    ));
    let mut invalid = content.clone();
    invalid.recipient_id = Some("deliberate@outsider:tos".parse()?);
    assert!(
        store
            .send_message(command(invalid, "isi-outsider")?)
            .await
            .is_err()
    );
    let deliveries = store.replay_deliveries(&org, &recipient, 0, 10).await?;
    assert!(
        matches!(&deliveries[0].payload, DeliveryPayload::Message { message: delivered }
        if delivered.sender_id == content.sender_id && delivered.recipient_id == content.recipient_id)
    );
    let edited = store
        .revise_message(
            &org,
            &sender,
            conversation.id,
            message.id,
            &idempotency_key("isi-edit-message")?,
            Some(MessageCreate {
                text: Some("edited".into()),
                ..MessageCreate::default()
            }),
        )
        .await?;
    assert_eq!(edited.sender_id, content.sender_id);
    assert_eq!(edited.recipient_id, content.recipient_id);
    let history = store
        .list_messages(
            &org,
            &recipient,
            conversation.id,
            &PageRequest::default(),
            false,
        )
        .await?;
    assert_eq!(history.items[0].recipient_id, content.recipient_id);
    let mut reroute = content.clone();
    reroute.recipient_id = Some("other@cos:tos".parse()?);
    assert!(
        store
            .revise_message(
                &org,
                &sender,
                conversation.id,
                message.id,
                &idempotency_key("isi-reroute-edit")?,
                Some(reroute)
            )
            .await
            .is_err()
    );
    let bundle = store
        .create_bundle(silicon_dm::application::commands::CreateBundleCommand {
            organization_id: org.clone(),
            conversation_id: conversation.id,
            creator: sender.clone(),
            bundle: silicon_dm::domain::BundleCreate {
                message_ids: vec![message.id],
                display_message: content.clone(),
            },
            idempotency_key: idempotency_key("isi-bundle-key")?,
        })
        .await?;
    assert_eq!(bundle.display_message.recipient_id, content.recipient_id);
    assert_eq!(bundle.display_message.sender_id, content.sender_id);
    Ok(())
}

fn member(actor_type: ActorType, id: &str) -> TestResult<ActorRef> {
    Ok(ActorRef {
        actor_type,
        id: id.parse()?,
    })
}

#[tokio::test]
async fn automatic_member_chats_are_scoped_idempotent_and_message_free() -> TestResult<()> {
    let database = TestDatabase::start().await?;
    let store = &database.store;
    let org: OrganizationId = format!("auto-{}", Uuid::new_v4()).parse()?;
    let other_org: OrganizationId = format!("other-{}", Uuid::new_v4()).parse()?;
    let actor = member(ActorType::Carbon, "owner")?;
    let tech = member(ActorType::Silicon, "tech:tos")?;
    let colleague = member(ActorType::Carbon, "colleague")?;
    let removed = member(ActorType::Silicon, "removed:tos")?;
    let outsider = member(ActorType::Carbon, "outsider")?;
    let unknown = member(ActorType::Carbon, "undisclosed")?;
    store
        .refresh_directory(
            &org,
            &[
                actor.clone(),
                tech.clone(),
                colleague.clone(),
                removed.clone(),
                unknown.clone(),
            ],
        )
        .await?;
    store
        .refresh_directory(&other_org, std::slice::from_ref(&outsider))
        .await?;
    for (scope, member, status) in [
        (&org, &actor, "active"),
        (&org, &tech, "active"),
        (&org, &colleague, "active"),
        (&org, &removed, "removed"),
        (&other_org, &outsider, "active"),
    ] {
        sqlx::query("INSERT INTO iam_membership_projections(membership_id,principal_id,iam_organization_id,organization_id,actor_kind,actor_id,iam_version,authorization_epoch,status) VALUES($1,$2,$3,$4,$5::text::actor_kind,$6,1,1,$7)")
            .bind(Uuid::new_v4()).bind(Uuid::new_v4()).bind(Uuid::new_v4()).bind(scope.as_str()).bind(member.actor_type.as_str()).bind(member.id.as_str()).bind(status).execute(store.pool()).await?;
    }
    let existing = store
        .create_conversation(CreateConversationCommand {
            organization_id: org.clone(),
            creator: actor.clone(),
            participants: vec![actor.clone(), tech.clone()],
            idempotency_key: Uuid::new_v4().to_string().parse()?,
        })
        .await?;
    let (first, second) = tokio::join!(
        store.initialize_member_conversations(&org, &actor),
        store.initialize_member_conversations(&org, &actor)
    );
    first?;
    second?;
    store.initialize_member_conversations(&org, &actor).await?;
    let chats = store
        .list_conversations(&org, &actor, &PageRequest::default())
        .await?;
    assert_eq!(chats.items.len(), 2);
    assert!(
        chats
            .items
            .iter()
            .all(|c| c.participants.len() == 2 && c.last_message.is_none())
    );
    assert!(
        chats
            .items
            .iter()
            .any(|c| c.participants.contains(&colleague))
    );
    let retained = chats
        .items
        .iter()
        .find(|c| c.id == existing.id)
        .ok_or("existing direct chat missing")?;
    assert_eq!(retained.updated_at, existing.updated_at);
    assert!(
        !chats.items.iter().any(|c| c.participants.contains(&removed)
            || c.participants.contains(&outsider)
            || c.participants.contains(&unknown))
    );
    store
        .initialize_member_conversations(&org, &unknown)
        .await?;
    assert!(
        store
            .list_conversations(&org, &unknown, &PageRequest::default())
            .await?
            .items
            .is_empty()
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM messages WHERE organization_id=$1")
        .bind(org.as_str())
        .fetch_one(store.pool())
        .await?;
    assert_eq!(count, 0);
    Ok(())
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "group policy transitions share one history and durable delivery stream"
)]
async fn groups_apply_current_iam_access_without_rewriting_history() -> TestResult<()> {
    use silicon_dm::application::auth::{AuthContext, PresentedCredential};
    use silicon_dm::domain::GroupSettings;
    use std::collections::BTreeSet;
    let database = TestDatabase::start().await?;
    let store = &database.store;
    sqlx::query("CREATE ROLE group_runtime")
        .execute(store.pool())
        .await?;
    let grants = include_str!("../deploy/runtime-grants.sql")
        .split_once("-- Run as the role")
        .ok_or("grants")?
        .1;
    let grants =
        format!("-- Run as the role{grants}").replace(":\"runtime_role\"", "group_runtime");
    sqlx::raw_sql(sqlx::AssertSqlSafe(grants))
        .execute(store.pool())
        .await?;
    let mut runtime = store.pool().begin().await?;
    sqlx::query("SET LOCAL ROLE group_runtime")
        .execute(&mut *runtime)
        .await?;
    sqlx::query("SELECT dm.sync_group_participants('groups-org'), dm_private.assert_conversation_participant_count($1)").bind(Uuid::nil()).execute(&mut *runtime).await?;
    runtime.commit().await?;
    let org: OrganizationId = "groups-org".parse()?;
    let actors = [
        ActorRef {
            actor_type: ActorType::Carbon,
            id: "owner".parse()?,
        },
        ActorRef {
            actor_type: ActorType::Carbon,
            id: "reader".parse()?,
        },
        ActorRef {
            actor_type: ActorType::Silicon,
            id: "bot:groups-org".parse()?,
        },
        ActorRef {
            actor_type: ActorType::Carbon,
            id: "late".parse()?,
        },
    ];
    let tag = Uuid::new_v4();
    let iam_org = Uuid::new_v4();
    store.refresh_directory(&org, &actors).await?;
    for actor in &actors {
        sqlx::query("INSERT INTO iam_membership_projections(membership_id,principal_id,iam_organization_id,organization_id,actor_kind,actor_id,iam_version,authorization_epoch,status,tag_ids) VALUES($1,$2,$3,$4,$5::text::actor_kind,$6,1,1,'active',$7)")
            .bind(Uuid::new_v4()).bind(Uuid::new_v4()).bind(iam_org).bind(org.as_str()).bind(actor.actor_type.as_str()).bind(actor.id.as_str()).bind(vec![tag]).execute(store.pool()).await?;
    }
    let authority = |actor: ActorRef, admin: bool, tags: Option<BTreeSet<Uuid>>| AuthContext {
        actor,

        session_id: None,
        organization_id: org.clone(),
        org_role: admin.then(|| "org_admin".into()),
        tag_ids: tags,
        represented_actor_ids: BTreeSet::new(),
        capabilities: BTreeSet::new(),
        credential: PresentedCredential::Bearer(SecretString::from("test-token")),
    };
    let owner = authority(actors[0].clone(), true, None);
    let reader = authority(actors[1].clone(), false, Some(BTreeSet::from([tag])));
    let bot = authority(actors[2].clone(), false, Some(BTreeSet::from([tag])));
    let settings = GroupSettings {
        name: "Research".into(),
        description: "Shared history".into(),
        is_public: false,
        tag_ids: vec![],
    };
    assert!(matches!(
        store
            .create_group(
                &reader,
                settings.clone(),
                vec![],
                &idempotency_key("denied-create")?
            )
            .await,
        Err(AppError::Forbidden)
    ));
    let group = store
        .create_group(
            &owner,
            settings.clone(),
            vec![actors[1].clone()],
            &idempotency_key("group-create")?,
        )
        .await?;
    assert_eq!(
        group.group.as_ref().ok_or("group")?.settings.name,
        "Research"
    );
    let retry = store
        .create_group(
            &owner,
            settings.clone(),
            vec![actors[1].clone()],
            &idempotency_key("group-create")?,
        )
        .await?;
    assert_eq!(retry.id, group.id);
    let second = store
        .create_group(
            &owner,
            GroupSettings {
                name: "Another research group".into(),
                ..settings.clone()
            },
            vec![actors[1].clone()],
            &idempotency_key("another-group")?,
        )
        .await?;
    assert_ne!(
        second.id, group.id,
        "groups with identical rosters remain distinct"
    );
    assert!(store.check_group_access(&bot, group.id).await.is_err());
    let message = store
        .send_message(SendMessageCommand {
            organization_id: org.clone(),
            conversation_id: group.id,
            sender: owner.actor.clone(),
            content: MessageCreate {
                text: Some("Before you joined".into()),
                ..MessageCreate::default()
            },
            idempotency_key: idempotency_key("group-message")?,
        })
        .await?;
    store
        .change_group_members(
            &owner,
            group.id,
            vec![actors[3].clone()],
            false,
            &idempotency_key("invite-late")?,
        )
        .await?;
    let history = store
        .list_messages(&org, &actors[3], group.id, &PageRequest::default(), false)
        .await?;
    assert_eq!(history.items[0].id, message.id);
    assert!(
        store
            .replay_deliveries(&org, &actors[3], 0, 10)
            .await?
            .is_empty(),
        "joining grants history without backfilling old transport deliveries"
    );
    store
        .record_receipt_without_payload(RecordReceiptCommand {
            organization_id: org.clone(),
            conversation_id: group.id,
            message_id: message.id,
            recipient: reader.actor.clone(),
            device_id: "reader-device".into(),
            status: ReceiptStatus::Read,
        })
        .await?;
    assert_eq!(
        store
            .get_message(&org, &owner.actor, group.id, message.id)
            .await?
            .status,
        MessageStatus::Read,
        "late members do not hold old receipts open"
    );
    store
        .change_group_members(
            &owner,
            group.id,
            vec![reader.actor.clone()],
            true,
            &idempotency_key("uninvite-reader")?,
        )
        .await?;
    assert!(
        store
            .require_participant(&org, &reader.actor, group.id)
            .await
            .is_err()
    );
    assert!(
        store
            .replay_deliveries(&org, &reader.actor, 0, 10)
            .await?
            .is_empty()
    );
    assert!(
        store
            .list_messages(
                &org,
                &reader.actor,
                group.id,
                &PageRequest::default(),
                false
            )
            .await
            .is_err()
    );
    assert!(
        store
            .get_draft(&org, &reader.actor, group.id)
            .await
            .is_err()
    );
    let current = store.group_details(&org, group.id).await?.ok_or("group")?;
    let tagged = GroupSettings {
        tag_ids: vec![tag],
        ..settings.clone()
    };
    store
        .update_group(
            &owner,
            group.id,
            tagged.clone(),
            current.version,
            &idempotency_key("enable-tags")?,
        )
        .await?;
    store.check_group_access(&reader, group.id).await?;
    store.check_group_access(&bot, group.id).await?;
    store
        .require_participant(&org, &bot.actor, group.id)
        .await?;
    let narrow = authority(reader.actor.clone(), false, None);
    assert!(
        store.check_group_access(&narrow, group.id).await.is_err(),
        "undisclosed tags cannot inherit broader cached token permissions"
    );
    assert!(
        store
            .list_conversations_scoped(&org, &reader.actor, &PageRequest::default(), &[], true)
            .await?
            .items
            .iter()
            .all(|c| c.id != group.id)
    );
    let (membership, principal):(Uuid,Uuid)=sqlx::query_as("SELECT membership_id,principal_id FROM iam_membership_projections WHERE organization_id=$1 AND actor_id=$2").bind(org.as_str()).bind(reader.actor.id.as_str()).fetch_one(store.pool()).await?;
    let event = |version: i64, tags: serde_json::Value| silicon_iam_client::models::WebhookEvent {
        spec_version: serde_json::json!(1),
        event_id: Uuid::new_v4(),
        event_type: "organization.membership.updated.v1".into(),
        occurred_at: OffsetDateTime::now_utc(),
        organization_id: Some(iam_org),
        aggregate: serde_json::json!({}),
        data: serde_json::json!({"current":{"members":[{"resource":{"type":"organization_membership","id":membership,"principal_id":principal,"principal_type":"carbon","status":"active","version":version},"principal":{"public_id":reader.actor.id},"organization":{"id":iam_org,"org_id":org},"membership":{"authorization_epoch":version,"tags":tags},"authorization":"active"}]}}),
    };
    let mut tx = store.pool().begin().await?;
    PostgresStore::project_iam_webhook(&mut tx, &event(1, serde_json::Value::Null)).await?;
    tx.commit().await?;
    store
        .require_participant(&org, &reader.actor, group.id)
        .await?;
    // A newer undisclosed projection invalidates the old tags; explicit fresh
    // disclosure restores them and an empty list revokes them immediately.
    for (version, tags, allowed) in [
        (2, serde_json::Value::Null, false),
        (
            3,
            serde_json::json!([{"id":tag,"name":"Engineering"}]),
            true,
        ),
        (4, serde_json::json!([]), false),
    ] {
        let mut tx = store.pool().begin().await?;
        PostgresStore::project_iam_webhook(&mut tx, &event(version, tags)).await?;
        tx.commit().await?;
        assert_eq!(
            store
                .require_participant(&org, &reader.actor, group.id)
                .await
                .is_ok(),
            allowed
        );
    }

    assert!(
        store
            .require_participant(&org, &reader.actor, group.id)
            .await
            .is_err(),
        "tag removal revokes historical roster access"
    );
    let current = store.group_details(&org, group.id).await?.ok_or("group")?;
    store
        .update_group(
            &owner,
            group.id,
            GroupSettings {
                is_public: true,
                ..tagged.clone()
            },
            current.version,
            &idempotency_key("enable-public")?,
        )
        .await?;
    store
        .require_participant(&org, &reader.actor, group.id)
        .await?;
    assert!(
        store.check_group_access(&bot, group.id).await.is_err(),
        "public Silicons require explicit invitation, even with matching tags"
    );
    assert!(
        store
            .require_participant(&org, &bot.actor, group.id)
            .await
            .is_err()
    );
    store
        .change_group_members(
            &owner,
            group.id,
            vec![bot.actor.clone()],
            false,
            &idempotency_key("invite-bot")?,
        )
        .await?;
    store.check_group_access(&bot, group.id).await?;
    store
        .require_participant(&org, &bot.actor, group.id)
        .await?;
    assert!(
        store
            .update_group(
                &owner,
                group.id,
                tagged,
                1,
                &idempotency_key("stale-policy")?
            )
            .await
            .is_err()
    );
    let mut cross_org = owner.clone();
    cross_org.organization_id = "other-org".parse()?;
    assert!(
        store
            .check_group_access(&cross_org, group.id)
            .await
            .is_err()
    );
    assert!(
        store
            .change_group_members(
                &cross_org,
                group.id,
                vec![reader.actor.clone()],
                false,
                &idempotency_key("cross-org-invite")?
            )
            .await
            .is_err()
    );
    sqlx::query("UPDATE iam_membership_projections SET status='removed' WHERE organization_id=$1 AND actor_id=$2").bind(org.as_str()).bind(bot.actor.id.as_str()).execute(store.pool()).await?;
    assert!(
        store
            .require_participant(&org, &bot.actor, group.id)
            .await
            .is_err(),
        "org departure overrides explicit invitation"
    );
    Ok(())
}
