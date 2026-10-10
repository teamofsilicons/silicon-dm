//! `dm-migrate link-identities`: the cutover re-key of IAM-era identities to Silicon
//! Accounts uuids, on a database upgraded from 0.14.1 (the legacy fixture). Dry runs change
//! nothing; a run rewrites every identity column, keeps every constraint, trigger and
//! index, is idempotent, and can be corrected; the service then serves the old data.
#![allow(
    clippy::too_many_lines,
    reason = "each test is one scenario, read top to bottom"
)]
mod support;

use std::collections::BTreeMap;

use serde_json::{Value, json};
use silicon_dm::infrastructure::postgres::link_identities::{
    LinkOptions, LinkReport, VerifiedAccount, link_identities, parse_mapping,
};
use sqlx::{Connection as _, PgConnection};
use support::{BoxError, TestDatabase};

/// si:cos is mapped by its IAM principal, the others by their public id; c:old-timer never
/// moved to Silicon Accounts; c:nobody is not in the data.
const MAPPING: &str = "iam_principal_id,accounts_uuid
# cutover mapping
c:saket,zQo
C:Ada,a8K
b6c8e8c5-77d6-4c2a-9ebf-75aca3364a39,K1E
c:nobody,Nb0
";

async fn connect(url: &str) -> Result<PgConnection, BoxError> {
    let mut connection = PgConnection::connect(url).await?;
    sqlx::query("SET search_path=public")
        .execute(&mut connection)
        .await?;
    Ok(connection)
}

async fn upgraded() -> Result<TestDatabase, BoxError> {
    TestDatabase::upgraded_from_0038().await
}

fn verified() -> BTreeMap<String, VerifiedAccount> {
    let account = |kind: &str, id: &str, name: &str, custodian: Option<&str>| VerifiedAccount {
        kind: kind.to_owned(),
        current_id: Some(id.to_owned()),
        display_name: Some(name.to_owned()),
        custodian_uuid: custodian.map(str::to_owned),
    };
    BTreeMap::from([
        (
            "zQo".to_owned(),
            account("carbon", "c:saket", "Saket", None),
        ),
        ("a8K".to_owned(), account("carbon", "c:ada", "Ada", None)),
        // Renamed since IAM: the link is what lets old routing show the current id.
        (
            "K1E".to_owned(),
            account(
                "silicon",
                "si:chief-of-staff",
                "Chief of Staff",
                Some("zQo"),
            ),
        ),
        (
            "Nb0".to_owned(),
            account("carbon", "c:nobody", "Nobody", None),
        ),
    ])
}

async fn run(
    url: &str,
    mapping: &str,
    dry_run: bool,
    force_relink: bool,
) -> Result<LinkReport, BoxError> {
    let mut connection = connect(url).await?;
    let report = link_identities(
        &mut connection,
        &parse_mapping(mapping)?,
        &verified(),
        &LinkOptions {
            dry_run,
            source: "mapping.csv".to_owned(),
            force_relink,
        },
    )
    .await?;
    connection.close().await?;
    Ok(report)
}

async fn scalar(url: &str, sql: &str) -> Result<i64, BoxError> {
    let mut connection = connect(url).await?;
    let value: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_owned()))
        .fetch_one(&mut connection)
        .await?;
    connection.close().await?;
    Ok(value)
}

/// Everything the re-key suspends and must restore exactly.
async fn invariants(url: &str) -> Result<Vec<String>, BoxError> {
    let mut connection = connect(url).await?;
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT format('constraint %s %s %s', conrelid::regclass, conname, pg_get_constraintdef(oid))
         FROM pg_constraint WHERE connamespace = 'dm'::regnamespace AND contype IN ('f', 'c')
         UNION ALL
         SELECT format('trigger %s %s %s', tgrelid::regclass, tgname, tgenabled)
         FROM pg_trigger WHERE NOT tgisinternal AND tgrelid IN (SELECT oid FROM pg_class WHERE relnamespace = 'dm'::regnamespace)
         UNION ALL
         SELECT format('table %s rls=%s forced=%s', oid::regclass, relrowsecurity, relforcerowsecurity)
         FROM pg_class WHERE relnamespace = 'dm'::regnamespace AND relkind = 'r'
         UNION ALL
         SELECT format('index %s', indexdef) FROM pg_indexes WHERE schemaname = 'dm'
         ORDER BY 1",
    )
    .fetch_all(&mut connection)
    .await?;
    connection.close().await?;
    Ok(rows)
}

