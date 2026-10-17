//! Public wire types of DM's HTTP contract 4, independent of the server implementation.
//!
//! Accounts are Silicon Accounts accounts: a Carbon (`c:handle`) or a Silicon (`si:handle`).
//! Responses name them as [`Actor`] `{type, id, uuid, display_name?, pfp_url?}` where `id` is
//! the account's current public id and `uuid` its permanent key. Store the uuid, show the id.
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

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

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ActorType {
    Carbon,
    Silicon,
}
impl ActorType {
    /// `"carbon"` or `"silicon"`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Carbon => "carbon",
            Self::Silicon => "silicon",
        }
    }
    /// The kind a public id names: `c:` is a Carbon, `si:` a Silicon (an ISI prefix such as
    /// `deliberate@si:cos` is allowed). Anything else (a uuid, a group, a conversation) is `None`.
    pub fn of_id(id: &str) -> Option<Self> {
        let base = id.rsplit_once('@').map_or(id, |(_, base)| base);
        if base.contains("::") {
            None
        } else if base.starts_with("c:") {
            Some(Self::Carbon)
        } else if base.starts_with("si:") {
            Some(Self::Silicon)
        } else {
            None
        }
    }
}

/// An account as DM names it in responses.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Actor {
    #[serde(rename = "type")]
    pub actor_type: ActorType,
    /// The current public id (`c:ada`, `si:scout`); the uuid for a deleted account; the stored
    /// id for an identity from before Silicon Accounts that was never linked to an account.
    pub id: String,
    /// The permanent Silicon Accounts 128-bit UUID (canonical lowercase `UUIDv4`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pfp_url: Option<String>,
    /// The account was deleted; its messages stay.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deleted: bool,
    /// An identity from before Silicon Accounts that was never linked to an account.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub legacy: bool,
}
impl Actor {
    pub fn new(actor_type: ActorType, id: impl Into<String>) -> Self {
        Self {
            actor_type,
            id: id.into(),
            uuid: None,
            display_name: None,
            pfp_url: None,
            deleted: false,
            legacy: false,
        }
    }
    pub fn with_uuid(mut self, uuid: impl Into<String>) -> Self {
        self.uuid = Some(uuid.into());
        self
    }
    /// Whether two references name the same account: by uuid when both carry one (uuids
    /// are compared exactly), else by kind and current id.
    pub fn same_account(&self, other: &Self) -> bool {
        match (&self.uuid, &other.uuid) {
            (Some(a), Some(b)) => a == b,
            _ => self.actor_type == other.actor_type && self.id == other.id,
        }
    }
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
impl Attachment {
    pub fn link(permanent_url: url::Url) -> Self {
        Self {
            permanent_url,
            name: None,
            content_type: None,
            size: None,
        }
    }
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
            Input::Link(permanent_url) => Self::link(permanent_url),
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
/// What a spoken message says: a script DM speaks in a voice (`message.create` `speech`).
///
/// Square-bracket cues in the script steer the delivery and are not read aloud
/// (`[warmly] It works!`, `[whispers] … [/whispers]`); the service sets the message's
/// `voice_transcript` to the words without cues ([`crate::speech::spoken_words`]). Check a
/// script with [`crate::speech::validate_script`] before sending it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SpeechInput {
    /// 1 to 5000 Unicode scalar values, cues included.
    pub script: String,
    /// A voice key from [`crate::Client::voices`] (`mark`, `brittney`…); `None` speaks in the
    /// sender's current voice. Either way the voice is pinned on the message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct MessageCreate {
    /// Optional routing address of your own, `isi@si:you`.
    pub sender_id: Option<String>,
    /// For `POST /messages`: the account, group or conversation to send to. Inside a
    /// conversation: an optional routing address such as `deliberate@si:cos`.
    pub recipient_id: Option<String>,
    /// The text; with `speech`, an optional caption shown with the spoken message.
    pub text: Option<String>,
    pub attachments: Vec<Attachment>,
    pub voice: Option<VoiceAttachment>,
    pub voice_transcript: Option<String>,
    pub gif: Option<Gif>,
    pub metadata: Metadata,
    pub reply_to_message_id: Option<String>,
    /// A spoken message: DM renders the script in a voice (DM 0.17 and later). On a message
    /// read back from DM this is the pinned script and voice, so re-sending the content of a
    /// spoken message keeps it spoken.
    pub speech: Option<SpeechInput>,
}
#[derive(Deserialize)]
#[serde(remote = "MessageCreate")]
struct MessageCreateWire {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_id: Option<String>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speech: Option<SpeechInput>,
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
        // Only spoken messages carry the field, so every other message keeps the exact shape
        // earlier services accept.
        if let Some(speech) = &self.speech {
            let mut spoken = serde_json::json!({"script": speech.script});
            if let Some(voice) = &speech.voice {
                spoken["voice"] = Value::String(voice.clone());
            }
            value["speech"] = spoken;
        }
        value.serialize(serializer)
    }
}

