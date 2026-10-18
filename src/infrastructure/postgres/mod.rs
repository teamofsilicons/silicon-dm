//! PostgreSQL pool and durable store.

mod accounts;
mod allow_list;
mod bundles;
mod conversation_events;
mod conversations;
mod delivery;
mod directory;
mod drafts;
mod group_ids;
pub(crate) mod groups;
mod http_presence;
mod idempotency;
pub mod link_identities;
mod messages;
mod presence;
mod public_messages;
mod read_states;
mod revisions;
mod rows;
mod speech;
mod sync;
mod ting;

use std::time::Duration;

use secrecy::ExposeSecret as _;
use sqlx::{Connection as _, PgPool, Row as _, migrate::Migrator, postgres::PgPoolOptions};

use crate::{
    AppError, AppResult,
    config::{DatabaseSettings, MigrationSettings},
};

pub use accounts::{Applied, StoredAccount, valid_public_id};
pub use allow_list::AllowListEntry;
pub use delivery::{Pruned, SYNC_RETENTION};
pub use http_presence::PresenceLease;
pub use presence::ActivityView;
pub use read_states::{
    BulkReceiptOutcome, MAX_BULK_RECEIPTS, UNREAD_COUNT_CAP, UNREAD_TOTAL_CONVERSATIONS,
    UnreadSummary,
};
pub use revisions::Reviser;
pub use speech::{
    AudioMeta, ChosenVoice, LimitExceeded, MAX_RENDER_ATTEMPTS, RenderClaim, RenderOutcome,
    RenderState, RenderStatus, SpeechCharge, SpeechCharged, SpeechLease, SpeechRenderJob,
    limit_error, retry_after_seconds,
};
pub(crate) use sync::sync_reset_required;
pub use sync::{SyncEvent, SyncScan};
pub use ting::{
    DISCARD_PREPARED_BEFORE_CUTOVER, DISCARD_RECIPIENT_WITHOUT_ACCOUNT,
    DISCARD_TING_NOT_CONFIGURED, TingDeliveryClaim, TingDeliveryContext,
};

pub(crate) static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

/// Cloneable PostgreSQL-backed DM store.
#[derive(Clone)]
pub struct PostgresStore {
    pool: PgPool,
}

impl PostgresStore {
    /// Opens a validated, bounded PostgreSQL pool.
    ///
    /// # Errors
    ///
    /// Returns a redacted database error when the pool cannot connect or apply
    /// its statement deadline.
    pub async fn connect(settings: &DatabaseSettings) -> AppResult<Self> {
        Self::connect_schema(settings, "dm").await
    }

