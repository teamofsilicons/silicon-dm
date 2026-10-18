//! The Accounts schema migrations apply to an empty database and upgrade a 0.14.1
//! database (migrations 0001–0038 plus a dataset written by the 0.14.1 code) without
//! losing or rewriting a single existing row.
#![allow(
    clippy::too_many_lines,
    reason = "each test is one scenario, read top to bottom"
)]
mod support;

use std::collections::BTreeMap;

use sqlx::{Connection as _, PgConnection, migrate::Migrator};
use support::{BoxError, TestDatabase};

static MIGRATOR: Migrator = sqlx::migrate!("./migrations");
const LEGACY_FIXTURE: &str = include_str!("fixtures/legacy-0038.sql");

/// Tables whose rows the upgrade must keep byte for byte (counted and checksummed).
const PRESERVED: &[&str] = &[
    "actor_deliveries",
    "actor_delivery_streams",
    "actor_snapshots",
    "client_presence_leases",
    "conversation_participants",
    "conversations",
    "draft_version_counters",
    "drafts",
    "group_invitations",
    "groups",
    "iam_membership_projections",
    "idempotency_records",
    "message_attachments",
    "message_bundle_items",
    "message_bundles",
    "message_gifs",
    "message_history",
    "message_receipts",
    "messages",
    "recent_gifs",
    "ting_handoffs",
];

async fn connect(url: &str) -> Result<PgConnection, BoxError> {
    let mut connection = PgConnection::connect(url).await?;
    // The migration journal always lives in public (see PostgresStore::migrate).
    sqlx::query("SET search_path=public")
        .execute(&mut connection)
        .await?;
    Ok(connection)
}

/// Row count and an order-independent digest of the original columns of a table.
async fn fingerprint(
    connection: &mut PgConnection,
    table: &str,
    columns: &str,
) -> Result<(i64, String), BoxError> {
    let row: (i64, Option<String>) = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT count(*), md5(string_agg(md5(row_text), '' ORDER BY md5(row_text))) \
         FROM (SELECT ({columns})::text AS row_text FROM dm.{table} t) rows"
    )))
    .fetch_one(&mut *connection)
    .await?;
    Ok((row.0, row.1.unwrap_or_default()))
}

/// The digest of every row of a table over the given (legacy) columns.
async fn digests(
    connection: &mut PgConnection,
    table: &str,
    columns: &str,
) -> Result<std::collections::BTreeSet<String>, BoxError> {
    let rows: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT md5(({columns})::text) FROM dm.{table} t"
    )))
    .fetch_all(&mut *connection)
    .await?;
    Ok(rows.into_iter().collect())
}

async fn legacy_columns(connection: &mut PgConnection, table: &str) -> Result<String, BoxError> {
    let columns: Vec<String> = sqlx::query_scalar(
        "SELECT format('t.%I', column_name) FROM information_schema.columns \
         WHERE table_schema = 'dm' AND table_name = $1 ORDER BY ordinal_position",
    )
    .bind(table)
    .fetch_all(&mut *connection)
    .await?;
    Ok(format!("ROW({})", columns.join(",")))
}

/// A migration may read a whole table (0043's backfill reads every message): `dm-migrate`
/// runs it without the services' statement deadline, which cancelled 0043 on a database of a
/// million messages with nothing but "internal server error" to show for it.
#[tokio::test]
async fn migrations_run_without_the_request_statement_deadline() -> Result<(), BoxError> {
    let database = TestDatabase::start().await?;
    let settings = silicon_dm::config::DatabaseSettings {
        url: secrecy::SecretString::from(database.url.clone()),
        max_connections: std::num::NonZeroU32::new(2).ok_or("pool size")?,
        min_connections: 0,
        acquire_timeout: std::time::Duration::from_secs(10),
        // Far shorter than the longest migration statement.
        statement_timeout: std::time::Duration::from_millis(2),
    };
    let store = silicon_dm::infrastructure::postgres::PostgresStore::connect(&settings).await?;
    store.migrate().await?;
    let mut connection = store.pool().acquire().await?;
    let timeout: String = sqlx::query_scalar("SHOW statement_timeout")
        .fetch_one(&mut *connection)
        .await?;
    assert_eq!(
        timeout, "2ms",
        "the pooled connection gets its deadline back"
    );
    drop(connection);
    store.pool().close().await;
    database.close().await
}

