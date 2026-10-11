//! Production process assembly and lifecycle supervision.

use std::{future::IntoFuture as _, sync::Arc};

use tokio::{net::TcpListener, time::timeout};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    AppResult,
    api::build_router,
    application::{accounts::AccountRuntime, state::AppState, sync_hub::SyncHub},
    config::{MigrationSettings, Settings},
    infrastructure::{accounts::AccountsAdapter, giphy::GiphyClient, postgres::PostgresStore},
    shutdown,
    worker::TingDeliveryWorker,
};

/// Constructs one fully validated application dependency graph.
///
/// PostgreSQL readiness is checked before the state is returned. Schema
/// migrations remain an explicit deployment step owned by `dm-migrate`.
///
/// # Errors
///
/// Returns a redacted application error when an adapter cannot be constructed,
/// PostgreSQL cannot be reached, or its readiness query fails.
pub async fn build_app_state(settings: Settings) -> AppResult<AppState> {
    let obsolete = Settings::obsolete_variables_present();
    if !obsolete.is_empty() {
        tracing::warn!(
            variables = ?obsolete,
            "these obsolete variables are set but DM no longer reads them; remove them"
        );
    }
    // rustls is built with both ring and aws-lc-rs here, so it cannot pick a process default
    // by itself; pin ring for every TLS client (PostgreSQL, Silicon Accounts, Ting).
    let _ = rustls::crypto::ring::default_provider().install_default();
    let identity = Arc::new(AccountsAdapter::new(&settings.accounts)?);
    let gifs = Arc::new(GiphyClient::new(&settings.providers)?);
    let store = PostgresStore::connect(&settings.database).await?;
    store.readiness().await?;
    Ok(AppState {
        instance_id: new_instance_id(),
        settings: Arc::new(settings),
        store,
        identity,
        accounts: AccountRuntime::default(),
        sync: SyncHub::default(),
        gifs,
    })
}

/// Runs the HTTP API until it exits or receives a process shutdown signal.
///
/// # Errors
///
/// Returns an error when dependencies cannot become ready, the listener cannot
/// bind, or Axum stops unexpectedly.
pub async fn run_api(settings: Settings) -> anyhow::Result<()> {
    let bind_address = settings.server.bind_addr;
    let shutdown_timeout = settings.server.shutdown_timeout;
    let state = build_app_state(settings).await?;

    let listener = TcpListener::bind(bind_address).await.map_err(|error| {
        tracing::error!(
            failure = "listener_bind",
            error_kind = ?error.kind(),
            "DM API failed to start"
        );
        anyhow::anyhow!("DM API listener could not bind")
    })?;
    let bound_address = listener.local_addr().unwrap_or(bind_address);
    tracing::info!(
        instance_id = %state.instance_id,
        %bound_address,
        "DM API is ready"
    );

    let cancellation = CancellationToken::new();
    let worker = build_ting_worker(&state, cancellation.clone());
    let sync_wakeups = tokio::spawn(
        state
            .sync
            .clone()
            .run(state.store.clone(), cancellation.clone()),
    );
    let reporting = tokio::spawn(crate::reporting::run(state.clone(), cancellation.clone()));
    let router = build_router(state);
    let server = axum::serve(listener, router)
        .with_graceful_shutdown(cancellation.clone().cancelled_owned())
        .into_future();
    let worker_run = worker.run(cancellation.clone());
    tokio::pin!(server);
    tokio::pin!(worker_run);

    let result = tokio::select! {
        result = &mut server => {
            cancellation.cancel();
            let worker_result = timeout(shutdown_timeout, &mut worker_run).await;
            map_server_result(result)?;
            if let Ok(result) = worker_result {
                result.map_err(anyhow::Error::from)
            } else {
                graceful_deadline_elapsed("DM API delivery worker", shutdown_timeout);
                Ok(())
            }
        },
        result = &mut worker_run => {
            cancellation.cancel();
            let server_result = timeout(shutdown_timeout, &mut server).await;
            result.map_err(anyhow::Error::from)?;
            if let Ok(result) = server_result {
                map_server_result(result)?;
            } else {
                graceful_deadline_elapsed("DM API server", shutdown_timeout);
            }
            Err(anyhow::anyhow!("DM API delivery worker stopped unexpectedly"))
        },
        () = shutdown::signal() => {
            cancellation.cancel();
            let drain = async {
                let (server_result, worker_result) = tokio::join!(&mut server, &mut worker_run);
                map_server_result(server_result)?;
                worker_result.map_err(anyhow::Error::from)
            };
            if let Ok(result) = timeout(shutdown_timeout, drain).await {
                result
            } else {
                graceful_deadline_elapsed("DM API", shutdown_timeout);
                Ok(())
            }
        }
    };
    cancellation.cancel();
    let _ = timeout(shutdown_timeout, reporting).await;
    let _ = timeout(shutdown_timeout, sync_wakeups).await;
    result
}