    /// Opens a pool whose every connection is permanently scoped to a trusted schema.
    ///
    /// # Errors
    /// Returns invalid-schema validation errors or redacted database connection errors.
    pub async fn connect_schema(settings: &DatabaseSettings, schema: &str) -> AppResult<Self> {
        if schema.is_empty()
            || schema.len() > 63
            || !schema
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        {
            return Err(AppError::validation("invalid database schema"));
        }
        let schema = schema.to_owned();
        let statement_timeout = settings.statement_timeout;
        let pool = PgPoolOptions::new()
            .max_connections(settings.max_connections.get())
            .min_connections(settings.min_connections)
            .acquire_timeout(settings.acquire_timeout)
            .after_release(|connection, _metadata| {
                Box::pin(async move {
                    // A 100 MB result must not permanently enlarge every pooled
                    // connection's read/write buffers after the request ends.
                    connection.shrink_buffers();
                    // A request cancelled while sqlx's begin() still waited for BEGIN's answer (a client that
                    // went away mid-request) hands its connection back inside that transaction: sqlx arms its
                    // rollback only once BEGIN is answered. Reused, it would let later autocommit statements
                    // join that transaction, hold their row locks and lose their writes if it rolled back. Such
                    // a connection is closed instead, which ends the transaction on the server. sqlx keeps the
                    // transaction state private, so the server is asked (off the request's path: release runs
                    // in its own task). As one simple-protocol query, outside a transaction the statement is its
                    // own transaction and both timestamps are equal; inside one they differ. (A prepared query's
                    // Parse/Bind/Execute would give the statement a later timestamp than its implicit transaction.)
                    let idle =
                        sqlx::raw_sql("SELECT transaction_timestamp() = statement_timestamp()")
                            .fetch_one(&mut *connection)
                            .await
                            .and_then(|row| row.try_get::<bool, _>(0));
                    Ok(matches!(idle, Ok(true)))
                })
            })
            .after_connect(move |connection, _metadata| {
                let schema = schema.clone();
                Box::pin(async move {
                    sqlx::query("SELECT set_config('search_path', $1, false)")
                        .bind(schema)
                        .execute(&mut *connection)
                        .await?;
                    let timeout = duration_as_postgres_milliseconds(statement_timeout);
                    sqlx::query("SELECT set_config('statement_timeout', $1, false)")
                        .bind(timeout)
                        .execute(&mut *connection)
                        .await?;
                    // The backstop for what after_release closes: the server ends a session that sits inside a
                    // transaction doing nothing for a minute. DM's transactions never wait that long between
                    // statements (the longest wait is a 10 s Postmark call while a report is claimed).
                    sqlx::query(
                        "SELECT set_config('idle_in_transaction_session_timeout', '60s', false)",
                    )
                    .execute(connection)
                    .await?;
                    Ok(())
                })
            })
            .connect(settings.url.expose_secret())
            .await
            .map_err(AppError::database)?;
        Ok(Self { pool })
    }

    /// Runs embedded migrations through the already configured pool.
    ///
    /// # Errors
    ///
    /// Returns a migration failure without exposing the connection URL.
    pub async fn migrate(&self) -> AppResult<()> {
        // The migration journal always lives in public, independent of the data
        // schema selected by ordinary query pools. This preserves existing installs.
        let mut connection = self.pool.acquire().await?;
        let old_path: String = sqlx::query_scalar("SHOW search_path")
            .fetch_one(&mut *connection)
            .await?;
        let old_timeout: String = sqlx::query_scalar("SHOW statement_timeout")
            .fetch_one(&mut *connection)
            .await?;
        sqlx::query("SELECT set_config('search_path', 'public', false)")
            .execute(&mut *connection)
            .await?;
        // A migration may backfill a large table (0043 reads every message once): it runs
        // without the request deadline (DM_DATABASE_STATEMENT_TIMEOUT_SECONDS), which would
        // otherwise cancel it on a large database. The services are stopped while it runs.
        sqlx::query("SELECT set_config('statement_timeout', '0', false)")
            .execute(&mut *connection)
            .await?;
        let migration_result = MIGRATOR.run(&mut *connection).await;
        let restore_result = sqlx::query(
            "SELECT set_config('search_path', $1, false), set_config('statement_timeout', $2, false)",
        )
        .bind(old_path)
        .bind(old_timeout)
        .execute(&mut *connection)
        .await;
        if restore_result.is_err() {
            connection.close_on_drop();
        }
        migration_result.map_err(|error| {
            AppError::internal(anyhow::anyhow!("database migration failed: {error}"))
        })?;
        restore_result?;
        Ok(())
    }

    /// Opens a migration-only pool and applies embedded migrations.
    ///
    /// # Errors
    ///
    /// Returns a redacted configuration, connection, or migration error.
    pub async fn migrate_from_settings(settings: &MigrationSettings) -> AppResult<()> {
        let store = Self::connect(&settings.database).await?;
        store.migrate().await?;
        store.pool.close().await;
        Ok(())
    }

