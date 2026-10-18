//! Shared application dependencies.

use std::sync::Arc;

use super::{
    accounts::AccountRuntime,
    ports::{GifProvider, IdentityProvider},
};
use crate::{config::Settings, infrastructure::postgres::PostgresStore};

/// Cheaply cloneable dependency container used by handlers and workers.
#[derive(Clone)]
pub struct AppState {
    /// Unique process identity used for leases and traces.
    pub instance_id: Arc<str>,
    /// Validated immutable settings.
    pub settings: Arc<Settings>,
    /// Durable storage.
    pub store: PostgresStore,
    /// Silicon Accounts.
    pub identity: Arc<dyn IdentityProvider>,
    /// Process-local accounts throttles.
    pub accounts: AccountRuntime,
    /// Wakes long-polling sync requests.
    pub sync: super::sync_hub::SyncHub,
    /// Giphy adapter.
    pub gifs: Arc<dyn GifProvider>,
    /// Speech and transcription (Deepgram), when configured.
    pub speech: super::speech::SpeechService,
}

impl AppState {
    /// Producer identity sealed into outgoing Ting requests.
    #[must_use]
    pub fn ting_delivery_context(&self) -> crate::infrastructure::postgres::TingDeliveryContext {
        crate::infrastructure::postgres::TingDeliveryContext {
            app_id: self.settings.accounts.app_id.clone(),
        }
    }

    /// Whether DM hands updates to Ting (only when `DM_TING_URL` is set).
    #[must_use]
    pub fn ting_enabled(&self) -> bool {
        self.settings.ting.base_url.is_some()
    }
}
