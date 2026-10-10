//! `dm-migrate link-identities`: re-keys IAM-era identities (`c:handle`, `si:handle`) to
//! Silicon Accounts uuids in one transaction, following the 0036 precedent: user triggers
//! and foreign keys are suspended, only the declared identity columns are rewritten, derived
//! participant hashes are recomputed, and everything is restored exactly. Signed or
//! prepared bodies, request hashes, free text, routing strings and ciphertext are never
//! rewritten. Every pair is recorded in `dm.identity_links`, so a later run with a corrected
//! mapping re-points a link instead of failing, and the run's report is kept in
//! `dm.identity_link_runs`. A dry run does all of it and rolls back.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use sqlx::{Connection as _, PgConnection};
use uuid::Uuid;

use crate::{AppError, AppResult};

/// Text columns that hold an account's storage key.
const IDENTITY_COLUMNS: [&str; 11] = [
    "actor_id",
    "created_by_id",
    "sender_id",
    "recipient_id",
    "target_id",
    "target_silicon_id",
    "carbon_id",
    "originator_id",
    "creator_actor_id",
    "invited_by_id",
    "added_by_id",
];
/// Tables whose identity-named columns are not IAM-era keys (Accounts-era bookkeeping and
/// the frozen IAM projection kept as history).
const EXCLUDED_TABLES: [&str; 7] = [
    "accounts",
    "identity_links",
    "identity_link_runs",
    "accounts_webhook_receipts",
    "silicon_contact_allowances",
    "ting_proof_grants",
    "iam_membership_projections",
];

/// One row of the operator's mapping file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MappingRow {
    /// Line number in the file (for messages).
    pub line: usize,
    /// The IAM public id (`c:x`, `si:x`) or an IAM principal UUID.
    pub iam_key: String,
    /// The Accounts uuid it becomes.
    pub accounts_uuid: String,
}

/// What an account lookup returned for a mapped uuid (verification and cache seeding).
#[derive(Clone, Debug)]
pub struct VerifiedAccount {
    /// `carbon` or `silicon`.
    pub kind: String,
    /// Current public id.
    pub current_id: Option<String>,
    /// Display name.
    pub display_name: Option<String>,
    /// A Silicon's custodian uuid.
    pub custodian_uuid: Option<String>,
}

/// Options of one run.
pub struct LinkOptions {
    /// Do everything, report, roll back.
    pub dry_run: bool,
    /// Name recorded as the source of the links (the mapping file).
    pub source: String,
    /// Re-point links whose current uuid already signed in to DM.
    pub force_relink: bool,
}

/// The report of a run (printed as JSON and stored with real runs).
#[derive(Debug, Default, Serialize)]
pub struct LinkReport {
    /// The run id (also in `dm.identity_links.run_id`).
    pub run_id: Uuid,
    /// Whether the run rolled back.
    pub dry_run: bool,
    /// Mapping rows read.
    pub mapping_rows: usize,
    /// Identities newly linked.
    pub linked: Vec<String>,
    /// Identities whose link was re-pointed to another uuid.
    pub relinked: Vec<String>,
    /// Identities already linked to the same uuid (idempotent re-run).
    pub unchanged: Vec<String>,
    /// Rows rewritten per `table.column`.
    pub rewritten: BTreeMap<String, u64>,
    /// IAM-era ids still present in the data with no mapping, with their row counts.
    pub unmatched: BTreeMap<String, u64>,
    /// Mapping rows that named an id found nowhere in the data.
    pub unused_mapping_rows: Vec<String>,
    /// Direct conversations marked superseded by this run.
    pub superseded: u64,
    /// Account cache rows seeded for mapped uuids.
    pub accounts_seeded: u64,
    /// Non-fatal observations.
    pub warnings: Vec<String>,
}