    /// Confirms PostgreSQL can serve a trivial query.
    ///
    /// # Errors
    ///
    /// Returns a database error when readiness is lost.
    pub async fn health(&self) -> AppResult<()> {
        sqlx::query_scalar::<_, i32>("SELECT 1")
            .fetch_one(&self.pool)
            .await
            .map(|_| ())
            .map_err(AppError::database)
    }

    /// Confirms that every embedded migration is installed with its expected
    /// checksum and that the runtime role can read a required DM table.
    ///
    /// # Errors
    ///
    /// Returns a redacted database or schema-readiness error.
    pub async fn readiness(&self) -> AppResult<()> {
        for migration in MIGRATOR.iter() {
            let installed = sqlx::query_scalar::<_, bool>(
                r#"
                SELECT EXISTS (
                    SELECT 1
                    FROM public._sqlx_migrations
                    WHERE version = $1
                      AND success
                      AND checksum = $2
                )
                "#,
            )
            .bind(migration.version)
            .bind(migration.checksum.as_ref())
            .fetch_one(&self.pool)
            .await
            .map_err(AppError::database)?;
            if !installed {
                return Err(AppError::internal(anyhow::anyhow!(
                    "database schema is not current"
                )));
            }
        }
        sqlx::query("SELECT 1 FROM accounts LIMIT 0")
            .execute(&self.pool)
            .await
            .map(|_| ())
            .map_err(AppError::database)
    }

    /// Borrows the pool for listener and graceful-shutdown integration.
    #[must_use]
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

fn duration_as_postgres_milliseconds(duration: Duration) -> String {
    format!("{}ms", duration.as_millis())
}

/// What a refusal by the database means to the client. The domain checks every rule a
/// request can break before anything is written, so these are a backstop; the rules a
/// request can still reach get their own words, and the rest say what to do instead of
/// naming an internal invariant.
pub(crate) fn map_constraint_error(error: sqlx::Error) -> AppError {
    let Some(database) = error.as_database_error() else {
        return AppError::database(error);
    };
    match database.code().as_deref() {
        Some("23503") => AppError::not_found(
            "Something this request refers to does not exist any more (a conversation, message or account it names).",
        )
        .with_hint("Read the current state, then send the request again with what exists."),
        Some("23505") => AppError::refused(
            axum::http::StatusCode::CONFLICT,
            "conflict",
            "This already exists: another request made the same change first.",
        )
        .with_hint(
            "Read the current state. A retry with the same Idempotency-Key returns the original result.",
        ),
        Some("23514" | "22001" | "22P02") => {
            broken_rule(database.constraint(), database.message())
        }
        _ => AppError::database(error),
    }
}

/// A check the database refused (23514), a value too long for its column (22001) or in a
/// form it cannot store (22P02).
fn broken_rule(constraint: Option<&str>, message: &str) -> AppError {
    const REPORT: &str = "Check the request against openapi.yaml. If it looks right, report it with dm report (POST /api/v1/reports), quoting the X-Request-ID response header.";
    // Rules raised by triggers have no constraint name; they are known by their message.
    if message == "group name must contain an ASCII letter or digit" {
        return crate::domain::unaddressable_name("");
    }
    match constraint {
        Some("groups_name_check") => AppError::invalid(format!(
            "A group needs a name of 1 to {} characters (spaces at either end do not count).",
            crate::domain::MAX_GROUP_NAME_CHARACTERS
        )),
        Some("groups_description_check") => AppError::invalid(format!(
            "A group's description is at most {} characters.",
            crate::domain::MAX_GROUP_DESCRIPTION_CHARACTERS
        )),
        Some(rule) => AppError::invalid(format!(
            "DM could not store this change: it breaks the stored-data rule {rule}, which DM's own checks should have caught first."
        ))
        .with_hint(REPORT)
        .with_details(serde_json::json!({ "rule": rule })),
        None => AppError::invalid(
            "DM could not store this change: a value in it breaks a rule DM keeps for stored data (it is too long, or not in a form DM stores).",
        )
        .with_hint(REPORT),
    }
}
