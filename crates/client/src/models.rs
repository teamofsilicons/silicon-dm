//! Public wire types, independent of the server implementation.
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Reply {
    #[serde(rename = "message-id")]
    pub message_id: String,
    pub sender: Actor,
    pub content: Option<ReplyContent>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReplyContent {
    pub message: Option<String>,
    pub attachments: Vec<String>,
    pub voice_transcript: Option<String>,
}

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

#[derive(Clone, Debug, Serialize)]
pub struct Attachment {
    pub permanent_url: url::Url,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}
impl<'de> Deserialize<'de> for Attachment {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Details {
            permanent_url: url::Url,
            #[serde(default)]
            name: Option<String>,
            #[serde(default)]
            content_type: Option<String>,
            #[serde(default)]
            size: Option<u64>,
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Input {
            Link(url::Url),
            Details(Details),
        }
        Ok(match Input::deserialize(deserializer)? {
            Input::Link(permanent_url) => Self {
                permanent_url,
                name: None,
                content_type: None,
                size: None,
            },
            Input::Details(d) => Self {
                permanent_url: d.permanent_url,
                name: d.name,
                content_type: d.content_type,
                size: d.size,
            },
        })
    }
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
#[derive(Clone, Debug, Default)]
pub struct MessageCreate {
    pub sender_id: Option<String>,
    /// Optional recipient address, for example deliberate@si:cos.
    pub recipient_id: Option<String>,
    pub text: Option<String>,
    pub attachments: Vec<Attachment>,
    pub voice: Option<VoiceAttachment>,
    pub voice_transcript: Option<String>,
    pub gif: Option<Gif>,
    pub metadata: Metadata,
    pub reply_to_message_id: Option<String>,
}
#[derive(Deserialize)]
#[serde(remote = "MessageCreate")]
struct MessageCreateWire {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_id: Option<String>,
    /// Optional recipient address, for example deliberate@si:cos.
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
    pub reply_to_message_id: Option<String>,
}
impl<'de> Deserialize<'de> for MessageCreate {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut value = Value::deserialize(deserializer)?;
        if let Some(reply) = value.get("reply").filter(|r| !r.is_null()) {
            let id = reply
                .get("message-id")
                .and_then(Value::as_str)
                .ok_or_else(|| serde::de::Error::custom("reply requires message-id"))?
                .to_owned();
            value["reply_to_message_id"] = Value::String(id);
        }
        MessageCreateWire::deserialize(value).map_err(serde::de::Error::custom)
    }
}
impl Serialize for MessageCreate {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut attachments = self
            .attachments
            .iter()
            .map(|a| a.permanent_url.to_string())
            .collect::<Vec<_>>();
        if let Some(voice) = &self.voice {
            attachments.push(voice.attachment.permanent_url.to_string());
        }
        if let Some(gif) = &self.gif {
            attachments.push(gif.url.to_string());
        }
        let mut value = serde_json::json!({"message":self.text.as_deref().unwrap_or(""),"attachments":attachments,"voice_transcript":self.voice_transcript,"reply":self.reply_to_message_id.as_ref().map(|id|serde_json::json!({"message-id":id}))});
        if let Some(id) = &self.sender_id {
            value["sender_id"] = Value::String(id.clone());
        }
        if let Some(id) = &self.recipient_id {
            value["recipient_id"] = Value::String(id.clone());
        }
        value.serialize(serializer)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum MessageStatus {
    Waiting,
    #[default]
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
#[derive(Clone, Debug)]
pub struct Message {
    pub id: String,
    pub conversation_id: String,
    pub sender: Actor,
    pub sequence: i64,
    pub status: MessageStatus,
    pub content: MessageCreate,
    pub reply: Option<Reply>,
    pub created_at: String,
    pub delivered_at: Option<String>,
    pub read_at: Option<String>,
    pub failure_reason: Option<String>,
    pub bundle: Option<BundleRef>,
    pub history: Vec<Value>,
    pub updated_at: Option<String>,
    pub deleted_at: Option<String>,
}
#[derive(Deserialize)]
#[serde(remote = "Message")]
struct MessageWire {
    #[serde(rename = "message-id", alias = "id")]
    pub id: String,
    pub conversation_id: String,
    pub sender: Actor,
    #[serde(default, skip_serializing)]
    pub sequence: i64,
    #[serde(default, skip_serializing)]
    pub status: MessageStatus,
    #[serde(flatten)]
    pub content: MessageCreate,
    #[serde(default)]
    pub reply: Option<Reply>,
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
    pub history: Vec<Value>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub deleted_at: Option<String>,
}
impl<'de> Deserialize<'de> for Message {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut message = MessageWire::deserialize(deserializer)?;
        if let Some(sequence) = silicon_dm_protocol::message_sequence(&message.id) {
            message.sequence = sequence;
        }
        if message.read_at.is_some() {
            message.status = MessageStatus::Read;
        } else if message.delivered_at.is_some() {
            message.status = MessageStatus::Delivered;
        }
        if let Some(reply) = &message.reply {
            message.content.reply_to_message_id = Some(reply.message_id.clone());
        }
        Ok(message)
    }
}
impl Serialize for Message {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut attachments = self
            .content
            .attachments
            .iter()
            .map(|a| a.permanent_url.to_string())
            .collect::<Vec<_>>();
        if let Some(voice) = &self.content.voice {
            attachments.push(voice.attachment.permanent_url.to_string());
        }
        if let Some(gif) = &self.content.gif {
            attachments.push(gif.url.to_string());
        }
        let deleted = self.deleted_at.is_some();
        if deleted {
            attachments.clear();
        }
        serde_json::json!({"message-id":self.id,"conversation_id":self.conversation_id,
            "recipient_id":self.content.recipient_id,"sender":self.sender,
            "message":if deleted {None} else {Some(self.content.text.as_deref().unwrap_or(""))},
            "attachments":attachments,"voice_transcript":if deleted {None} else {self.content.voice_transcript.as_deref()},
            "reply":self.reply,"bundle":self.bundle,"history":self.history,"created_at":self.created_at,"updated_at":self.updated_at,
            "deleted_at":self.deleted_at,"delivered_at":self.delivered_at,"read_at":self.read_at
        }).serialize(serializer)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BundleRef {
    pub id: String,
    pub role: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GroupSettings {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub is_public: bool,
    #[serde(default)]
    pub tag_ids: Vec<Uuid>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GroupCreate {
    #[serde(flatten)]
    pub settings: GroupSettings,
    #[serde(default)]
    pub member_ids: Vec<String>,
}
#[derive(Clone, Debug, Deserialize)]
pub struct GroupDetails {
    #[serde(flatten)]
    pub settings: GroupSettings,
    #[serde(default)]
    pub version: i64,
    #[serde(default)]
    pub invited_members: Vec<Actor>,
}
impl Serialize for GroupDetails {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut value = serde_json::to_value(&self.settings).map_err(serde::ser::Error::custom)?;
        if self.version == 0 {
            if let Some(object) = value.as_object_mut() {
                object.remove("is_public");
            }
        } else {
            value["version"] = serde_json::json!(self.version);
            value["invited_members"] = serde_json::json!(self.invited_members);
        }
        value.serialize(serializer)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Conversation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<GroupDetails>,
    pub id: String,
    pub org_id: String,
    pub participants: Vec<Actor>,
    pub last_message: Option<Message>,
    #[serde(default)]
    pub last_message_status: Option<MessageStatus>,
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
    pub reply_to_message_id: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Draft {
    pub conversation_id: String,
    #[serde(rename = "member_id", alias = "actor_id")]
    pub actor_id: String,
    pub version: i64,
    #[serde(flatten)]
    pub content: DraftInput,
    pub updated_at: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BundleCreate {
    pub message_ids: Vec<String>,
    pub display_message: MessageCreate,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Bundle {
    pub id: String,
    pub conversation_id: String,
    pub original_message_ids: Vec<String>,
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
    #[serde(rename = "member_id", alias = "actor_id")]
    pub actor_id: String,
    pub availability: String,
    pub activity: Option<Activity>,
    pub last_seen_at: Option<String>,
}
/// Device presence is an HTTP lease, independent of Ting delivery transport.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PresenceLease {
    pub presence: Presence,
    pub lease_expires_at: String,
    pub activity_expires_at: Option<String>,
}

/// Explicit Ting grant for the authenticated recipient. It does not log in to Ting.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeliveryRegistration {
    pub id: String,
    pub app_id: String,
    #[serde(rename = "for")]
    pub recipient: String,
    pub active: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SyncRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u16>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reset: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncEventKind {
    Message,
    MessageStatus,
}

/// Authoritative reference; fetch the current message under current permissions.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SyncEvent {
    pub event_id: Uuid,
    pub sequence: i64,
    #[serde(rename = "type")]
    pub kind: SyncEventKind,
    pub conversation_id: String,
    pub message_id: String,
}

/// `cursor` remains durable on the last page; `has_more` binds a fixed scan boundary.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SyncPage {
    pub events: Vec<SyncEvent>,
    pub cursor: String,
    pub has_more: bool,
    pub upper_sequence: i64,
    pub testing_environment_id: Option<Uuid>,
    pub testing_generation: Option<i64>,
}
/// Tokens are deliberately not Debug; callers explicitly own their persistence.
#[derive(Clone, Serialize, Deserialize)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: String,
    pub token_type: String,
    pub expires_in: i64,
    pub scope: String,
    #[serde(rename = "member", alias = "actor")]
    pub actor: Actor,
    pub organization_id: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Identity {
    #[serde(rename = "member", alias = "actor")]
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
    #[serde(rename = "creator_member_id", alias = "creator_actor_id")]
    pub creator_actor_id: String,
    #[serde(rename = "creator_member_kind", alias = "creator_actor_kind")]
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
    #[serde(rename = "ping.success")]
    Pong { ping_id: String },
    Ack {
        #[serde(rename = "member_id")]
        actor_id: String,
        through_sequence: i64,
    },
    Resume {
        #[serde(rename = "member_id")]
        actor_id: String,
        after_sequence: i64,
    },
    Presence {
        #[serde(rename = "member_id")]
        actor_id: String,
        activity: Option<Activity>,
    },
    Receipt {
        #[serde(rename = "member_id")]
        actor_id: String,
        conversation_id: String,
        message_id: String,
        status: ReceiptStatus,
        device_id: String,
    },
    #[serde(rename = "message.create")]
    SendMessage {
        #[serde(rename = "member_id")]
        actor_id: String,
        org_id: String,
        conversation_id: String,
        idempotency_key: String,
        #[serde(flatten)]
        message: Box<MessageCreate>,
    },
    #[serde(rename = "bundle")]
    CreateBundle {
        #[serde(rename = "member_id")]
        actor_id: String,
        org_id: String,
        conversation_id: String,
        idempotency_key: String,
        #[serde(flatten)]
        bundle: Box<BundleCreate>,
    },
    #[serde(rename = "ping.error")]
    PingError {
        ping_id: String,
        code: String,
        message: String,
        recoverable: bool,
    },
}
#[derive(Clone, Debug)]
pub enum ServerFrame {
    Ready {
        protocol_version: u16,
        connection_id: Uuid,
        actors: Vec<String>,
        acknowledged_through: BTreeMap<String, i64>,
        testing_generation: Option<i64>,
    },
    Ping {
        ping_id: String,
    },
    MessageAccepted {
        idempotency_key: String,
        message: Box<Message>,
    },
    ReceiptRecorded {
        conversation_id: String,
        message_id: String,
        status: ReceiptStatus,
    },
    Message {
        delivery_id: Uuid,
        actor_id: String,
        delivery_sequence: i64,
        message: Box<Message>,
    },
    Receipt {
        delivery_id: Uuid,
        actor_id: String,
        delivery_sequence: i64,
        message_id: String,
        status: MessageStatus,
        snapshot: Option<Box<Message>>,
    },
    Error {
        code: String,
        message: String,
        recoverable: bool,
    },
    /// Explicit success/error response, preserving the command type and correlation fields.
    CommandResponse(Value),
}
#[derive(Serialize, Deserialize)]
#[serde(
    remote = "ServerFrame",
    tag = "type",
    content = "data",
    rename_all = "snake_case"
)]
enum ServerFrameWire {
    #[serde(rename = "connection.ready")]
    Ready {
        protocol_version: u16,
        connection_id: Uuid,
        #[serde(rename = "members", alias = "actors")]
        actors: Vec<String>,
        acknowledged_through: BTreeMap<String, i64>,
        #[serde(default)]
        testing_generation: Option<i64>,
    },
    Ping {
        ping_id: String,
    },
    #[serde(rename = "message.create.successful", alias = "message.create.success")]
    MessageAccepted {
        idempotency_key: String,
        #[serde(flatten)]
        message: Box<Message>,
    },
    #[serde(rename = "receipt.success")]
    ReceiptRecorded {
        conversation_id: String,
        message_id: String,
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
        message_id: String,
        status: MessageStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        snapshot: Option<Box<Message>>,
    },
    #[serde(rename = "connection.error")]
    Error {
        code: String,
        message: String,
        recoverable: bool,
    },
    #[serde(untagged)]
    CommandResponse(Value),
}
impl<'de> Deserialize<'de> for ServerFrame {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut value = Value::deserialize(deserializer)?;
        let kind = value["type"].as_str().unwrap_or("").to_owned();
        if matches!(
            kind.as_str(),
            "message.created"
                | "message.create"
                | "message.updated"
                | "message.deleted"
                | "message.read"
                | "message.delivered"
                | "message.failed"
        ) || (kind == "message.create.successful"
            && (value["data"]["metadata"]["delivery_id"].is_string()
                || value["metadata"]["delivery_id"].is_string()))
        {
            let metadata = value["data"]
                .get("metadata")
                .or_else(|| value.get("metadata"))
                .cloned()
                .unwrap_or(Value::Null);
            let snapshot = value["data"].clone();
            let data = value
                .get_mut("data")
                .and_then(Value::as_object_mut)
                .ok_or_else(|| serde::de::Error::custom("missing event data"))?;
            data.insert("delivery_id".into(), metadata["delivery_id"].clone());
            data.insert(
                "delivery_sequence".into(),
                metadata["delivery_sequence"].clone(),
            );
            data.insert(
                "actor_id".into(),
                data.get("recipient_id").cloned().unwrap_or(Value::Null),
            );
            if matches!(
                kind.as_str(),
                "message.read" | "message.delivered" | "message.failed"
            ) {
                data.insert("snapshot".into(), snapshot);
                data.insert(
                    "message_id".into(),
                    data.get("message-id").cloned().unwrap_or(Value::Null),
                );
                data.insert(
                    "status".into(),
                    Value::String(kind.trim_start_matches("message.").into()),
                );
                value["type"] = Value::String("receipt".into());
            } else {
                value["type"] = Value::String("new_message".into());
            }
            value.as_object_mut().unwrap().remove("metadata");
        }
        match kind.as_str() {
            "subscribe.success" | "ready" => {
                value["type"] = Value::String("connection.ready".into())
            }
            "message_accepted" => value["type"] = Value::String("message.create.successful".into()),
            "receipt_recorded" => value["type"] = Value::String("receipt.success".into()),
            "subscribe.error" | "subscription.closed" | "error" => {
                value["type"] = Value::String("connection.error".into());
                if value["data"]["message"].is_null() {
                    value["data"]["message"] = Value::String("subscription closed".into());
                }
            }
            "bundle.success"
            | "bundle.error"
            | "message.create.error"
            | "ack.success"
            | "ack.error"
            | "resume.success"
            | "resume.error"
            | "presence.success"
            | "presence.error"
            | "receipt.error" => return Ok(Self::CommandResponse(value)),
            _ => {}
        }
        ServerFrameWire::deserialize(value).map_err(serde::de::Error::custom)
    }
}

impl Serialize for ServerFrame {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let (delivery_id, actor_id, delivery_sequence, message, kind) = match self {
            Self::Message {
                delivery_id,
                actor_id,
                delivery_sequence,
                message,
            } => (
                delivery_id,
                actor_id,
                delivery_sequence,
                message,
                if message.deleted_at.is_some() {
                    "message.deleted"
                } else if message.updated_at.is_some() {
                    "message.updated"
                } else {
                    silicon_dm_protocol::message_creation_event(&message.sender.id, actor_id)
                },
            ),
            Self::Receipt {
                delivery_id,
                actor_id,
                delivery_sequence,
                snapshot: Some(message),
                status,
                ..
            } => (
                delivery_id,
                actor_id,
                delivery_sequence,
                message,
                match status {
                    MessageStatus::Read => "message.read",
                    MessageStatus::Failed => "message.failed",
                    _ => "message.delivered",
                },
            ),
            _ => return ServerFrameWire::serialize(self, serializer),
        };
        let mut data = serde_json::to_value(message).map_err(serde::ser::Error::custom)?;
        data["recipient_id"] = Value::String(actor_id.clone());
        data["metadata"] = serde_json::json!({"source":"dm","delivery_id":delivery_id,"delivery_sequence":delivery_sequence});
        serde_json::json!({"type":kind,"metadata":data["metadata"].clone(),"data":data})
            .serialize(serializer)
    }
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
    #[serde(default)]
    pub testing_environment_id: Option<Uuid>,
    #[serde(default)]
    pub testing_generation: Option<i64>,
    #[serde(default)]
    pub testing_environment: Option<Value>,
    pub app_id: String,
    pub iam_base_url: String,
    pub api_base_url: String,
}