/// Applies embedded database migrations and closes the migration pool.
///
/// # Errors
///
/// Returns a redacted database or migration error.
pub async fn run_migrations(settings: MigrationSettings) -> AppResult<()> {
    tracing::info!("DM database migration started");
    PostgresStore::migrate_from_settings(&settings).await?;
    tracing::info!("DM database migration completed");
    Ok(())
}

/// Runs the durable delivery worker until completion or graceful shutdown.
///
/// # Errors
///
/// Returns an error when dependencies cannot become ready or the worker stops
/// with a durable-delivery failure.
pub async fn run_worker(settings: Settings) -> anyhow::Result<()> {
    let shutdown_timeout = settings.server.shutdown_timeout;
    let state = build_app_state(settings).await?;
    let cancellation = CancellationToken::new();
    let worker = build_ting_worker(&state, cancellation.clone());
    let worker_run = worker.run(cancellation.clone());
    tokio::pin!(worker_run);
    tracing::info!(instance_id = %state.instance_id, "DM delivery worker is ready");

    tokio::select! {
        result = &mut worker_run => result.map_err(anyhow::Error::from),
        () = shutdown::signal() => {
            cancellation.cancel();
            if let Ok(result) = timeout(shutdown_timeout, &mut worker_run).await {
                result.map_err(anyhow::Error::from)
            } else {
                graceful_deadline_elapsed("DM worker", shutdown_timeout);
                Ok(())
            }
        }
    }
}

fn new_instance_id() -> Arc<str> {
    Arc::from(Uuid::now_v7().to_string())
}

fn map_server_result(result: std::io::Result<()>) -> anyhow::Result<()> {
    result.map_err(|error| {
        tracing::error!(
            failure = "server_runtime",
            error_kind = ?error.kind(),
            "DM API stopped unexpectedly"
        );
        anyhow::anyhow!("DM API server stopped unexpectedly")
    })
}

fn graceful_deadline_elapsed(component: &str, deadline: std::time::Duration) {
    tracing::warn!(
        component,
        deadline_seconds = deadline.as_secs(),
        "graceful-shutdown deadline elapsed"
    );
}

/// Builds the Ting worker: a publisher over a prewarmed socket when `DM_TING_URL` is set,
/// otherwise a worker that marks handoffs discarded. Never owns recipient-facing sockets.
pub(crate) fn build_ting_worker(
    state: &AppState,
    cancellation: CancellationToken,
) -> TingDeliveryWorker {
    use crate::infrastructure::{ting::TingSocket, ting_publisher::ProofTingPublisher};
    let publisher = state.settings.ting.base_url.clone().map(|base_url| {
        let socket = TingSocket::start(base_url, &state.settings.ting, cancellation);
        Arc::new(ProofTingPublisher::new(
            state.identity.clone(),
            socket,
            state.settings.ting.app_id.clone(),
        )) as crate::infrastructure::ting::SharedTingPublisher
    });
    TingDeliveryWorker::new(state.clone(), publisher)
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::{map_server_result, new_instance_id};

    #[test]
    fn instance_ids_are_uuid_v7() {
        assert!(Uuid::parse_str(&new_instance_id()).is_ok_and(|id| id.get_version_num() == 7));
    }

    #[test]
    fn listener_errors_are_redacted_at_the_process_boundary() {
        let result = map_server_result(Err(std::io::Error::other(
            "postgres://user:password@example.invalid/database",
        )));
        assert!(result.is_err_and(|error| !error.to_string().contains("password")));
    }
}