/// Parses `iam_principal_id,accounts_uuid` (header optional; `#` comments and blank lines
/// ignored).
///
/// # Errors
/// Names the line and what is wrong with it.
pub fn parse_mapping(text: &str) -> AppResult<Vec<MappingRow>> {
    let mut rows = Vec::new();
    let mut seen = BTreeSet::new();
    let mut targets: BTreeMap<String, String> = BTreeMap::new();
    for (index, raw) in text.lines().enumerate() {
        let line = index + 1;
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = trimmed.split(',').map(str::trim).collect();
        if fields.len() != 2 {
            return Err(AppError::validation(format!(
                "line {line}: expected two columns `iam_principal_id,accounts_uuid`, got {}",
                fields.len()
            )));
        }
        if index == 0 && fields[0] == "iam_principal_id" && fields[1] == "accounts_uuid" {
            continue;
        }
        let (iam_key, accounts_uuid) = (fields[0].to_ascii_lowercase(), fields[1].to_owned());
        let public = super::valid_public_id(&iam_key);
        if !public && Uuid::parse_str(&iam_key).is_err() {
            return Err(AppError::validation(format!(
                "line {line}: `{}` is neither an IAM public id (c:handle, si:handle) nor an IAM principal UUID",
                fields[0]
            )));
        }
        if !crate::domain::is_account_uuid(&accounts_uuid) {
            return Err(AppError::validation(format!(
                "line {line}: `{accounts_uuid}` is not a Silicon Accounts uuid (letters and digits, case-sensitive, such as zQo)"
            )));
        }
        if !seen.insert(iam_key.clone()) {
            return Err(AppError::validation(format!(
                "line {line}: `{iam_key}` is mapped twice"
            )));
        }
        if let Some(other) = targets.insert(accounts_uuid.clone(), iam_key.clone()) {
            return Err(AppError::validation(format!(
                "line {line}: `{accounts_uuid}` is already the target of `{other}`; two IAM identities cannot become one account"
            )));
        }
        rows.push(MappingRow {
            line,
            iam_key,
            accounts_uuid,
        });
    }
    Ok(rows)
}

/// Suspends user triggers, foreign keys, NOT VALID checks, forced RLS and the canonical
/// direct-conversation index, remembering exactly what to restore (0036's procedure).
const SUSPEND: &str = r#"
CREATE TEMP TABLE link_tables ON COMMIT DROP AS
SELECT c.oid, format('%I.%I', n.nspname, c.relname) AS relation, c.relforcerowsecurity AS forced
FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
WHERE n.nspname IN ('dm', 'dm_private') AND c.relkind = 'r';
CREATE TEMP TABLE link_triggers ON COMMIT DROP AS
SELECT t2.relation, t.tgname, t.tgenabled
FROM pg_trigger t JOIN link_tables t2 ON t2.oid = t.tgrelid
WHERE NOT t.tgisinternal;
CREATE TEMP TABLE link_constraints ON COMMIT DROP AS
SELECT conrelid::regclass::text AS relation, conname, pg_get_constraintdef(oid) AS definition
FROM pg_constraint
WHERE conrelid IN (SELECT oid FROM link_tables)
  AND (contype = 'f' OR (contype = 'c' AND NOT convalidated));
DO $$ DECLARE r record; BEGIN
    FOR r IN SELECT * FROM link_tables ORDER BY relation LOOP
        EXECUTE format('LOCK TABLE %s IN ACCESS EXCLUSIVE MODE', r.relation);
        EXECUTE format('ALTER TABLE %s NO FORCE ROW LEVEL SECURITY', r.relation);
        EXECUTE format('ALTER TABLE %s DISABLE TRIGGER USER', r.relation);
    END LOOP;
    FOR r IN SELECT * FROM link_constraints LOOP
        EXECUTE format('ALTER TABLE %s DROP CONSTRAINT %I', r.relation, r.conname);
    END LOOP;
END $$;
DROP INDEX IF EXISTS dm.conversations_canonical_direct_idx;
"#;

