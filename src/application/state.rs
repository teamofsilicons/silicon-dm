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
    /// Nonblocking observability exporter.
    pub telemetry: crate::telemetry::Recorder,
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

impl AppState {
    /// Immutable application/environment binding for delivery and credential storage.
    #[must_use]
    pub fn ting_delivery_context(&self) -> crate::infrastructure::postgres::TingDeliveryContext {
        crate::infrastructure::postgres::TingDeliveryContext {
            app_id: self.settings.iam.app_id.clone(),
            testing_environment_id: self.testing_environment,
            testing_generation: self.testing_generation,
        }
    }

    /// Opens an encrypted access-token cache in the selected data plane.
    /// The configured secret is encryption key material only; authorization is
    /// always supplied by the selected identity adapter and reverified by IAM.
    ///
    /// # Errors
    /// Rejects invalid encryption material or environment binding.
    pub fn ting_credentials(
        &self,
    ) -> crate::AppResult<crate::infrastructure::ting_credentials::TingCredentialCache> {
        crate::infrastructure::ting_credentials::TingCredentialCache::new(
            self.store.clone(),
            &self.settings.iam.app_secret,
            self.ting_delivery_context(),
        )
    }
}
