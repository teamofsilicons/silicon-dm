//! PostgreSQL pool and durable store.

mod bundles;
mod conversations;
mod delivery;
mod directory;
mod drafts;
mod idempotency;
mod messages;
mod presence;
mod rows;

use std::time::Duration;

use secrecy::ExposeSecret as _;
use sqlx::{PgPool, migrate::Migrator, postgres::PgPoolOptions};

use crate::{
    AppError, AppResult,
    config::{DatabaseSettings, MigrationSettings},
};

pub use delivery::DeliveryClaim;

static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

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
        let statement_timeout = settings.statement_timeout;
        let pool = PgPoolOptions::new()
            .max_connections(settings.max_connections.get())
            .min_connections(settings.min_connections)
            .acquire_timeout(settings.acquire_timeout)
            .after_connect(move |connection, _metadata| {
                Box::pin(async move {
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
        MIGRATOR.run(&self.pool).await.map_err(|error| {
            AppError::internal(anyhow::anyhow!("database migration failed: {error}"))
        })
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
        sqlx::query("SELECT 1 FROM dm.organization_snapshots LIMIT 0")
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
