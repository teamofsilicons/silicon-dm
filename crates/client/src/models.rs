//! Public wire types, independent of the server implementation.
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use uuid::Uuid;

/// JSON object that is preserved with every message and draft.
pub type Metadata = Map<String, Value>;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActorType {
    Carbon,
    Silicon,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Actor {
    #[serde(rename = "type")]
    pub actor_type: ActorType,
    pub id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Attachment {
    pub permanent_url: url::Url,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VoiceAttachment {
    #[serde(flatten)]
    pub attachment: Attachment,
    /// Required for writes; historical server records can contain null.
    pub duration_milliseconds: Option<u64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Gif {
    pub provider_id: String,
    pub url: url::Url,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_url: Option<url::Url>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MessageCreate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_id: Option<String>,
    /// Optional recipient address, for example deliberate@cos:tos.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipient_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(rename = "message", alias = "text")]
    pub text: Option<String>,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<VoiceAttachment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_transcript: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gif: Option<Gif>,
    #[serde(default)]
    pub metadata: Metadata,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to_message_id: Option<Uuid>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageStatus {
    Waiting,
    Sent,
    Delivered,
    Read,
    Failed,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptStatus {
    Delivered,
    Read,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    pub id: Uuid,
    pub conversation_id: Uuid,
    pub sender: Actor,
    pub sequence: i64,
    pub status: MessageStatus,
    #[serde(flatten)]
    pub content: MessageCreate,
    pub created_at: String,
    #[serde(default)]
    pub delivered_at: Option<String>,
    #[serde(default)]
    pub read_at: Option<String>,
    #[serde(default)]
    pub failure_reason: Option<String>,
    #[serde(default)]
    pub bundle: Option<BundleRef>,
    #[serde(default)]
    pub version: Option<i64>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub deleted_at: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BundleRef {
    pub id: Uuid,
    pub role: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Conversation {
    pub id: Uuid,
    pub org_id: String,
    pub participants: Vec<Actor>,
    pub last_message: Option<Message>,
    pub created_at: String,
    pub updated_at: String,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PageRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u16>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    #[serde(default)]
    pub next_cursor: Option<String>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DraftInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_content: Option<String>,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<VoiceAttachment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_transcript: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gif: Option<Gif>,
    #[serde(default)]
    pub metadata: Metadata,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to_message_id: Option<Uuid>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Draft {
    pub conversation_id: Uuid,
    pub actor_id: String,
    pub version: i64,
    #[serde(flatten)]
    pub content: DraftInput,
    pub updated_at: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BundleCreate {
    pub message_ids: Vec<Uuid>,
    pub display_message: MessageCreate,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Bundle {
    pub id: Uuid,
    pub conversation_id: Uuid,
    pub original_message_ids: Vec<Uuid>,
    pub display_message: Message,
    pub created_by: Actor,
    pub created_at: String,
    #[serde(default)]
    pub original_messages: Vec<Message>,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Activity {
    Typing,
    RecordingVoice,
    TranscribingVoice,
    UploadingFile,
    SearchingGifs,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Presence {
    pub actor_id: String,
    pub availability: String,
    pub activity: Option<Activity>,
    pub last_seen_at: Option<String>,
}
/// Tokens are deliberately not Debug; callers explicitly own their persistence.
#[derive(Clone, Serialize, Deserialize)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: String,
    pub token_type: String,
    pub expires_in: i64,
    pub scope: String,
    pub actor: Actor,
    pub organization_id: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Identity {
    pub actor: Actor,
    pub organization_id: String,
    pub principal_id: String,
    pub session_id: Option<String>,
    pub org_role: Option<String>,
    pub capabilities: Vec<String>,
}
/// Secrets are deliberately not Debug.
#[derive(Clone, Serialize, Deserialize)]
pub struct TestEnvironmentCreate {
    pub name: String,
    pub description: Option<String>,
    pub iam_environment_id: Uuid,
    pub iam_environment_key: String,
    pub iam_app_id: String,
    pub iam_app_secret: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iam_webhook_secret: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iam_webhook_key_version: Option<i64>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TestEnvironmentUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct TestEnvironment {
    pub environment_id: Uuid,
    pub organization_id: String,
    pub creator_actor_id: String,
    pub creator_actor_kind: String,
    pub iam_app_id: String,
    pub version: i64,
    pub name: String,
    pub description: Option<String>,
    pub status: String,
    pub iam_environment_id: Uuid,
    pub created_at: String,
    pub last_activity_at: String,
    pub deleted_at: Option<String>,
    pub purge_after: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_key: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct TestEnvironmentKey {
    pub environment_id: Uuid,
    pub root_key: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "type",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ClientFrame {
    Pong {
        ping_id: String,
    },
    Ack {
        actor_id: String,
        through_sequence: i64,
    },
    Resume {
        actor_id: String,
        after_sequence: i64,
    },
    Presence {
        actor_id: String,
        activity: Option<Activity>,
    },
    Receipt {
        actor_id: String,
        conversation_id: Uuid,
        message_id: Uuid,
        status: ReceiptStatus,
        device_id: String,
    },
    #[serde(rename = "new_message")]
    SendMessage {
        actor_id: String,
        org_id: String,
        conversation_id: Uuid,
        idempotency_key: String,
        #[serde(flatten)]
        message: Box<MessageCreate>,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ServerFrame {
    Ready {
        protocol_version: u16,
        connection_id: Uuid,
        actors: Vec<String>,
        acknowledged_through: BTreeMap<String, i64>,
        #[serde(default)]
        testing_generation: Option<i64>,
    },
    Ping {
        ping_id: String,
    },
    MessageAccepted {
        idempotency_key: String,
        #[serde(flatten)]
        message: Box<Message>,
    },
    ReceiptRecorded {
        message_id: Uuid,
        status: ReceiptStatus,
    },
    #[serde(rename = "new_message")]
    Message {
        delivery_id: Uuid,
        actor_id: String,
        delivery_sequence: i64,
        #[serde(flatten)]
        message: Box<Message>,
    },
    Receipt {
        delivery_id: Uuid,
        actor_id: String,
        delivery_sequence: i64,
        message_id: Uuid,
        status: MessageStatus,
    },
    Error {
        code: String,
        message: String,
        recoverable: bool,
    },
}
impl ServerFrame {
    pub fn delivery_position(&self) -> Option<(Uuid, &str, i64)> {
        match self {
            Self::Message {
                delivery_id,
                actor_id,
                delivery_sequence,
                ..
            }
            | Self::Receipt {
                delivery_id,
                actor_id,
                delivery_sequence,
                ..
            } => Some((*delivery_id, actor_id, *delivery_sequence)),
            _ => None,
        }
    }
}

/// Public application discovery. Application secrets are never returned.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IamInfo {
    pub app_id: String,
    pub iam_base_url: String,
    pub api_base_url: String,
}
