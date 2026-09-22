//! Local-PostgreSQL coverage for HTTP catch-up and transport-independent presence.

mod support;

use std::{collections::BTreeSet, time::Duration};

use secrecy::SecretString;
use silicon_dm::{
    application::{
        auth::{AuthContext, PresentedCredential},
        commands::{CreateConversationCommand, SendMessageCommand},
    },
    config::DatabaseSettings,
    domain::{Activity, ActorRef, ActorType, Availability, GroupSettings, MessageCreate},
    infrastructure::postgres::PostgresStore,
};
use uuid::Uuid;

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

fn authority(org: &str, actor: &str) -> Result<AuthContext> {
    Ok(AuthContext {
        organization_id: org.parse()?,
        actor: ActorRef {
            actor_type: ActorType::Carbon,
            id: actor.parse()?,
        },
        session_id: None,
        org_role: Some("org_admin".into()),
        tag_ids: None,
        represented_actor_ids: BTreeSet::new(),
        capabilities: BTreeSet::new(),
        credential: PresentedCredential::Bearer(SecretString::from("not-persisted")),
        credential_expires_at: time::OffsetDateTime::now_utc() + time::Duration::hours(1),
    })
}

async fn message(store: &PostgresStore, auth: &AuthContext, conversation: Uuid) -> Result {
    store
        .send_message(SendMessageCommand {
            organization_id: auth.organization_id.clone(),
            conversation_id: conversation,
            sender: auth.actor.clone(),
            content: MessageCreate {
                text: Some("content must not enter a reference response".into()),
                metadata: serde_json::from_value(serde_json::json!({"secret":"private"}))?,
                ..MessageCreate::default()
            },
            idempotency_key: Uuid::new_v4().to_string().parse()?,
        })
        .await?;
    Ok(())
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one isolated database exercises the complete sync and lease lifecycle"
)]
async fn references_preserve_boundaries_permissions_and_retention_while_presence_uses_http()
-> Result {
    let fixture = support::TestDatabase::start().await?;
    let store = PostgresStore::connect(&DatabaseSettings {
        url: SecretString::from(fixture.url.clone()),
        max_connections: 4.try_into()?,
        min_connections: 1,
        acquire_timeout: Duration::from_secs(10),
        statement_timeout: Duration::from_secs(30),
    })
    .await?;
    store.migrate().await?;
    let org = format!("sync_{}", Uuid::new_v4().simple());
    let alice = authority(&org, "alice")?;
    let bob = authority(&org, "bob")?;
    let mallory = authority(&org, "mallory")?;
    let outsider = authority("other_org", "bob")?;
    let chat = store
        .create_conversation(CreateConversationCommand {
            organization_id: alice.organization_id.clone(),
            creator: alice.actor.clone(),
            participants: vec![alice.actor.clone(), bob.actor.clone()],
            idempotency_key: Uuid::new_v4().to_string().parse()?,
        })
        .await?;
    assert_eq!(store.sync_head(&bob).await?, 0);
    message(&store, &alice, chat.id).await?;
    message(&store, &alice, chat.id).await?;
    let upper = store.sync_head(&bob).await?;
    assert_eq!(upper, 2);
    let first = store.sync_events(&bob, 0, upper, 1).await?;
    assert_eq!(first.events.len(), 1);
    assert_eq!(first.position, 1);
    assert!(first.has_more);
    assert_eq!(first.events[0].conversation_id, "alice::bob");
    assert_eq!(first.events[0].message_id, "000");
    let serialized = serde_json::to_string(&first.events)?;
    assert!(!serialized.contains("content must not"));
    assert!(!serialized.contains("private"));
    assert!(!serialized.contains(&chat.id.to_string()));

    // A newer committed event cannot move a fixed pagination boundary.
    message(&store, &alice, chat.id).await?;
    let last = store.sync_events(&bob, first.position, upper, 1).await?;
    assert_eq!(last.position, 2);
    assert!(!last.has_more);
    let new_head = store.sync_head(&bob).await?;
    let later = store
        .sync_events(&bob, last.position, new_head, 100)
        .await?;
    assert_eq!(later.events.len(), 1);
    assert_eq!(later.position, 3);
    assert_eq!(store.sync_head(&mallory).await?, 0);
    assert_eq!(store.sync_head(&outsider).await?, 0);

    for actor in [&alice.actor, &bob.actor] {
        sqlx::query("INSERT INTO iam_membership_projections(membership_id,iam_organization_id,organization_id,actor_kind,actor_id,iam_version,authorization_epoch,status) VALUES($1,$2,$3,'carbon',$4,1,1,'active')")
            .bind(Uuid::new_v4()).bind(Uuid::new_v4()).bind(&org).bind(actor.id.as_str())
            .execute(store.pool()).await?;
    }

    // Access may disappear after the event was queued. Scan the position without
    // exposing the reference, and do not confuse that filtered row with a gap.
    let tag = Uuid::new_v4();
    sqlx::query("UPDATE iam_membership_projections SET tag_ids=$1 WHERE organization_id=$2 AND actor_id='bob'")
        .bind(vec![tag]).bind(&org).execute(store.pool()).await?;
    let group = store
        .create_group(
            &alice,
            GroupSettings {
                name: "Restricted".into(),
                description: String::new(),
                is_public: false,
                tag_ids: vec![tag],
            },
            vec![bob.actor.clone()],
            &Uuid::new_v4().to_string().parse()?,
        )
        .await?;
    message(&store, &alice, group.id).await?;
    let group_head = store.sync_head(&bob).await?;
    store
        .change_group_members(
            &alice,
            group.id,
            vec![bob.actor.clone()],
            true,
            &Uuid::new_v4().to_string().parse()?,
        )
        .await?;
    let filtered = store.sync_events(&bob, new_head, group_head, 100).await?;
    assert!(filtered.events.is_empty());
    assert_eq!(filtered.position, group_head);
    assert!(!filtered.has_more);
    // Broader cached IAM tags still make Bob eligible, but do not authorize a
    // token whose disclosed tag set is empty. Only that exact token can opt in.
    let mut tagged_bob = bob.clone();
    tagged_bob.tag_ids = Some(BTreeSet::from([tag]));
    assert_eq!(
        store
            .sync_events(&tagged_bob, new_head, group_head, 100)
            .await?
            .events
            .len(),
        1
    );

    // Exercise real retention deletion, with a short retention interval only in
    // this owned test database. Missing source rows must never silently advance.
    sqlx::query("UPDATE actor_deliveries SET acked_at=clock_timestamp(), retain_until=clock_timestamp()+interval '1 millisecond' WHERE organization_id=$1 AND target_id='bob' AND sequence=2")
        .bind(&org).execute(store.pool()).await?;
    sqlx::query("SELECT pg_sleep(0.01)")
        .execute(store.pool())
        .await?;
    store.compact_delivery_state().await?;
    let gap = store.sync_events(&bob, 1, group_head, 100).await;
    assert_eq!(
        gap.err().map(|error| error.code()),
        Some("sync_reset_required")
    );
    assert!(store.sync_events(&bob, -1, 3, 50).await.is_err());
    assert!(store.sync_events(&bob, 0, 3, 101).await.is_err());

    // An HTTP lease makes a device online without any DM or Ting WebSocket.
    let lease = store
        .renew_http_presence(
            &bob,
            "laptop",
            Some(Activity::Typing),
            Duration::from_secs(60),
            Duration::from_millis(10),
        )
        .await?;
    assert_eq!(lease.presence.availability, Availability::Online);
    assert_eq!(lease.presence.activity, Some(Activity::Typing));
    assert!(lease.activity_expires_at.is_some());
    sqlx::query("SELECT pg_sleep(0.02)")
        .execute(store.pool())
        .await?;
    let idle = store
        .get_presence(&bob.organization_id, &bob.actor, &bob.actor.id)
        .await?;
    assert_eq!(idle.availability, Availability::Online);
    assert_eq!(idle.activity, None);

    store
        .renew_http_presence(
            &bob,
            "phone",
            None,
            Duration::from_secs(60),
            Duration::from_secs(10),
        )
        .await?;
    store.close_http_presence(&mallory, "phone").await?;
    store.close_http_presence(&outsider, "phone").await?;
    store.close_http_presence(&bob, "laptop").await?;
    let still_online = store
        .get_presence(&bob.organization_id, &bob.actor, &bob.actor.id)
        .await?;
    assert_eq!(still_online.availability, Availability::Online);
    assert_eq!(
        still_online.last_seen_at, None,
        "closing one of two devices is not an offline transition"
    );
    store.close_http_presence(&bob, "phone").await?;
    store.close_http_presence(&bob, "phone").await?;
    let offline = store
        .get_presence(&bob.organization_id, &bob.actor, &bob.actor.id)
        .await?;
    assert_eq!(offline.availability, Availability::Offline);
    assert!(offline.last_seen_at.is_some());

    // Reopening a closed lease is safe and expiry works without a sweeper.
    store
        .renew_http_presence(
            &bob,
            "phone",
            None,
            Duration::from_millis(5),
            Duration::from_millis(5),
        )
        .await?;
    sqlx::query("SELECT pg_sleep(0.01)")
        .execute(store.pool())
        .await?;
    assert_eq!(
        store
            .get_presence(&bob.organization_id, &bob.actor, &bob.actor.id)
            .await?
            .availability,
        Availability::Offline
    );
    assert!(
        store
            .renew_http_presence(
                &bob,
                "",
                None,
                Duration::from_secs(30),
                Duration::from_secs(10)
            )
            .await
            .is_err()
    );
    assert!(
        store
            .renew_http_presence(&bob, "valid", None, Duration::ZERO, Duration::from_secs(10))
            .await
            .is_err()
    );
    let credentials: i64 = sqlx::query_scalar("SELECT count(*) FROM client_presence_leases WHERE row_to_json(client_presence_leases)::text LIKE '%not-persisted%'")
        .fetch_one(store.pool()).await?;
    assert_eq!(credentials, 0);
    store.pool().close().await;
    Ok(())
}