/// Rewrites every identity column through `link_map`, counting rows per column.
const REWRITE: &str = r#"
CREATE TEMP TABLE link_counts (relation text, column_name text, rows bigint) ON COMMIT DROP;
-- An anchor row may already exist for the new key in the same partition (an account that
-- was added to a legacy conversation before the re-key); the old anchor is then left.
UPDATE dm.actor_snapshots s SET actor_id = m.new_key
FROM link_map m
WHERE s.actor_id = m.old_key
  AND NOT EXISTS (SELECT 1 FROM dm.actor_snapshots t WHERE t.organization_id = s.organization_id
                  AND t.actor_kind = s.actor_kind AND t.actor_id = m.new_key);
DO $$ DECLARE r record; changed bigint; BEGIN
    FOR r IN SELECT * FROM link_columns ORDER BY relation, column_name LOOP
        EXECUTE format('UPDATE %s target SET %I = m.new_key FROM link_map m WHERE target.%I = m.old_key',
            r.relation, r.column_name, r.column_name);
        GET DIAGNOSTICS changed = ROW_COUNT;
        IF changed > 0 THEN
            INSERT INTO link_counts VALUES (r.relation, r.column_name, changed);
        END IF;
    END LOOP;
END $$;
-- Derived participant identities of direct conversations (groups hash their own id).
WITH canonical AS (
    SELECT conversation_id,
        jsonb_agg(jsonb_build_object('type', actor_kind::text, 'id', actor_id)
            ORDER BY actor_kind::text COLLATE "C", actor_id COLLATE "C") AS participants,
        sha256(string_agg(int8send(octet_length(actor_kind::text)::bigint) || convert_to(actor_kind::text, 'UTF8') ||
            int8send(octet_length(actor_id)::bigint) || convert_to(actor_id, 'UTF8'), ''::bytea
            ORDER BY actor_kind::text COLLATE "C", actor_id COLLATE "C")) AS hash
    FROM dm.conversation_participants
    WHERE conversation_id IN (
        SELECT DISTINCT p.conversation_id FROM dm.conversation_participants p
        JOIN link_map m ON m.new_key = p.actor_id)
    GROUP BY conversation_id
)
UPDATE dm.conversations c SET participant_set = canonical.participants,
    participant_set_fingerprint = sha256(convert_to(canonical.participants::text, 'UTF8')),
    participant_set_hash = canonical.hash
FROM canonical
WHERE c.id = canonical.conversation_id AND NOT c.is_group;
"#;

/// Marks duplicates of one participant set superseded by the most recently active one.
const SUPERSEDE: &str = r#"
WITH ranked AS (
    SELECT id,
        row_number() OVER direct_set AS position,
        first_value(id) OVER direct_set AS canonical_id
    FROM dm.conversations
    WHERE NOT is_group AND superseded_by IS NULL
    WINDOW direct_set AS (PARTITION BY participant_set_hash ORDER BY updated_at DESC, id DESC)
)
UPDATE dm.conversations c SET superseded_by = ranked.canonical_id
FROM ranked WHERE ranked.id = c.id AND ranked.position > 1
"#;

/// Points conversations superseded earlier (at the 0040 upgrade, or by a previous run) at
/// the final canonical conversation, so a 409 never names a conversation that is itself
/// superseded. Run until it changes nothing.
const FLATTEN: &str = r#"
UPDATE dm.conversations c SET superseded_by = target.superseded_by
FROM dm.conversations target
WHERE c.superseded_by = target.id AND target.superseded_by IS NOT NULL
"#;

/// Keeps each re-keyed account's single delivery stream above its legacy sequences.
const STREAMS: &str = r#"
UPDATE dm.actor_delivery_streams s
SET next_sequence = GREATEST(s.next_sequence, high.max_sequence + 1),
    sync_floor = GREATEST(s.sync_floor, high.max_sequence)
FROM (
    SELECT d.target_kind, d.target_id, max(d.sequence) AS max_sequence
    FROM dm.actor_deliveries d JOIN link_map m ON m.new_key = d.target_id
    GROUP BY d.target_kind, d.target_id
) high
WHERE s.organization_id = '@accounts' AND s.actor_kind = high.target_kind
  AND s.actor_id = high.target_id AND s.next_sequence <= high.max_sequence