/// How a spoken message sounds, as DM renders it on every message (`speech`, or null).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Speech {
    /// The voice key pinned on the message (`mark`); a later voice change does not alter it.
    pub voice: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_name: Option<String>,
    /// The script, cues included.
    pub script: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// `pending` (DM is rendering it), `ready` or `failed` (playback still renders on demand).
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub duration_milliseconds: Option<u64>,
    /// 64 loudness values from 0 to 100, for drawing a waveform; `None` until rendered.
    #[serde(default)]
    pub peaks: Option<Vec<u8>>,
}

/// A voice note on a message as DM renders it (`voice`, or null).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct VoiceNote {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_milliseconds: Option<u64>,
}

/// What DM knows about one of a message's `attachments` (same order as the URLs).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AttachmentDetails {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
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
    /// The app that sent the message for its sender with a User verification proof (for
    /// example `interface`); `None` when the sender sent it itself.
    pub via_app: Option<String>,
    /// Name, type and size of each of `content.attachments`, in the same order, when the
    /// service knows them (DM 0.17 and later; empty from earlier services).
    pub attachment_details: Vec<AttachmentDetails>,
    /// The voice note (its URL is also in `content.attachments`).
    pub voice: Option<VoiceNote>,
    /// The GIF (its URL is also in `content.attachments`).
    pub gif: Option<Gif>,
    /// How a spoken message sounds; `None` for other messages and on tombstones. Its audio is
    /// [`crate::Client::message_speech`].
    pub speech: Option<Speech>,
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
    #[serde(default)]
    pub via_app: Option<String>,
    // Read from the rendered message in `Message::deserialize`: their shapes differ from the
    // write shapes the flattened content uses (a rendered voice note has `url`).
    #[serde(skip)]
    pub attachment_details: Vec<AttachmentDetails>,
    #[serde(skip)]
    pub voice: Option<VoiceNote>,
    #[serde(skip)]
    pub gif: Option<Gif>,
    #[serde(skip)]
    pub speech: Option<Speech>,
}
/// A rendered field read leniently: a shape this release does not know leaves it empty instead
/// of making the whole message unreadable.
fn rendered<T: serde::de::DeserializeOwned>(value: Option<Value>) -> Option<T> {
    value
        .filter(|value| !value.is_null())
        .and_then(|value| serde_json::from_value(value).ok())
}
impl<'de> Deserialize<'de> for Message {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut value = Value::deserialize(deserializer)?;
        let object = value
            .as_object_mut()
            .ok_or_else(|| serde::de::Error::custom("a message is a JSON object"))?;
        let (details, voice, gif, speech) = (
            object.remove("attachment_details"),
            object.remove("voice"),
            object.remove("gif"),
            object.remove("speech"),
        );
        let mut message = MessageWire::deserialize(value).map_err(serde::de::Error::custom)?;
        message.attachment_details = rendered(details).unwrap_or_default();
        message.voice = rendered(voice);
        message.gif = rendered(gif);
        message.speech = rendered::<Speech>(speech);
        if let Some(speech) = &message.speech {
            message.content.speech = Some(SpeechInput {
                script: speech.script.clone(),
                voice: Some(speech.voice.clone()),
            });
        }
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
        let mut value = serde_json::json!({"message-id":self.id,"conversation_id":self.conversation_id,
            "recipient_id":self.content.recipient_id,"sender":self.sender,
            "message":if deleted {None} else {Some(self.content.text.as_deref().unwrap_or(""))},
            "attachments":attachments,"voice_transcript":if deleted {None} else {self.content.voice_transcript.as_deref()},
            "reply":self.reply,"bundle":self.bundle,"history":self.history,"created_at":self.created_at,"updated_at":self.updated_at,
            "deleted_at":self.deleted_at,"delivered_at":self.delivered_at,"read_at":self.read_at
        });
        if let Some(app) = &self.via_app {
            value["via_app"] = Value::String(app.clone());
        }
        // The fields DM 0.17 adds; a tombstone hides them like the rest of its content.
        let shown = |field: Value| if deleted { Value::Null } else { field };
        value["attachment_details"] = if deleted {
            Value::Array(Vec::new())
        } else {
            serde_json::to_value(&self.attachment_details).unwrap_or_default()
        };
        value["voice"] = shown(serde_json::to_value(&self.voice).unwrap_or_default());
        value["gif"] = shown(serde_json::to_value(&self.gif).unwrap_or_default());
        value["speech"] = shown(serde_json::to_value(&self.speech).unwrap_or_default());
        value["metadata"] = Value::Object(self.content.metadata.clone());
        value.serialize(serializer)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BundleRef {
    pub id: String,
    pub role: String,
}
/// The editable settings of a group: its name and description.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GroupSettings {
    pub name: String,
    #[serde(default)]
    pub description: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GroupCreate {
    #[serde(flatten)]
    pub settings: GroupSettings,
    /// Accounts to invite (`c:`/`si:` ids or uuids). Invitations must be accepted, except a
    /// custodian's own Silicons, who join at once.
    #[serde(default)]
    pub member_ids: Vec<String>,
}
/// A group's settings and people. Conversation lists carry only name, description and
/// owners; a group read (`GET /groups/{id}`) adds the version, members and, for owners,
/// pending invitations.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GroupDetails {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Send it back as `If-Match` with the next settings change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<i64>,
    /// Co-equal owners; a group always has at least one.
    #[serde(default)]
    pub owners: Vec<Actor>,
    /// Accepted members, owners included.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub members: Option<Vec<Actor>>,
    /// Invitations not accepted yet (owners only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_invitations: Option<Vec<Actor>>,
}
/// A group invitation waiting for its invitee.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Invitation {
    pub conversation_id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub invitee: Actor,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invited_by: Option<Actor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invited_at: Option<String>,
    #[serde(default)]
    pub owners: Vec<Actor>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Invitations {
    pub items: Vec<Invitation>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Conversation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<GroupDetails>,
    /// `c:ada::si:scout` (current ids), a group id, or the UUID of a conversation kept only
    /// for its history.
    pub id: String,
    pub participants: Vec<Actor>,
    /// Custodians who can read it without taking part (it includes one of their Silicons).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub visible_to_custodians: Vec<Actor>,
    /// Set on a conversation kept only for its history: send to this one instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,
    /// Older conversations of the same accounts, kept for their history.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supersedes: Vec<String>,
    pub last_message: Option<Message>,
    #[serde(default)]
    pub last_message_status: Option<MessageStatus>,
    /// Messages from others that the listed account has not read (the caller, or the Silicon
    /// a custodian lists with `for`). `None` from services before DM 0.17.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unread_count: Option<u64>,
    /// The newest message the listed account has read (a message code).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_read_message_id: Option<String>,
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
    /// The owner's current id.
    #[serde(rename = "member_id", alias = "actor_id")]
    pub actor_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member_uuid: Option<String>,
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
    /// The account's current id.
    #[serde(rename = "member_id", alias = "actor_id")]
    pub actor_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member_uuid: Option<String>,
    pub availability: String,
    pub activity: Option<Activity>,
    pub last_seen_at: Option<String>,
}
/// Device presence is an HTTP lease, independent of how updates are delivered.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PresenceLease {
    pub presence: Presence,
    pub lease_expires_at: String,
    pub activity_expires_at: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SyncRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u16>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reset: bool,
    /// Seconds (0 to 30) the service holds an empty answer open, returning as soon as
    /// something new arrives.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait: Option<u16>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SyncEventKind {
    Message,
    MessageStatus,
}