#[tokio::test]
async fn mappings_are_validated_line_by_line() {
    for (text, message) in [
        ("c:ada", "two columns"),
        ("c:ada,a8K,extra", "two columns"),
        ("ada,a8K", "neither an IAM public id"),
        ("c:ada,not-a-uuid!", "not a Silicon Accounts uuid"),
        ("c:ada,a8K\nc:ada,b9Z", "mapped twice"),
        (
            "c:ada,a8K\nc:bob,a8K",
            "two IAM identities cannot become one account",
        ),
    ] {
        let error = parse_mapping(text)
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default();
        assert!(error.contains(message), "{text}: {error}");
    }
    let rows = parse_mapping(MAPPING).unwrap_or_default();
    assert_eq!(rows.len(), 4);
    assert_eq!(
        rows[1].iam_key, "c:ada",
        "public ids are compared in lowercase"
    );
    assert_eq!(rows[1].accounts_uuid, "a8K", "uuids keep their case");
}

#[tokio::test]
async fn a_dry_run_reports_everything_and_changes_nothing() -> Result<(), BoxError> {
    let database = upgraded().await?;
    let before = invariants(&database.url).await?;
    let report = run(&database.url, MAPPING, true, false).await?;
    assert!(report.dry_run);
    assert_eq!(report.linked.len(), 4, "{report:?}");
    assert_eq!(report.unused_mapping_rows, vec!["c:nobody".to_owned()]);
    assert!(
        report.unmatched.contains_key("c:old-timer"),
        "{:?}",
        report.unmatched
    );
    assert!(
        report
            .rewritten
            .get("dm.messages.sender_id")
            .is_some_and(|rows| *rows > 0),
        "{:?}",
        report.rewritten
    );
    assert_eq!(
        scalar(&database.url, "SELECT count(*) FROM dm.identity_links").await?,
        0
    );
    assert_eq!(
        scalar(&database.url, "SELECT count(*) FROM dm.identity_link_runs").await?,
        0
    );
    assert_eq!(
        scalar(
            &database.url,
            "SELECT count(*) FROM dm.accounts WHERE uuid IN ('zQo', 'a8K', 'K1E')"
        )
        .await?,
        0
    );
    assert!(
        scalar(
            &database.url,
            "SELECT count(*) FROM dm.messages WHERE sender_id = 'c:saket'"
        )
        .await?
            > 0
    );
    assert_eq!(invariants(&database.url).await?, before);
    database.close().await
}