"#;

/// Restores everything SUSPEND took away, validating every foreign key again.
const RESTORE: &str = r#"
CREATE UNIQUE INDEX conversations_canonical_direct_idx
    ON dm.conversations (participant_set_hash)
    WHERE NOT is_group AND superseded_by IS NULL;
DO $$ DECLARE r record; BEGIN
    FOR r IN SELECT * FROM link_constraints LOOP
        EXECUTE format('ALTER TABLE %s ADD CONSTRAINT %I %s', r.relation, r.conname, r.definition);
    END LOOP;
    FOR r IN SELECT * FROM link_tables LOOP
        IF r.forced THEN EXECUTE format('ALTER TABLE %s FORCE ROW LEVEL SECURITY', r.relation); END IF;
    END LOOP;
    FOR r IN SELECT * FROM link_triggers LOOP
        EXECUTE format('ALTER TABLE %s %s TRIGGER %I', r.relation,
            CASE r.tgenabled WHEN 'D' THEN 'DISABLE' WHEN 'R' THEN 'ENABLE REPLICA'
                WHEN 'A' THEN 'ENABLE ALWAYS' ELSE 'ENABLE' END, r.tgname);
    END LOOP;
END $$;
"#;

/// Identity columns of IAM-era data (one row per `relation`, `column_name`).
const COLUMNS: &str = r#"
INSERT INTO link_columns (relation, column_name)
SELECT format('%I.%I', c.table_schema, c.table_name), c.column_name::text
FROM information_schema.columns c
JOIN pg_class k ON k.relname = c.table_name
JOIN pg_namespace n ON n.oid = k.relnamespace AND n.nspname = c.table_schema
WHERE c.table_schema IN ('dm', 'dm_private') AND k.relkind = 'r' AND c.data_type = 'text'
  AND c.column_name = ANY($1) AND c.table_name <> ALL($2)
  AND NOT (c.table_name = 'actor_snapshots' AND c.column_name = 'actor_id')
"#;

struct Pair {
    public_id: String,
    principal: Option<Uuid>,
    kind: &'static str,
    uuid: String,
}