/// Authoritative reference; fetch the current message under current permissions.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SyncEvent {
    pub event_id: uuid::Uuid,
    pub sequence: i64,
    #[serde(rename = "type")]
    pub kind: SyncEventKind,
    pub conversation_id: String,
    pub message_id: String,
}

/// `cursor` remains usable on the last page (keep it); `has_more` binds a fixed scan boundary.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SyncPage {
    pub events: Vec<SyncEvent>,
    pub cursor: String,
    pub has_more: bool,
    pub upper_sequence: i64,
}

/// How updates reach an account: the sync feed, or Ting when the service has it configured.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Delivery {
    /// `sync` while Ting is not configured for this DM service, else `ting`.
    #[serde(default)]
    pub transport: String,
    #[serde(default)]
    pub ting_enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ting_api_base_url: Option<String>,
    /// Present for a signed-in account: Ting knows it as a recipient.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enrolled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enrolment: Option<String>,
    #[serde(default)]
    pub sync_path: String,
    #[serde(default)]
    pub sync_long_poll_seconds: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipts_path: Option<String>,
    #[serde(default)]
    pub dm_websocket_supported: bool,
}

/// `GET /api/v1/me`: the account behind the credential.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Me {
    pub account: Actor,
    /// A Silicon's custodian.
    #[serde(default)]
    pub custodian: Option<Actor>,
    /// The Silicons a Carbon looks after.
    #[serde(default)]
    pub custodian_of: Vec<Actor>,
    #[serde(default)]
    pub scopes: Vec<String>,
    /// The app that issued the proof, when the request came with one.
    #[serde(default)]
    pub via_app: Option<String>,
    #[serde(default)]
    pub delivery: Delivery,
    /// Whether this service speaks and transcribes (DM 0.17 and later).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speech: Option<SpeechDiscovery>,
}

