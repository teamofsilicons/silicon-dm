//! Isolated PostgreSQL for delivery integration tests, locally and in CI.

use std::{env, error::Error};
use testcontainers::{ContainerAsync, ImageExt as _, runners::AsyncRunner as _};
use testcontainers_modules::postgres::Postgres;

pub struct TestDatabase {
    pub url: String,
    _container: Option<ContainerAsync<Postgres>>,
}

impl TestDatabase {
    pub async fn start() -> Result<Self, Box<dyn Error + Send + Sync>> {
        match env::var("DM_TEST_DATABASE_URL") {
            // Explicit native fixtures retain the existing contract: the
            // operator provides a fresh disposable database for each scenario.
            Ok(url) => Ok(Self {
                url,
                _container: None,
            }),
            Err(env::VarError::NotPresent) => {
                let container = Postgres::default().with_tag("16-alpine").start().await?;
                let host = container.get_host().await?;
                let port = container.get_host_port_ipv4(5432).await?;
                Ok(Self {
                    url: format!("postgres://postgres:postgres@{host}:{port}/postgres"),
                    _container: Some(container),
                })
            }
            Err(error) => Err(Box::new(error)),
        }
    }
}
