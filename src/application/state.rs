//! Shared application dependencies.

use std::sync::Arc;

use super::ports::{AttachmentProvider, GifProvider, IdentityProvider, TranscriptionProvider};
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
    /// Briefcase adapter.
    pub attachments: Arc<dyn AttachmentProvider>,
    /// Waveform adapter.
    pub transcription: Arc<dyn TranscriptionProvider>,
    /// Giphy adapter.
    pub gifs: Arc<dyn GifProvider>,
    /// Process-local realtime connection registry.
    pub realtime: RealtimeHub,
}
