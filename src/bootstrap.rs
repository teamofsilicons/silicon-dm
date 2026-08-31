//! Production process assembly and lifecycle supervision.

use std::{future::IntoFuture as _, sync::Arc};

use tokio::{net::TcpListener, time::timeout};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    AppResult,
    api::build_router,
    application::state::AppState,
    config::{MigrationSettings, Settings},
    infrastructure::{
        briefcase::BriefcaseClient, giphy::GiphyClient, iam::IamClient, postgres::PostgresStore,
        waveform::WaveformClient,
    },
    realtime::RealtimeHub,
    shutdown,
    worker::DeliveryWorker,
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
    let identity = Arc::new(IamClient::new(&settings.iam)?);
    let attachments = Arc::new(BriefcaseClient::new(&settings.providers)?);
    let transcription = Arc::new(WaveformClient::new(&settings.providers)?);
    let gifs = Arc::new(GiphyClient::new(&settings.providers)?);
    let store = PostgresStore::connect(&settings.database).await?;
    store.readiness().await?;

    Ok(AppState {
        instance_id: new_instance_id(),
        settings: Arc::new(settings),
        store,
        identity,
        attachments,
        transcription,
        gifs,
        realtime: RealtimeHub::default(),
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

    let worker = DeliveryWorker::new(
        state.store.clone(),
        state.realtime.clone(),
        Arc::clone(&state.instance_id),
        state.settings.worker.clone(),
    );
    let router = build_router(state);
    let cancellation = CancellationToken::new();
    let server = axum::serve(listener, router)
        .with_graceful_shutdown(cancellation.clone().cancelled_owned())
        .into_future();
    let worker_run = worker.run(cancellation.clone());
    tokio::pin!(server);
    tokio::pin!(worker_run);

    tokio::select! {
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
    }
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
    let worker = DeliveryWorker::new(
        state.store.clone(),
        state.realtime.clone(),
        Arc::clone(&state.instance_id),
        state.settings.worker.clone(),
    );
    let cancellation = CancellationToken::new();
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