#[tokio::test]
async fn a_run_rekeys_every_identity_column_and_restores_every_invariant() -> Result<(), BoxError> {
    let database = upgraded().await?;
    let url = database.url.clone();
    let before = invariants(&url).await?;
    let messages = scalar(&url, "SELECT count(*) FROM dm.messages").await?;
    let report = run(&url, MAPPING, false, false).await?;
    assert_eq!(report.linked.len(), 4);
    assert_eq!(report.accounts_seeded, 4);
    assert_eq!(
        invariants(&url).await?,
        before,
        "constraints, triggers, RLS and indexes are back"
    );
    assert_eq!(
        scalar(&url, "SELECT count(*) FROM dm.messages").await?,
        messages,
        "no row is added or lost"
    );
    for (table, column) in [
        ("messages", "sender_id"),
        ("conversation_participants", "actor_id"),
        ("conversations", "created_by_id"),
        ("message_receipts", "recipient_id"),
        ("drafts", "actor_id"),
        ("group_invitations", "actor_id"),
        ("actor_deliveries", "target_id"),
    ] {
        let legacy = scalar(
            &url,
            &format!(
                "SELECT count(*) FROM dm.{table} WHERE {column} IN ('c:saket', 'c:ada', 'si:cos')"
            ),
        )
        .await?;
        assert_eq!(legacy, 0, "{table}.{column} still names a mapped IAM id");
    }
    assert!(
        scalar(
            &url,
            "SELECT count(*) FROM dm.messages WHERE sender_id = 'zQo'"
        )
        .await?
            > 0
    );
    assert!(
        scalar(
            &url,
            "SELECT count(*) FROM dm.messages WHERE sender_id = 'c:old-timer'"
        )
        .await?
            > 0,
        "unmapped ids stay"
    );
    // Derived participant sets follow the new keys.
    assert_eq!(
        scalar(&url, "SELECT count(*) FROM dm.conversations WHERE NOT is_group AND participant_set::text LIKE '%c:saket%'").await?,
        0
    );
    // Links, the run record and the seeded account cache.
    assert_eq!(
        scalar(&url, "SELECT count(*) FROM dm.identity_links").await?,
        4
    );
    assert_eq!(
        scalar(&url, "SELECT count(*) FROM dm.identity_links WHERE iam_public_id = 'si:cos' AND iam_principal_id = 'b6c8e8c5-77d6-4c2a-9ebf-75aca3364a39'").await?,
        1
    );
    assert_eq!(
        scalar(&url, "SELECT count(*) FROM dm.identity_link_runs").await?,
        1
    );
    assert_eq!(
        scalar(&url, "SELECT count(*) FROM dm.accounts WHERE uuid = 'K1E' AND current_id = 'si:chief-of-staff' AND custodian_uuid = 'zQo'").await?,
        1
    );
    // Each account's one delivery stream continues above its legacy sequences.
    assert_eq!(
        scalar(&url, "SELECT count(*) FROM dm.actor_delivery_streams s WHERE s.organization_id = '@accounts'
            AND s.next_sequence <= (SELECT coalesce(max(sequence), 0) FROM dm.actor_deliveries d WHERE d.target_id = s.actor_id)").await?,
        0
    );
    database.close().await
}

#[tokio::test]
async fn running_again_is_idempotent_and_corrections_repoint_with_history() -> Result<(), BoxError>
{
    let database = upgraded().await?;
    let url = database.url.clone();
    run(&url, MAPPING, false, false).await?;
    let again = run(&url, MAPPING, false, false).await?;
    assert!(
        again.linked.is_empty() && again.relinked.is_empty(),
        "{again:?}"
    );
    assert_eq!(again.unchanged.len(), 4);
    assert!(again.rewritten.is_empty(), "{:?}", again.rewritten);

    // c:ada was mapped to the wrong account; nobody signed in with it yet.
    let corrected = run(&url, "c:ada,a9Z\n", false, false).await?;
    assert_eq!(corrected.relinked, vec!["c:ada: a8K -> a9Z".to_owned()]);
    assert_eq!(
        scalar(
            &url,
            "SELECT count(*) FROM dm.messages WHERE sender_id = 'a8K'"
        )
        .await?,
        0
    );
    assert!(
        scalar(
            &url,
            "SELECT count(*) FROM dm.messages WHERE sender_id = 'a9Z'"
        )
        .await?
            > 0
    );
    assert_eq!(
        scalar(&url, "SELECT count(*) FROM dm.identity_links WHERE iam_public_id = 'c:ada' AND accounts_uuid = 'a9Z' AND previous_uuid = 'a8K'").await?,
        1
    );
    // Once that account has signed in, moving its data needs --force-relink.
    let mut connection = connect(&url).await?;
    sqlx::query(
        "UPDATE dm.accounts SET last_authenticated_at = clock_timestamp() WHERE uuid = 'a9Z'",
    )
    .execute(&mut connection)
    .await?;
    connection.close().await?;
    let refused = run(&url, "c:ada,a7Y\n", false, false).await;
    assert!(
        refused
            .err()
            .is_some_and(|error| error.to_string().contains("--force-relink"))
    );
    assert!(
        scalar(
            &url,
            "SELECT count(*) FROM dm.messages WHERE sender_id = 'a9Z'"
        )
        .await?
            > 0,
        "nothing changed"
    );
    let forced = run(&url, "c:ada,a7Y\n", false, true).await?;
    assert_eq!(forced.relinked, vec!["c:ada: a9Z -> a7Y".to_owned()]);
    // A uuid already linked to another identity is refused outright.
    let taken = run(&url, "c:old-timer,zQo\n", false, false).await;
    assert!(
        taken
            .err()
            .is_some_and(|error| error.to_string().contains("already linked to c:saket"))
    );
    // An IAM principal DM never saw.
    let unknown = run(
        &url,
        "00000000-0000-4000-8000-000000000000,Uk1\n",
        false,
        false,
    )
    .await;
    assert!(
        unknown
            .err()
            .is_some_and(|error| error.to_string().contains("not in DM's IAM projections"))
    );
    database.close().await
}

#[tokio::test]
async fn dm_migrate_runs_the_link_from_a_mapping_file() -> Result<(), BoxError> {
    let database = upgraded().await?;
    let directory = std::env::temp_dir().join(format!("dm-link-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&directory)?;
    let file = directory.join("mapping.csv");
    std::fs::write(&file, MAPPING)?;
    let command = |arguments: &[&str]| {
        std::process::Command::new(env!("CARGO_BIN_EXE_dm-migrate"))
            .args(arguments)
            .current_dir(&directory)
            .env_clear()
            .env("DM_DATABASE_URL", &database.url)
            .env("DM_ENVIRONMENT", "test")
            .env("DM_LOG_FILTER", "warn")
            .output()
    };
    let help = command(&["--help"])?;
    assert!(String::from_utf8_lossy(&help.stdout).contains("link-identities --file <mapping.csv>"));
    let path = file.display().to_string();
    let dry = command(&["link-identities", "--file", &path, "--dry-run", "--offline"])?;
    assert!(
        dry.status.success(),
        "{}",
        String::from_utf8_lossy(&dry.stderr)
    );
    let report: Value = serde_json::from_slice(&dry.stdout)?;
    assert_eq!(report["dry_run"], true);
    assert_eq!(report["unused_mapping_rows"], json!(["c:nobody"]));
    assert!(String::from_utf8_lossy(&dry.stderr).contains("dry run (rolled back): 4 linked"));
    // Online verification needs Silicon Accounts settings; nothing runs without them.
    let online = command(&["link-identities", "--file", &path])?;
    assert!(!online.status.success());
    assert!(String::from_utf8_lossy(&online.stderr).contains("ACCOUNTS_URL is required"));
    let applied = command(&["link-identities", "--file", &path, "--offline"])?;
    assert!(
        applied.status.success(),
        "{}",
        String::from_utf8_lossy(&applied.stderr)
    );
    assert_eq!(
        scalar(&database.url, "SELECT count(*) FROM dm.identity_links").await?,
        4
    );
    // Offline seeding leaves the cache stale so DM refreshes it from Silicon Accounts.
    assert_eq!(
        scalar(&database.url, "SELECT count(*) FROM dm.accounts WHERE uuid = 'K1E' AND current_id = 'si:cos' AND refreshed_at = to_timestamp(0)").await?,
        1
    );
    let missing = command(&["link-identities"])?;
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("--file"));
    std::fs::remove_dir_all(&directory)?;
    database.close().await
}

#[tokio::test]
async fn the_service_serves_legacy_data_by_account_after_the_rekey() -> Result<(), BoxError> {
    use axum::http::StatusCode;
    use support::{
        accounts::TestAccount,
        app::{TestApp, expect_status},
    };
    let app = TestApp::start_on(upgraded().await?, "").await?;
    let saket = TestAccount::carbon("zQo", "c:saket");
    let ada = TestAccount::carbon("a8K", "c:ada");
    let cos = TestAccount::silicon("K1E", "si:chief-of-staff", &saket);
    app.accounts.serve_only(&[&saket, &ada, &cos]).await;
    // Before the re-key, ada signs in and starts a new-era conversation with saket.
    let early = app
        .post(
            "/api/v1/messages",
            &app.accounts.token(&ada),
            json!({"recipient_id": "c:saket", "message": "new era"}),
        )
        .await?;
    expect_status(&early, StatusCode::ACCEPTED)?;
    let report = run(&app.database.url, MAPPING, false, false).await?;
    assert_eq!(
        report.superseded, 1,
        "the legacy pair is now a duplicate of the new conversation: {report:?}"
    );

    let token = app.accounts.token(&saket);
    let listed = app.get("/api/v1/conversations", &token).await?;
    expect_status(&listed, StatusCode::OK)?;
    let ids: Vec<&str> = listed.1["items"]
        .as_array()
        .map(|items| items.iter().filter_map(|c| c["id"].as_str()).collect())
        .unwrap_or_default();
    for expected in [
        "c:ada::c:saket",
        "c:saket::si:chief-of-staff",
        "g:tos:product-design",
        "g:tos:all-hands",
        "g:tos:tagged",
    ] {
        assert!(ids.contains(&expected), "{expected} missing from {ids:?}");
    }
    assert_eq!(
        ids.iter().filter(|id| **id == "c:ada::c:saket").count(),
        1,
        "superseded duplicates are not listed"
    );
    let canonical = listed.1["items"]
        .as_array()
        .and_then(|items| items.iter().find(|c| c["id"] == "c:ada::c:saket"))
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        canonical["supersedes"].as_array().map(Vec::len),
        Some(2),
        "{canonical}"
    );

    // Old messages show today's ids; old ISI routing follows the link.
    let history = app
        .get(
            "/api/v1/conversations/c:saket::si:chief-of-staff/messages",
            &token,
        )
        .await?;
    expect_status(&history, StatusCode::OK)?;
    let items = history.1["items"].as_array().cloned().unwrap_or_default();
    assert!(
        items
            .iter()
            .any(|m| m["sender"]["id"] == "si:chief-of-staff" && m["sender"]["uuid"] == "K1E"),
        "{}",
        history.1
    );
    assert!(
        items
            .iter()
            .any(|m| m["recipient_id"] == "deliberate@si:chief-of-staff"),
        "{}",
        history.1
    );

    // A superseded conversation keeps its history but takes no new messages.
    let old_id = canonical["supersedes"][0]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let old = app
        .get(&format!("/api/v1/conversations/{old_id}/messages"), &token)
        .await?;
    expect_status(&old, StatusCode::OK)?;
    let refused = app
        .post(
            &format!("/api/v1/conversations/{old_id}/messages"),
            &token,
            json!({"message": "x"}),
        )
        .await?;
    assert_eq!(refused.0, StatusCode::CONFLICT);
    assert_eq!(refused.1["error"]["code"], "conversation_superseded");
    assert!(
        refused.1["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("c:ada::c:saket"))
    );
    let sent = app
        .post(
            "/api/v1/messages",
            &token,
            json!({"recipient_id": "c:ada", "message": "same place"}),
        )
        .await?;
    expect_status(&sent, StatusCode::ACCEPTED)?;
    assert_eq!(sent.1["conversation_id"], "c:ada::c:saket");

    // An identity that never moved is shown as stored, and its conversation stays readable.
    let ada_token = app.accounts.token(&ada);
    let with_old_timer = app
        .get(
            "/api/v1/conversations/c:ada::c:old-timer/messages",
            &ada_token,
        )
        .await?;
    expect_status(&with_old_timer, StatusCode::OK)?;
    assert!(
        with_old_timer.1["items"]
            .as_array()
            .is_some_and(|items| items
                .iter()
                .any(|m| m["sender"]["legacy"] == true && m["sender"]["id"] == "c:old-timer")),
        "{}",
        with_old_timer.1
    );
    // Groups keep their members and owners under the new keys.
    let group = app
        .get("/api/v1/groups/g:tos:product-design", &token)
        .await?;
    expect_status(&group, StatusCode::OK)?;
    assert!(
        group.1["group"]["owners"]
            .as_array()
            .is_some_and(|owners| owners.iter().any(|o| o["uuid"] == "zQo")),
        "{}",
        group.1
    );
    app.close().await
}
