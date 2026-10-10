//! Isolated PostgreSQL for integration tests, locally and in CI.
//!
//! Choose one, in this order:
//! - `DM_TEST_POSTGRES_URL`: a server URL whose role may create databases (for example
//!   `postgres://postgres@127.0.0.1:5460/postgres`). Every test gets its own fresh database
//!   named `dm_t_<random>`, dropped again by [`TestDatabase::close`].
//! - `DM_TEST_DATABASE_URL`: one fresh, empty, disposable database for this test process.
//! - neither: a `postgres:16-alpine` testcontainer (needs Docker).
#![allow(
    dead_code,
    reason = "each integration test binary uses a different subset"
)]

pub mod accounts;
pub mod app;

use std::{env, error::Error};
use testcontainers::{ContainerAsync, ImageExt as _, runners::AsyncRunner as _};
use testcontainers_modules::postgres::Postgres;

pub type BoxError = Box<dyn Error + Send + Sync>;

pub struct TestDatabase {
    pub url: String,
    created: Option<(String, String)>,
    _container: Option<ContainerAsync<Postgres>>,
}

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");
const LEGACY_FIXTURE: &str = include_str!("../fixtures/legacy-0038.sql");

impl TestDatabase {
    /// A database migrated to 0038, loaded with the dataset the 0.14.1 code wrote
    /// (`tests/fixtures/legacy-0038.sql`), then upgraded to the latest schema.
    pub async fn upgraded_from_0038() -> Result<Self, BoxError> {
        use sqlx::Connection as _;
        let database = Self::start().await?;
        let mut connection = sqlx::PgConnection::connect(&database.url).await?;
        sqlx::query("SET search_path=public")
            .execute(&mut connection)
            .await?;
        MIGRATOR.run_to(38, &mut connection).await?;
        sqlx::raw_sql(LEGACY_FIXTURE)
            .execute(&mut connection)
            .await?;
        sqlx::query("SET search_path=public")
            .execute(&mut connection)
            .await?;
        MIGRATOR.run(&mut connection).await?;
        connection.close().await?;
        Ok(database)
    }

    pub async fn start() -> Result<Self, BoxError> {
        if let Ok(server) = env::var("DM_TEST_POSTGRES_URL") {
            let name = format!("dm_t_{}", uuid::Uuid::new_v4().simple());
            let admin = sqlx::PgPool::connect(&server).await?;
            sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
                .execute(&admin)
                .await?;
            admin.close().await;
            let mut url = url::Url::parse(&server)?;
            url.set_path(&format!("/{name}"));
            return Ok(Self {
                url: url.to_string(),
                created: Some((server, name)),
                _container: None,
            });
        }
        match env::var("DM_TEST_DATABASE_URL") {
            // Explicit native fixtures retain the existing contract: the
            // operator provides a fresh disposable database for each scenario.
            Ok(url) => Ok(Self {
                url,
                created: None,
                _container: None,
            }),
            Err(env::VarError::NotPresent) => {
                let container = Postgres::default().with_tag("16-alpine").start().await?;
                let host = container.get_host().await?;
                let port = container.get_host_port_ipv4(5432).await?;
                Ok(Self {
                    url: format!("postgres://postgres:postgres@{host}:{port}/postgres"),
                    created: None,
                    _container: Some(container),
                })
            }
            Err(error) => Err(Box::new(error)),
        }
    }

    /// Drops a database this helper created. Call it at the end of a passing test; a failed
    /// test leaves its `dm_t_*` database behind for inspection.
    pub async fn close(self) -> Result<(), BoxError> {
        if let Some((server, name)) = self.created {
            let admin = sqlx::PgPool::connect(&server).await?;
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "DROP DATABASE IF EXISTS {name} WITH (FORCE)"
            )))
            .execute(&admin)
            .await?;
            admin.close().await;
        }
        Ok(())
    }
}
