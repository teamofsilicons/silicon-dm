//! Shared application dependencies.

use std::sync::Arc;

use super::ports::{GifProvider, IdentityProvider};
use crate::{config::Settings, infrastructure::postgres::PostgresStore, realtime::RealtimeHub};

/// Cheaply cloneable dependency container used by handlers and workers.
#[derive(Clone)]
pub struct AppState {
    /// Unique process identity used for leases and traces.
    pub instance_id: Arc<str>,
    /// Validated immutable settings.
    pub settings: Arc<Settings>,
    /// Durable storage.
    pub store: PostgresStore,
    /// IAM adapter.
    pub identity: Arc<dyn IdentityProvider>,
    /// Production registry for test environment lifecycle and routing.
    pub testing: Option<Arc<crate::testing::TestingRegistry>>,
    /// Selected test environment, absent for production.
    pub testing_environment: Option<uuid::Uuid>,
    /// Lifecycle generation captured when a request or connection was admitted.
    pub testing_generation: Option<i64>,
    /// Giphy adapter.
    pub gifs: Arc<dyn GifProvider>,
    /// Process-local realtime connection registry.
    pub realtime: RealtimeHub,
}