#[tokio::test]
async fn empty_database_applies_every_migration() -> Result<(), BoxError> {
    let database = TestDatabase::start().await?;
    let mut connection = connect(&database.url).await?;
    MIGRATOR.run(&mut connection).await?;
    let partition: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM dm.organization_snapshots WHERE organization_id = '@accounts')",
    )
    .fetch_one(&mut connection)
    .await?;
    assert!(partition, "the @accounts partition exists from the start");
    for relation in [
        "dm.accounts",
        "dm.identity_links",
        "dm.identity_link_runs",
        "dm.accounts_webhook_receipts",
        "dm.group_owners",
        "dm.silicon_contact_allowances",
        "dm.ting_proof_grants",
        "dm.conversations_canonical_direct_idx",
    ] {
        let exists: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
            .bind(relation)
            .fetch_one(&mut connection)
            .await?;
        assert!(exists, "{relation} exists");
    }
    // uuids are case-sensitive text: two accounts differing only in case are different rows.
    sqlx::query("INSERT INTO dm.accounts (uuid, kind, current_id) VALUES ('a8K', 'carbon', 'c:ada'), ('A8k', 'silicon', 'si:scout')")
        .execute(&mut connection)
        .await?;
    let rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM dm.accounts WHERE uuid IN ('a8K', 'A8k')")
            .fetch_one(&mut connection)
            .await?;
    assert_eq!(rows, 2);
    // A uuid is never mistaken for a public id and vice versa.
    assert!(
        sqlx::query("INSERT INTO dm.accounts (uuid, kind) VALUES ('c:ada', 'carbon')")
            .execute(&mut connection)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("INSERT INTO dm.accounts (uuid, kind, current_id) VALUES ('zQo', 'carbon', 'si:mismatch')")
            .execute(&mut connection)
            .await
            .is_err(),
        "kind and id prefix must agree"
    );
    connection.close().await?;
    database.close().await
}

