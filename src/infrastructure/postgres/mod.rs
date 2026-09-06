//! PostgreSQL pool and durable store.

mod bundles;
mod conversations;
mod delivery;
mod directory;
mod drafts;
mod iam_directory;
mod idempotency;
mod messages;
mod presence;
mod revisions;
mod rows;

use std::time::Duration;

use secrecy::ExposeSecret as _;
use sqlx::{Connection as _, PgPool, migrate::Migrator, postgres::PgPoolOptions};

use crate::{
    AppError, AppResult,
    config::{DatabaseSettings, MigrationSettings},
};

pub use delivery::DeliveryClaim;

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
                    Ok(true)
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
                        .execute(connection)
                        .await?;
                    Ok(())
                })
            })
            .connect(settings.url.expose_secret())
            .await
            .map_err(AppError::Database)?;
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
        sqlx::query("SELECT set_config('search_path', 'public', false)")
            .execute(&mut *connection)
            .await?;
        let migration_result = MIGRATOR.run(&mut *connection).await;
        let restore_result = sqlx::query("SELECT set_config('search_path', $1, false)")
            .bind(old_path)
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
            .map_err(AppError::Database)
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
            .map_err(AppError::Database)?;
            if !installed {
                return Err(AppError::internal(anyhow::anyhow!(
                    "database schema is not current"
                )));
            }
        }
        sqlx::query("SELECT 1 FROM organization_snapshots LIMIT 0")
            .execute(&self.pool)
            .await
            .map(|_| ())
            .map_err(AppError::Database)
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

pub(crate) fn map_constraint_error(error: sqlx::Error) -> AppError {
    let Some(database) = error.as_database_error() else {
        return AppError::Database(error);
    };
    match database.code().as_deref() {
        Some("23503") => AppError::NotFound,
        Some("23505") => AppError::conflict("resource already exists"),
        Some("23514" | "22001" | "22P02") => {
            AppError::validation("request violates a durable data invariant")
        }
        _ => AppError::Database(error),
    }
}