/// Re-keys the mapped identities in one transaction (see the module documentation).
/// `verified` holds Silicon Accounts lookups of the mapped uuids when the operator ran
/// online; offline runs seed the account cache from the IAM ids and let DM refresh them.
///
/// # Errors
/// Any inconsistency (unknown principal, kind mismatch, a uuid already linked to another
/// identity, a re-point of a uuid that already signed in without `--force-relink`, a
/// foreign key that would no longer hold) aborts the whole run and changes nothing.
#[allow(
    clippy::too_many_lines,
    reason = "one audited transaction, phase by phase"
)]
pub async fn link_identities(
    connection: &mut PgConnection,
    rows: &[MappingRow],
    verified: &BTreeMap<String, VerifiedAccount>,
    options: &LinkOptions,
) -> AppResult<LinkReport> {
    let mut report = LinkReport {
        run_id: Uuid::now_v7(),
        dry_run: options.dry_run,
        mapping_rows: rows.len(),
        ..LinkReport::default()
    };
    let mut tx = connection.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('dm.link_identities', 0))")
        .execute(&mut *tx)
        .await?;

    // 1. Principal UUIDs → the public ids DM stored for them.
    let mut pairs = Vec::with_capacity(rows.len());
    for row in rows {
        let (public_id, principal) = if super::valid_public_id(&row.iam_key) {
            (row.iam_key.clone(), None)
        } else {
            let principal = Uuid::parse_str(&row.iam_key).map_err(AppError::internal)?;
            let ids: Vec<String> = sqlx::query_scalar(
                "SELECT DISTINCT actor_id FROM dm.iam_membership_projections WHERE principal_id = $1 AND actor_id IS NOT NULL",
            )
            .bind(principal)
            .fetch_all(&mut *tx)
            .await?;
            match ids.as_slice() {
                [only] => (only.clone(), Some(principal)),
                [] => {
                    return Err(AppError::validation(format!(
                        "line {}: IAM principal {principal} is not in DM's IAM projections; map its public id (c:/si:) instead",
                        row.line
                    )));
                }
                _ => {
                    return Err(AppError::validation(format!(
                        "line {}: IAM principal {principal} had several public ids ({}); map each public id instead",
                        row.line,
                        ids.join(", ")
                    )));
                }
            }
        };
        let kind = if public_id.starts_with("si:") {
            "silicon"
        } else {
            "carbon"
        };
        if let Some(account) = verified.get(&row.accounts_uuid)
            && account.kind != kind
        {
            return Err(AppError::validation(format!(
                "line {}: {public_id} is a {kind} but Silicon Accounts says {} is a {}",
                row.line, row.accounts_uuid, account.kind
            )));
        }
        let cached: Option<String> =
            sqlx::query_scalar("SELECT kind::text FROM dm.accounts WHERE uuid = $1")
                .bind(&row.accounts_uuid)
                .fetch_optional(&mut *tx)
                .await?;
        if cached.as_deref().is_some_and(|cached| cached != kind) {
            return Err(AppError::validation(format!(
                "line {}: {public_id} is a {kind} but DM knows {} as a {}",
                row.line,
                row.accounts_uuid,
                cached.unwrap_or_default()
            )));
        }
        pairs.push(Pair {
            public_id,
            principal,
            kind,
            uuid: row.accounts_uuid.clone(),
        });
    }

    // 2. Existing links decide each identity's current storage key.
    sqlx::query("CREATE TEMP TABLE link_map (old_key text PRIMARY KEY, new_key text NOT NULL UNIQUE, public_id text NOT NULL) ON COMMIT DROP")
        .execute(&mut *tx)
        .await?;
    for pair in &pairs {
        let existing: Option<String> = sqlx::query_scalar(
            "SELECT accounts_uuid FROM dm.identity_links WHERE iam_public_id = $1",
        )
        .bind(&pair.public_id)
        .fetch_optional(&mut *tx)
        .await?;
        let holder: Option<String> = sqlx::query_scalar(
            "SELECT iam_public_id FROM dm.identity_links WHERE accounts_uuid = $1",
        )
        .bind(&pair.uuid)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(holder) = holder.filter(|holder| *holder != pair.public_id) {
            return Err(AppError::validation(format!(
                "{} cannot become {}: that uuid is already linked to {holder}",
                pair.public_id, pair.uuid
            )));
        }
        let old_key = match existing {
            Some(current) if current == pair.uuid => {
                report.unchanged.push(pair.public_id.clone());
                continue;
            }
            Some(current) => {
                let signed_in: bool = sqlx::query_scalar(
                    "SELECT EXISTS (SELECT 1 FROM dm.accounts WHERE uuid = $1 AND last_authenticated_at IS NOT NULL)",
                )
                .bind(&current)
                .fetch_one(&mut *tx)
                .await?;
                if signed_in && !options.force_relink {
                    return Err(AppError::validation(format!(
                        "{} is linked to {current}, which has already signed in to DM; re-pointing it would move that account's new data too. Pass --force-relink if that is intended.",
                        pair.public_id
                    )));
                }
                report
                    .relinked
                    .push(format!("{}: {current} -> {}", pair.public_id, pair.uuid));
                current
            }
            None => {
                report
                    .linked
                    .push(format!("{} -> {}", pair.public_id, pair.uuid));
                pair.public_id.clone()
            }
        };
        sqlx::query("INSERT INTO link_map (old_key, new_key, public_id) VALUES ($1, $2, $3)")
            .bind(&old_key)
            .bind(&pair.uuid)
            .bind(&pair.public_id)
            .execute(&mut *tx)
            .await
            .map_err(|_| {
                AppError::validation(format!(
                    "{} appears twice in the mapping after resolving principals",
                    pair.public_id
                ))
            })?;
    }

    // 3. Rewrite with constraints suspended, then restore and revalidate everything.
    sqlx::query("CREATE TEMP TABLE link_columns (relation text, column_name text) ON COMMIT DROP")
        .execute(&mut *tx)
        .await?;
    let identity_columns: Vec<String> = IDENTITY_COLUMNS.iter().map(|c| (*c).to_owned()).collect();
    let excluded: Vec<String> = EXCLUDED_TABLES.iter().map(|t| (*t).to_owned()).collect();
    sqlx::query(COLUMNS)
        .bind(&identity_columns)
        .bind(&excluded)
        .execute(&mut *tx)
        .await?;
    let map: Vec<(String, String)> =
        sqlx::query_as("SELECT old_key, public_id FROM link_map ORDER BY public_id")
            .fetch_all(&mut *tx)
            .await?;
    let old_keys: Vec<String> = map.iter().map(|(old, _)| old.clone()).collect();
    let mut scanned: Vec<(String, String)> =
        sqlx::query_as("SELECT relation, column_name FROM link_columns")
            .fetch_all(&mut *tx)
            .await?;
    scanned.push(("dm.actor_snapshots".to_owned(), "actor_id".to_owned()));
    let mut used = BTreeSet::new();
    for (relation, column) in &scanned {
        let found: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT DISTINCT {column} FROM {relation} WHERE {column} = ANY($1)"
        )))
        .bind(&old_keys)
        .fetch_all(&mut *tx)
        .await?;
        used.extend(found);
    }
    for (old, public_id) in &map {
        if !used.contains(old) {
            report.unused_mapping_rows.push(public_id.clone());
        }
    }
    if !map.is_empty() {
        sqlx::raw_sql(SUSPEND).execute(&mut *tx).await?;
        sqlx::raw_sql(REWRITE).execute(&mut *tx).await?;
        report.superseded = sqlx::query(SUPERSEDE)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        while sqlx::query(FLATTEN)
            .execute(&mut *tx)
            .await?
            .rows_affected()
            > 0
        {}
        sqlx::query(STREAMS).execute(&mut *tx).await?;
        sqlx::raw_sql(RESTORE).execute(&mut *tx).await.map_err(|error| {
            AppError::validation(format!(
                "the re-keyed data would break a database invariant, so nothing was changed: {error}"
            ))
        })?;
        let counts: Vec<(String, String, i64)> =
            sqlx::query_as("SELECT relation, column_name, rows FROM link_counts ORDER BY 1, 2")
                .fetch_all(&mut *tx)
                .await?;
        for (relation, column, rows) in counts {
            report.rewritten.insert(
                format!("{relation}.{column}"),
                u64::try_from(rows).unwrap_or(0),
            );
        }
        let orphans: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM dm.actor_snapshots s JOIN link_map m ON m.old_key = s.actor_id",
        )
        .fetch_one(&mut *tx)
        .await?;
        if orphans > 0 {
            report.warnings.push(format!(
                "{orphans} directory anchor rows kept their IAM-era key because the account already had an anchor in the same partition (unreferenced now; harmless)"
            ));
        }
    }

    // 4. Record the links and seed the account cache.
    for pair in &pairs {
        let previous: Option<String> = sqlx::query_scalar(
            "SELECT accounts_uuid FROM dm.identity_links WHERE iam_public_id = $1",
        )
        .bind(&pair.public_id)
        .fetch_optional(&mut *tx)
        .await?;
        if previous.as_deref() == Some(pair.uuid.as_str()) {
            continue;
        }
        sqlx::query(
            "INSERT INTO dm.identity_links (iam_public_id, iam_principal_id, kind, accounts_uuid, previous_uuid, source, run_id)
             VALUES ($1, $2, $3::text::dm.actor_kind, $4, NULL, $5, $6)
             ON CONFLICT (iam_public_id) DO UPDATE SET accounts_uuid = EXCLUDED.accounts_uuid,
                previous_uuid = dm.identity_links.accounts_uuid, source = EXCLUDED.source,
                run_id = EXCLUDED.run_id, linked_at = clock_timestamp(),
                iam_principal_id = COALESCE(EXCLUDED.iam_principal_id, dm.identity_links.iam_principal_id)",
        )
        .bind(&pair.public_id)
        .bind(pair.principal)
        .bind(pair.kind)
        .bind(&pair.uuid)
        .bind(&options.source)
        .bind(report.run_id)
        .execute(&mut *tx)
        .await?;
        let verified_account = verified.get(&pair.uuid);
        let seeded = sqlx::query(
            "INSERT INTO dm.accounts (uuid, kind, current_id, display_name, custodian_uuid, refreshed_at)
             VALUES ($1, $2::text::dm.actor_kind, $3, $4, $5,
                     CASE WHEN $6 THEN clock_timestamp() ELSE to_timestamp(0) END)
             ON CONFLICT (uuid) DO NOTHING",
        )
        .bind(&pair.uuid)
        .bind(pair.kind)
        .bind(
            verified_account
                .and_then(|account| account.current_id.clone())
                .or_else(|| Some(pair.public_id.clone())),
        )
        .bind(verified_account.and_then(|account| account.display_name.clone()))
        .bind(verified_account.and_then(|account| account.custodian_uuid.clone()))
        .bind(verified_account.is_some())
        .execute(&mut *tx)
        .await?
        .rows_affected();
        report.accounts_seeded += seeded;
    }

    // 5. What is left: IAM-era ids nobody mapped, and mapping rows nothing used.
    let columns: Vec<(String, String)> =
        sqlx::query_as("SELECT relation, column_name FROM link_columns ORDER BY 1, 2")
            .fetch_all(&mut *tx)
            .await?;
    for (relation, column) in &columns {
        let leftovers: Vec<(String, i64)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {column}, count(*) FROM {relation} WHERE {column} LIKE '%:%' GROUP BY 1"
        )))
        .fetch_all(&mut *tx)
        .await?;
        for (id, count) in leftovers {
            *report.unmatched.entry(id).or_default() += u64::try_from(count).unwrap_or(0);
        }
    }
    if !report.unmatched.is_empty() {
        report.warnings.push(format!(
            "{} IAM-era ids have no mapping; their rows stay as they are (shown as legacy identities) until a run links them",
            report.unmatched.len()
        ));
    }
    if options.dry_run {
        tx.rollback().await?;
    } else {
        sqlx::query(
            "INSERT INTO dm.identity_link_runs (run_id, source, report) VALUES ($1, $2, $3)",
        )
        .bind(report.run_id)
        .bind(&options.source)
        .bind(sqlx::types::Json(&report))
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
    }
    Ok(report)
}