#[tokio::test]
async fn upgrade_from_0038_keeps_every_legacy_row() -> Result<(), BoxError> {
    let database = TestDatabase::start().await?;
    let mut connection = connect(&database.url).await?;
    MIGRATOR.run_to(38, &mut connection).await?;
    sqlx::raw_sql(LEGACY_FIXTURE)
        .execute(&mut connection)
        .await?;
    sqlx::query("SET search_path=public")
        .execute(&mut connection)
        .await?;

    let mut before = BTreeMap::new();
    let mut row_digests = BTreeMap::new();
    for table in PRESERVED {
        let columns = legacy_columns(&mut connection, table).await?;
        row_digests.insert(*table, digests(&mut connection, table, &columns).await?);
        before.insert(
            *table,
            (
                columns.clone(),
                fingerprint(&mut connection, table, &columns).await?,
            ),
        );
    }
    let conversations_before: Vec<(uuid::Uuid, time::OffsetDateTime)> =
        sqlx::query_as("SELECT id, updated_at FROM dm.conversations ORDER BY id")
            .fetch_all(&mut connection)
            .await?;
    assert!(before["messages"].1.0 > 10, "the fixture is loaded");

    MIGRATOR.run(&mut connection).await?;

    // Every legacy row keeps its original column values (new columns are additive).
    for (table, (columns, fingerprint_before)) in &before {
        let after = fingerprint(&mut connection, table, columns).await?;
        if *table == "group_invitations" || *table == "conversation_participants" {
            // Public and tag access is materialised as extra memberships; nothing is removed
            // or changed.
            let now = digests(&mut connection, table, columns).await?;
            let missing: Vec<_> = row_digests[table].difference(&now).collect();
            assert!(
                missing.is_empty(),
                "{table} lost or changed rows: {missing:?}"
            );
        } else {
            assert_eq!(
                &after, fingerprint_before,
                "{table} changed during the upgrade"
            );
        }
    }
    let conversations_after: Vec<(uuid::Uuid, time::OffsetDateTime)> =
        sqlx::query_as("SELECT id, updated_at FROM dm.conversations ORDER BY id")
            .fetch_all(&mut connection)
            .await?;
    assert_eq!(
        conversations_after, conversations_before,
        "activity times are untouched by supersession marking"
    );

    // The c:saket/c:ada pair talked in tos and in bricks; bricks is more recent and canonical.
    let duplicates: Vec<(String, Option<uuid::Uuid>, uuid::Uuid)> = sqlx::query_as(
        "SELECT c.organization_id, c.superseded_by, c.id FROM dm.conversations c \
         WHERE NOT c.is_group AND c.participant_set @> '[{\"id\":\"c:saket\"},{\"id\":\"c:ada\"}]' \
         ORDER BY c.organization_id",
    )
    .fetch_all(&mut connection)
    .await?;
    assert_eq!(duplicates.len(), 2);
    let bricks = &duplicates[0];
    let tos = &duplicates[1];
    assert_eq!((bricks.0.as_str(), bricks.1), ("bricks", None));
    assert_eq!((tos.0.as_str(), tos.1), ("tos", Some(bricks.2)));

    // Group creators are the first owners; existing invitations are accepted memberships.
    let owners: Vec<(String, String)> = sqlx::query_as(
        "SELECT g.public_id, o.actor_id FROM dm.group_owners o JOIN dm.groups g USING (conversation_id) ORDER BY 1",
    )
    .fetch_all(&mut connection)
    .await?;
    assert_eq!(
        owners,
        vec![
            ("g:tos:all-hands".to_owned(), "c:saket".to_owned()),
            ("g:tos:product-design".to_owned(), "c:saket".to_owned()),
            ("g:tos:tagged".to_owned(), "c:saket".to_owned()),
        ]
    );
    let pending: i64 =
        sqlx::query_scalar("SELECT count(*) FROM dm.group_invitations WHERE status <> 'accepted'")
            .fetch_one(&mut connection)
            .await?;
    assert_eq!(pending, 0);
    // Public (every org Carbon) and IAM-tag access became accepted memberships; explicit
    // invitations stayed. Nobody gained or lost access.
    let members: Vec<(String, Vec<String>)> = sqlx::query_as(
        "SELECT g.public_id, array_agg(e.actor_id ORDER BY e.actor_id) \
         FROM dm.eligible_group_members e JOIN dm.groups g USING (conversation_id) \
         GROUP BY g.public_id ORDER BY g.public_id",
    )
    .fetch_all(&mut connection)
    .await?;
    let strings = |ids: &[&str]| ids.iter().map(|id| (*id).to_owned()).collect::<Vec<_>>();
    assert_eq!(
        members,
        vec![
            (
                "g:tos:all-hands".to_owned(),
                strings(&["c:ada", "c:old-timer", "c:saket"])
            ),
            (
                "g:tos:product-design".to_owned(),
                strings(&["c:saket", "si:cos"])
            ),
            ("g:tos:tagged".to_owned(), strings(&["c:ada", "c:saket"])),
        ]
    );
    let roster_gaps: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM dm.eligible_group_members e WHERE NOT EXISTS (
            SELECT 1 FROM dm.conversation_participants p
            WHERE p.conversation_id = e.conversation_id AND p.actor_kind = e.actor_kind
              AND p.actor_id = e.actor_id)",
    )
    .fetch_one(&mut connection)
    .await?;
    assert_eq!(roster_gaps, 0, "every member has its roster anchor");

    // Addresses: the canonical pair keeps `c:ada::c:saket`; the superseded copy is reachable
    // by UUID only; legacy ids (not yet linked) render as stored.
    let address = |id: uuid::Uuid| {
        sqlx::query_scalar::<_, String>(
            "SELECT public_id FROM dm.conversation_addresses WHERE id = $1",
        )
        .bind(id)
    };
    assert_eq!(
        address(bricks.2).fetch_one(&mut connection).await?,
        "c:ada::c:saket"
    );
    assert_eq!(
        address(tos.2).fetch_one(&mut connection).await?,
        tos.2.to_string()
    );
    let silicon_pair: Vec<String> = sqlx::query_scalar(
        "SELECT public_id FROM dm.conversation_addresses WHERE public_id LIKE '%si:cos%'",
    )
    .fetch_all(&mut connection)
    .await?;
    assert_eq!(silicon_pair, vec!["c:saket::si:cos".to_owned()]);
    let contract: String = sqlx::query_scalar(
        "SELECT string_agg(version::text || ':' || status, ',' ORDER BY version) \
         FROM dm.contract_versions WHERE family = 'http' AND version >= 3",
    )
    .fetch_one(&mut connection)
    .await?;
    assert_eq!(contract, "3:deprecated,4:active");

    // The handoffs are all still there, none discarded by the migration itself.
    let handoffs: (i64, i64) = sqlx::query_as(
        "SELECT count(*), count(*) FILTER (WHERE discarded_at IS NOT NULL) FROM dm.ting_handoffs",
    )
    .fetch_one(&mut connection)
    .await?;
    assert_eq!(handoffs, (before["ting_handoffs"].1.0, 0));

    // 0043 gave every participant a read position from the receipts already recorded.
    let unplaced: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM dm.conversation_participants p WHERE NOT EXISTS (
            SELECT 1 FROM dm.conversation_read_states s WHERE s.conversation_id = p.conversation_id
              AND s.actor_kind = p.actor_kind AND s.actor_id = p.actor_id)",
    )
    .fetch_one(&mut connection)
    .await?;
    assert_eq!(unplaced, 0, "every participant has a read position");
    let (read, sequence): (Option<i64>, i64) = sqlx::query_as(
        "SELECT s.last_read_sequence, m.sequence FROM dm.messages m
         JOIN dm.conversation_read_states s ON s.conversation_id = m.conversation_id
         WHERE m.id = '01a122d0-ea9d-7f00-a45e-5785d46f2b43'
           AND s.actor_kind = 'silicon' AND s.actor_id = 'si:cos'",
    )
    .fetch_one(&mut connection)
    .await?;
    assert_eq!(
        read,
        Some(sequence),
        "the receipt from before the upgrade counts"
    );
    let beyond: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM dm.conversation_read_states s
         WHERE s.read_through_sequence > COALESCE((SELECT max(sequence) FROM dm.messages m
                                                   WHERE m.conversation_id = s.conversation_id), 0)",
    )
    .fetch_one(&mut connection)
    .await?;
    assert_eq!(
        beyond, 0,
        "no read position lies past the conversation's last message"
    );
    connection.close().await?;
    database.close().await
}