/// What a DM service says about speech (`speech` on `GET /api/v1/accounts` and `/me`).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SpeechDiscovery {
    /// False when the service has no speech provider configured: speech and transcription
    /// answer 503 `speech_not_configured`.
    #[serde(default)]
    pub enabled: bool,
    /// The text-to-speech model (`eleven_v4`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The transcription model (`nova-3`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stt_model: Option<String>,
    /// `/api/v1/voices`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voices_path: Option<String>,
}

/// One voice DM speaks with (`GET /api/v1/voices`).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Voice {
    /// The public identifier used by the API, the CLI and the website (`mark`).
    pub key: String,
    /// `Mark`.
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// The ElevenLabs voice id, listed for transparency; DM is always addressed by `key`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_id: Option<String>,
}

/// The voices of a DM service (`GET /api/v1/voices`, no sign-in needed).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VoiceCatalog {
    /// The text-to-speech model every voice speaks with (`eleven_v4`).
    #[serde(default)]
    pub model: String,
    /// False when the service has no speech provider configured (it lists its voices anyway).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    pub items: Vec<Voice>,
    /// How an account's voice is chosen when nobody chose one.
    #[serde(default)]
    pub default_rule: String,
}
impl VoiceCatalog {
    /// The voice with this key, ignoring case.
    pub fn get(&self, key: &str) -> Option<&Voice> {
        self.items
            .iter()
            .find(|voice| voice.key.eq_ignore_ascii_case(key.trim()))
    }
    /// Every key, in catalog order.
    pub fn keys(&self) -> Vec<&str> {
        self.items.iter().map(|voice| voice.key.as_str()).collect()
    }
}

/// An account's voice (`GET|PUT|DELETE /api/v1/voices/{account}`). Every account has one: a
/// stored choice, else a default derived from its uuid.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AccountVoice {
    pub account: Actor,
    /// `key`, `name` and `description`.
    pub voice: Voice,
    /// Whether the voice was chosen (true) or is the account's default (false).
    #[serde(default)]
    pub chosen: bool,
    /// Who chose it: the account itself or, for a Silicon, its custodian.
    #[serde(default)]
    pub chosen_by: Option<Actor>,
    #[serde(default)]
    pub updated_at: Option<String>,
    /// Whether the signed-in account may change it (itself, or the custodian of a Silicon).
    #[serde(default)]
    pub can_change: bool,
}

/// Speech audio from DM: a WAV file (24 kHz mono 16-bit PCM) and what DM said about it.
#[derive(Clone, Debug)]
pub struct SpeechAudio {
    pub wav: Vec<u8>,
    /// `audio/wav`.
    pub content_type: String,
    /// The voice key it was spoken in (`x-dm-voice`).
    pub voice: Option<String>,
    /// From `x-dm-duration-ms`, else the WAV header.
    pub duration_milliseconds: Option<u64>,
    /// `hit` when DM served it from its speech cache, `miss` when it rendered it now.
    pub cache: Option<String>,
    pub etag: Option<String>,
}

/// Speech to text (`POST /api/v1/speech/transcriptions`). An empty transcript means no speech
/// was recognised.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Transcription {
    pub transcript: String,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub duration_milliseconds: Option<u64>,
    #[serde(default)]
    pub confidence: Option<f64>,
}

/// The answer to marking messages up to a code read or delivered.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReceiptsUpdate {
    /// Messages whose receipt changed (0 when they already were).
    pub updated: u64,
    /// What is still unread in the conversation.
    #[serde(default)]
    pub unread_count: Option<u64>,
    #[serde(default)]
    pub last_read_message_id: Option<String>,
}

/// How to sign in to DM (`GET /api/v1/accounts`, no sign-in needed).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SignInDiscovery {
    pub app_id: String,
    /// The Silicon Accounts URL that issues DM's tokens.
    pub accounts_url: String,
    pub api_base_url: String,
    #[serde(default)]
    pub contract_version: u16,
    #[serde(default)]
    pub sign_in: Value,
    /// Scopes DM honours on User verification proofs from other apps.
    #[serde(default)]
    pub proof_scopes: Vec<String>,
    #[serde(default)]
    pub delivery: Delivery,
    /// Speech and transcription, on DM 0.17 and later (`None` before).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speech: Option<SpeechDiscovery>,
}

/// Accounts a Silicon allows to start conversations with it and invite it to groups,
/// besides its custodian and the custodian's other Silicons.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AllowList {
    pub silicon: Actor,
    pub items: Vec<AllowListEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}
/// One allowed account, who allowed it (the Silicon or its custodian) and when.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AllowListEntry {
    pub account: Actor,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added_by: Option<Actor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added_at: Option<String>,
}
