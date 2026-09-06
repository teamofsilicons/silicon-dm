//! Transient actor availability and activity.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use super::ActorId;

/// Aggregate online state.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    /// At least one unexpired connection is active.
    Online,
    /// No active connection is known.
    Offline,
}

/// Ephemeral user activity.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "presence_activity", rename_all = "snake_case")]
pub enum Activity {
    /// Composing text.
    Typing,
    /// Recording a voice attachment.
    RecordingVoice,
    /// Waiting for speech-to-text.
    TranscribingVoice,
    /// Uploading a file through the caller's chosen provider.
    UploadingFile,
    /// Searching for GIFs.
    SearchingGifs,
}

/// Public presence snapshot.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Presence {
    /// Actor whose presence is described.
    pub actor_id: ActorId,
    /// Aggregate availability.
    pub availability: Availability,
    /// Most recently updated activity among active connections.
    pub activity: Option<Activity>,
    /// Last transition to an offline state.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub last_seen_at: Option<OffsetDateTime>,
}