/// The IAM-era ids still present in the data (for drafting a mapping).
///
/// # Errors
/// Database failures.
pub async fn legacy_ids(connection: &mut PgConnection) -> AppResult<BTreeMap<String, u64>> {
    let mut tx = connection.begin().await?;
    sqlx::query("CREATE TEMP TABLE link_columns (relation text, column_name text) ON COMMIT DROP")
        .execute(&mut *tx)
        .await?;
    let identity_columns: Vec<String> = IDENTITY_COLUMNS.iter().map(|c| (*c).to_owned()).collect();
    let excluded: Vec<String> = EXCLUDED_TABLES.iter().map(|t| (*t).to_owned()).collect();
    sqlx::query(COLUMNS)
        .bind(&identity_columns)
        .bind(&excluded)
        .execute(&mut *tx)
        .await?;
    let columns: Vec<(String, String)> =
        sqlx::query_as("SELECT relation, column_name FROM link_columns ORDER BY 1, 2")
            .fetch_all(&mut *tx)
            .await?;
    let mut ids = BTreeMap::new();
    for (relation, column) in &columns {
        let found: Vec<(String, i64)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {column}, count(*) FROM {relation} WHERE {column} LIKE '%:%' GROUP BY 1"
        )))
        .fetch_all(&mut *tx)
        .await?;
        for (id, count) in found {
            *ids.entry(id).or_default() += u64::try_from(count).unwrap_or(0);
        }
    }
    tx.rollback().await?;
    Ok(ids)
}
