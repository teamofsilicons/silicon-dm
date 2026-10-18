//! A pooled connection handed back inside a transaction is never reused.
//!
//! A request cancelled while sqlx's `begin()` still waited for BEGIN's answer (a client that went away) returns its
//! connection inside that transaction, because sqlx arms its rollback only once BEGIN is answered. Reused, the next
//! request's autocommit statements would join that transaction: they would keep their row locks (every other request
//! of the account then waits out the statement timeout) and their writes would be lost if it rolled back. The pool
//! closes such a connection instead. Here a raw `BEGIN` stands in for the cancelled `begin()`, which cannot be timed
//! deterministically.

mod support;

use std::{num::NonZeroU32, time::Duration};

use secrecy::SecretString;
use silicon_dm::{config::DatabaseSettings, infrastructure::postgres::PostgresStore};
use sqlx::Row as _;
use support::{BoxError, TestDatabase};

#[tokio::test]
async fn a_connection_handed_back_inside_a_transaction_is_closed_not_reused() -> Result<(), BoxError>
{
    let database = TestDatabase::start().await?;
    let settings = DatabaseSettings {
        url: SecretString::from(database.url.clone()),
        // One connection, so the next acquire would get the very same one if the pool kept it.
        max_connections: NonZeroU32::new(1).ok_or("pool size")?,
        min_connections: 0,
        acquire_timeout: Duration::from_secs(10),
        statement_timeout: Duration::from_secs(10),
    };
    let store = PostgresStore::connect_schema(&settings, "public").await?;
    let pool = store.pool();

    let leaked_backend: i32 = {
        let mut connection = pool.acquire().await?;
        sqlx::query("BEGIN").execute(&mut *connection).await?;
        sqlx::query("CREATE TABLE written_inside_the_leaked_transaction (x int)")
            .execute(&mut *connection)
            .await?;
        sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *connection)
            .await?
        // Dropped here, inside the transaction, as a cancelled request would leave it.
    };

    let mut connection = pool.acquire().await?;
    let backend: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *connection)
        .await?;
    assert_ne!(
        backend, leaked_backend,
        "the connection left inside a transaction was reused"
    );
    // One simple-protocol statement outside a transaction is its own transaction: both timestamps are equal.
    let outside_any_transaction: bool =
        sqlx::raw_sql("SELECT transaction_timestamp() = statement_timestamp()")
            .fetch_one(&mut *connection)
            .await?
            .try_get(0)?;
    assert!(
        outside_any_transaction,
        "the next request ran inside the leaked transaction"
    );
    let table: Option<String> =
        sqlx::query_scalar("SELECT to_regclass('written_inside_the_leaked_transaction')::text")
            .fetch_one(&mut *connection)
            .await?;
    assert_eq!(
        table, None,
        "closing the connection rolls back what was written inside the leak"
    );

    // An ordinary connection, used and handed back outside any transaction, is kept.
    drop(connection);
    let mut connection = pool.acquire().await?;
    let again: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *connection)
        .await?;
    assert_eq!(again, backend, "a clean connection is reused");

    // The backstop is set on every connection.
    let idle_timeout: String = sqlx::query_scalar("SHOW idle_in_transaction_session_timeout")
        .fetch_one(&mut *connection)
        .await?;
    assert_eq!(idle_timeout, "1min");

    drop(connection);
    pool.close().await;
    database.close().await
}
