//! Upgrade coverage for pre-address groups, duplicate names, and immutable identities.
use secrecy::SecretString;
use silicon_dm::{
    application::auth::{AuthContext, PresentedCredential},
    config::DatabaseSettings,
    domain::{ActorRef, ActorType, GroupSettings, OrganizationId},
    infrastructure::postgres::PostgresStore,
};
use std::{collections::BTreeSet, num::NonZeroU32, time::Duration};
use testcontainers::{ImageExt as _, runners::AsyncRunner as _};
use testcontainers_modules::postgres::Postgres;
use uuid::Uuid;
type Result = std::result::Result<(), Box<dyn std::error::Error + Send + Sync>>;

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "the chronological upgrade fixture keeps pre-migration data and post-migration identity assertions together"
)]
async fn upgrade_keeps_existing_groups_and_assigns_unique_stable_addresses() -> Result {
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
        .run_to(20, &mut *connection)
        .await?;
    sqlx::query("SET search_path=dm")
        .execute(&mut *connection)
        .await?;
    drop(connection);
    let org: OrganizationId = "tos".parse()?;
    let actor = ActorRef {
        actor_type: ActorType::Carbon,
        id: "c:alice".parse()?,
    };
    store
        .refresh_directory(&org, std::slice::from_ref(&actor))
        .await?;
    sqlx::query("INSERT INTO iam_membership_projections(membership_id,principal_id,iam_organization_id,organization_id,actor_kind,actor_id,iam_version,authorization_epoch,status) VALUES($1,$2,$3,'tos','carbon','c:alice',1,1,'active')")
        .bind(Uuid::new_v4()).bind(Uuid::new_v4()).bind(Uuid::new_v4()).execute(store.pool()).await?;
    let auth = AuthContext {
        actor,

        session_id: None,
        organization_id: org.clone(),
        org_role: Some("org_owner".into()),
        tag_ids: None,
        represented_actor_ids: BTreeSet::new(),
        capabilities: BTreeSet::new(),
        credential: PresentedCredential::Bearer(SecretString::from("fixture")),
        credential_expires_at: time::OffsetDateTime::now_utc() + time::Duration::hours(1),
    };
    let mut ids = Vec::new();
    for (i, name) in [
        "Product Design",
        "Product---Design!",
        "Product Design 2",
        "你好",
    ]
    .iter()
    .enumerate()
    {
        let group = store
            .create_group(
                &auth,
                GroupSettings {
                    name: (*name).into(),
                    description: String::new(),
                    is_public: false,
                    tag_ids: vec![],
                },
                vec![],
                &format!("legacy-group-{i}").parse()?,
            )
            .await?;
        ids.push(group.id);
    }
    store.migrate().await?;
    let rows: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT conversation_id,public_id FROM groups ORDER BY conversation_id")
            .fetch_all(store.pool())
            .await?;
    assert_eq!(rows.len(), 4);
    assert_eq!(rows.iter().map(|r| r.0).collect::<Vec<_>>(), ids);
    assert_eq!(
        rows.iter().map(|r| r.1.as_str()).collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "g:tos:product-design",
            "g:tos:product-design-2",
            "g:tos:product-design-2-2",
            "g:tos:group"
        ])
    );
    store
        .update_group(
            &auth,
            ids[0],
            GroupSettings {
                name: "Brand new name".into(),
                description: String::new(),
                is_public: false,
                tag_ids: vec![],
            },
            1,
            &"rename-after-upgrade".parse()?,
        )
        .await?;
    let public: String =
        sqlx::query_scalar("SELECT public_id FROM groups WHERE conversation_id=$1")
            .bind(ids[0])
            .fetch_one(store.pool())
            .await?;
    assert_eq!(public, "g:tos:product-design");
    assert!(
        sqlx::query("UPDATE groups SET public_id='g:tos:changed' WHERE conversation_id=$1")
            .bind(ids[0])
            .execute(store.pool())
            .await
            .is_err()
    );
    let before: i64 = sqlx::query_scalar("SELECT count(*) FROM conversations")
        .fetch_one(store.pool())
        .await?;
    assert!(
        store
            .create_group(
                &auth,
                GroupSettings {
                    name: "PRODUCT design".into(),
                    description: String::new(),
                    is_public: false,
                    tag_ids: vec![]
                },
                vec![],
                &"duplicate-after-upgrade".parse()?
            )
            .await
            .is_err()
    );
    let after: i64 = sqlx::query_scalar("SELECT count(*) FROM conversations")
        .fetch_one(store.pool())
        .await?;
    assert_eq!(
        after, before,
        "failed duplicate creation leaves no orphan conversation"
    );
    for (name, slug) in [
        ("  Product  DESIGN!!  ", "product-design"),
        ("a___b", "a-b"),
        ("123", "123"),
        ("!!!", ""),
    ] {
        let actual: String = sqlx::query_scalar("SELECT group_slug($1)")
            .bind(name)
            .fetch_one(store.pool())
            .await?;
        assert_eq!(actual, slug);
    }
    Ok(())
}
