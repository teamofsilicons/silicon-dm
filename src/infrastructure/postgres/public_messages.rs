//! Public message addresses and fixed message snapshots. Durable keys stay private.
use std::collections::{HashMap, HashSet};

use super::{PostgresStore, RenderState};
use crate::{
    AppError, AppResult,
    domain::speech::{SpeechScope, cache_key},
};
use serde_json::{Value, json};
use uuid::Uuid;

impl PostgresStore {
    pub(crate) async fn resolve_bundle_id(
        &self,
        conversation: Uuid,
        value: &str,
    ) -> AppResult<Uuid> {
        let sequence = silicon_dm_protocol::bundle_sequence(value).ok_or_else(|| {
            AppError::validation("expected a conversation-local bundle code such as 001")
        })?;
        sqlx::query_scalar(
            "SELECT id FROM message_bundles WHERE conversation_id=$1 AND sequence=$2",
        )
        .bind(conversation)
        .bind(sequence)
        .fetch_optional(self.pool())
        .await?
        .ok_or_else(|| {
            AppError::not_found(format!("This conversation has no bundle {value}."))
                .with_hint("A bundle's code is in the answer to POST /api/v1/conversations/{conversation}/bundles (dm bundles create), and on every message folded into it.")
        })
    }
    pub(crate) async fn resolve_message_id(
        &self,
        conversation: Uuid,
        value: &str,
    ) -> AppResult<Uuid> {
        if let Ok(id) = Uuid::parse_str(value) {
            return Ok(id);
        }
        let sequence = silicon_dm_protocol::message_sequence(value).ok_or_else(|| {
            AppError::invalid(format!(
                "{} is not a message code: a message is named by its code in the conversation, such as 00a (or, for older messages, a UUID).",
                value.chars().take(64).collect::<String>()
            ))
            .with_hint("dm messages list CONVERSATION shows each message with its code.")
        })?;
        sqlx::query_scalar("SELECT id FROM messages WHERE conversation_id=$1 AND sequence=$2")
            .bind(conversation)
            .bind(sequence)
            .fetch_optional(self.pool())
            .await?
            .ok_or_else(|| {
                AppError::not_found(format!("This conversation has no message {value}."))
                    .with_hint("Codes count up from 000 in each conversation: dm messages list CONVERSATION (GET /api/v1/conversations/{conversation}/messages) shows them.")
            })
    }

    /// The sequence of a message named by its code (or legacy UUID) in a conversation.
    ///
    /// # Errors
    /// Validation for something that is not a message code; not found for a message the
    /// conversation does not have.
    pub(crate) async fn message_sequence(&self, conversation: Uuid, value: &str) -> AppResult<i64> {
        let id = self.resolve_message_id(conversation, value).await?;
        sqlx::query_scalar("SELECT sequence FROM messages WHERE conversation_id=$1 AND id=$2")
            .bind(conversation)
            .bind(id)
            .fetch_optional(self.pool())
            .await?
            .ok_or(AppError::NotFound)
    }

    /// Normalize reference fields only, never text, metadata or quoted client content.
    pub(crate) async fn resolve_message_input(
        &self,
        conversation: Uuid,
        value: &mut Value,
    ) -> AppResult<()> {
        if let Some(reply) = value.get("reply").filter(|v| !v.is_null()) {
            let id = reply
                .get("message-id")
                .and_then(Value::as_str)
                .ok_or_else(|| AppError::validation("reply requires message-id"))?
                .to_owned();
            value["reply_to_message_id"] = json!(self.resolve_message_id(conversation, &id).await?);
        } else if let Some(id) = value
            .get("reply_to_message_id")
            .and_then(Value::as_str)
            .map(str::to_owned)
        {
            value["reply_to_message_id"] = json!(self.resolve_message_id(conversation, &id).await?);
        }
        if let Some(display) = value.get_mut("display_message")
            && let Some(id) = display
                .get("reply")
                .and_then(|r| r.get("message-id"))
                .or_else(|| display.get("reply_to_message_id"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        {
            display["reply_to_message_id"] =
                json!(self.resolve_message_id(conversation, &id).await?);
        }
        if let Some(ids) = value.get_mut("message_ids").and_then(Value::as_array_mut) {
            for id in ids {
                let raw = id
                    .as_str()
                    .ok_or_else(|| AppError::validation("message_ids must contain strings"))?;
                *id = json!(self.resolve_message_id(conversation, raw).await?);
            }
        }
        Ok(())
    }

    /// Translate only known message containers. Recursion never enters caller metadata.
    ///
    /// Everything a response needs from the database (each conversation's address and,
    /// for direct conversations, its participants; reply targets; bundle and message codes;
    /// speech rendering states) is gathered in one walk and read in a few queries for the
    /// whole response. One by one, a page of 50 messages cost hundreds of round trips.
    pub(crate) async fn public_messages(&self, value: &mut Value) -> AppResult<()> {
        let mut needs = Needs::default();
        collect(value, &mut needs);
        let facts = self.facts(needs).await?;
        apply(value, &facts)
    }

    async fn facts(&self, needs: Needs) -> AppResult<Facts> {
        let mut facts = Facts::default();
        if !needs.conversations.is_empty() {
            let ids: Vec<Uuid> = needs.conversations.iter().copied().collect();
            facts.addresses = sqlx::query_as::<_, (Uuid, String)>(
                "SELECT id, public_id FROM conversation_addresses WHERE id = ANY($1)",
            )
            .bind(&ids)
            .fetch_all(self.pool())
            .await?
            .into_iter()
            .collect();
        }
        // A message without a routing address names its one recipient: only direct
        // conversations (and conversations of several) need their participants for that.
        let direct: Vec<Uuid> = needs
            .participants
            .iter()
            .filter(|id| {
                !facts
                    .addresses
                    .get(id)
                    .is_some_and(|public| public.starts_with("g:"))
            })
            .copied()
            .collect();
        if !direct.is_empty() {
            for (conversation, kind, id) in sqlx::query_as::<_, (Uuid, String, String)>(
                "SELECT conversation_id, actor_kind::text, actor_id FROM conversation_participants
                 WHERE conversation_id = ANY($1) ORDER BY conversation_id, actor_id",
            )
            .bind(&direct)
            .fetch_all(self.pool())
            .await?
            {
                facts
                    .participants
                    .entry(conversation)
                    .or_default()
                    .push((kind, id));
            }
        }
        if !needs.replies.is_empty() {
            let ids: Vec<Uuid> = needs.replies.iter().copied().collect();
            for message in self.load_messages(&ids).await? {
                let id = message.id;
                let conversation = message.conversation_id;
                let value = serde_json::to_value(message).map_err(AppError::internal)?;
                facts.replies.insert(id, (conversation, value));
            }
        }
        if !needs.bundles.is_empty() {
            let ids: Vec<Uuid> = needs.bundles.iter().copied().collect();
            facts.bundles = sqlx::query_as::<_, (Uuid, i64)>(
                "SELECT id, sequence FROM message_bundles WHERE id = ANY($1)",
            )
            .bind(&ids)
            .fetch_all(self.pool())
            .await?
            .into_iter()
            .collect();
        }
        if !needs.references.is_empty() {
            let ids: Vec<Uuid> = needs.references.iter().copied().collect();
            facts.sequences = sqlx::query_as::<_, (Uuid, Uuid, i64)>(
                "SELECT id, conversation_id, sequence FROM messages WHERE id = ANY($1)",
            )
            .bind(&ids)
            .fetch_all(self.pool())
            .await?
            .into_iter()
            .map(|(id, conversation, sequence)| (id, (conversation, sequence)))
            .collect();
        }
        if !needs.speech.is_empty() {
            let versions: Vec<(Uuid, [u8; 32])> = needs.speech.iter().copied().collect();
            facts.speech = self.speech_render_states(&versions).await?;
        }
        Ok(facts)
    }
}

/// What rendering a response needs from the database.
#[derive(Default)]
struct Needs {
    /// Conversations whose address the response names.
    conversations: HashSet<Uuid>,
    /// Conversations whose participants an unrouted message's recipient comes from.
    participants: HashSet<Uuid>,
    /// Messages replied to (quoted with their sender and words).
    replies: HashSet<Uuid>,
    /// Bundles named by id.
    bundles: HashSet<Uuid>,
    /// Messages named by id (`reply_to_message_id`, `original_message_ids`).
    references: HashSet<Uuid>,
    /// Spoken message versions: (message, cache key).
    speech: HashSet<(Uuid, [u8; 32])>,
}

/// What the database said about those needs.
#[derive(Default)]
struct Facts {
    addresses: HashMap<Uuid, String>,
    participants: HashMap<Uuid, Vec<(String, String)>>,
    replies: HashMap<Uuid, (Uuid, Value)>,
    bundles: HashMap<Uuid, i64>,
    sequences: HashMap<Uuid, (Uuid, i64)>,
    speech: HashMap<(Uuid, [u8; 32]), RenderState>,
}

fn is_message(value: &Value) -> bool {
    value.get("sender").is_some()
        && value.get("sequence").is_some()
        && value.get("conversation_id").is_some()
}

fn uuid_of(value: &Value) -> Option<Uuid> {
    value.as_str().and_then(|text| Uuid::parse_str(text).ok())
}

/// The containers a response may hold messages in (the same walk `apply` makes).
const CONTAINERS: [&str; 5] = [
    "items",
    "data",
    "last_message",
    "display_message",
    "original_messages",
];

fn collect(value: &Value, needs: &mut Needs) {
    if is_message(value) {
        collect_message(value, needs);
        return;
    }
    match value {
        Value::Array(items) => {
            for item in items {
                collect(item, needs);
            }
        }
        Value::Object(object) => {
            if object.contains_key("original_message_ids")
                && let Some(id) = object.get("id").and_then(uuid_of)
            {
                needs.bundles.insert(id);
            }
            if object.get("conversation_id").and_then(uuid_of).is_some() {
                for id in object
                    .get("original_message_ids")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(uuid_of)
                {
                    needs.references.insert(id);
                }
                if let Some(id) = object.get("reply_to_message_id").and_then(uuid_of) {
                    needs.references.insert(id);
                }
            }
            for key in CONTAINERS {
                if let Some(child) = object.get(key) {
                    collect(child, needs);
                }
            }
        }
        _ => {}
    }
}

fn collect_message(value: &Value, needs: &mut Needs) {
    let Some(conversation) = value["conversation_id"]
        .as_str()
        .and_then(|s| Uuid::parse_str(s).ok())
    else {
        return;
    };
    needs.conversations.insert(conversation);
    if value["recipient_id"].is_null() {
        needs.participants.insert(conversation);
    }
    if let Some(reply) = uuid_of(&value["reply_to_message_id"]) {
        needs.replies.insert(reply);
    }
    let message_id = uuid_of(&value["id"]);
    if value["deleted_at"].is_null() {
        for entry in value["history"].as_array().into_iter().flatten() {
            if let Some(version) =
                speech_version(message_id, conversation, &entry["content"]["speech"])
            {
                needs.speech.insert(version);
            }
            if let Some(reply) = uuid_of(&entry["content"]["reply_to_message_id"]) {
                needs.replies.insert(reply);
            }
        }
        if let Some(version) = speech_version(message_id, conversation, &value["speech"]) {
            needs.speech.insert(version);
        }
    }
    if let Some(bundle) = value
        .get("bundle")
        .and_then(|bundle| bundle.get("id"))
        .and_then(uuid_of)
    {
        needs.bundles.insert(bundle);
    }
}

/// The rendering a stored speech (`{script, pinned: {model, voice_id}}`) is kept under.
fn speech_version(
    message_id: Option<Uuid>,
    conversation: Uuid,
    stored: &Value,
) -> Option<(Uuid, [u8; 32])> {
    let pinned = &stored["pinned"];
    match (
        message_id,
        stored["script"].as_str(),
        pinned["model"].as_str(),
        pinned["voice_id"].as_str(),
    ) {
        (Some(message_id), Some(script), Some(model), Some(voice_id)) => Some((
            message_id,
            cache_key(
                SpeechScope::Conversation(conversation),
                model,
                voice_id,
                script,
            ),
        )),
        _ => None,
    }
}

fn apply(value: &mut Value, facts: &Facts) -> AppResult<()> {
    if is_message(value) {
        return public_message(value, facts);
    }
    match value {
        Value::Array(items) => {
            for item in items {
                apply(item, facts)?;
            }
        }
        Value::Object(object) => {
            if object.contains_key("original_message_ids")
                && let Some(id) = object.get_mut("id")
            {
                bundle_reference(id, facts)?;
            }
            if object.contains_key("participants") && object.contains_key("last_message") {
                let last = &object["last_message"];
                let status = if last.is_null() {
                    Value::Null
                } else if !last["read_at"].is_null() {
                    json!("read")
                } else if !last["delivered_at"].is_null() {
                    json!("delivered")
                } else {
                    json!("sent")
                };
                object.insert("last_message_status".into(), status);
            }
            if let Some(conversation) = object.get("conversation_id").and_then(uuid_of) {
                if let Some(ids) = object
                    .get_mut("original_message_ids")
                    .and_then(Value::as_array_mut)
                {
                    for id in ids {
                        message_reference(conversation, id, facts);
                    }
                }
                if let Some(id) = object.get_mut("reply_to_message_id") {
                    message_reference(conversation, id, facts);
                }
            }
            for key in CONTAINERS {
                if let Some(child) = object.get_mut(key) {
                    apply(child, facts)?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn bundle_reference(value: &mut Value, facts: &Facts) -> AppResult<()> {
    if let Some(id) = uuid_of(value) {
        let sequence =
            facts.bundles.get(&id).copied().ok_or_else(|| {
                AppError::internal(anyhow::anyhow!("a rendered bundle has no row"))
            })?;
        *value = json!(
            silicon_dm_protocol::bundle_code(sequence)
                .ok_or_else(|| AppError::validation("bundle code overflow"))?
        );
    }
    Ok(())
}

fn message_reference(conversation: Uuid, value: &mut Value, facts: &Facts) {
    if let Some(id) = uuid_of(value) {
        let sequence = facts
            .sequences
            .get(&id)
            .filter(|(owner, _)| *owner == conversation)
            .map(|(_, sequence)| *sequence);
        *value = json!(sequence.and_then(silicon_dm_protocol::message_code));
    }
}

/// A quoted message: its code, sender and (unless deleted) words and links.
fn quote(original: &Value) -> Value {
    json!({"message-id":silicon_dm_protocol::message_code(original["sequence"].as_i64().unwrap_or(0)), "sender":original["sender"], "content": if original["deleted_at"].is_null() { reply_content(original) } else { Value::Null }})
}

fn public_message(value: &mut Value, facts: &Facts) -> AppResult<()> {
    let sequence = value["sequence"].as_i64().ok_or(AppError::NotFound)?;
    let conversation = value["conversation_id"]
        .as_str()
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or(AppError::NotFound)?;
    let public = facts
        .addresses
        .get(&conversation)
        .cloned()
        .unwrap_or_else(|| conversation.to_string());
    // The routing address the sender gave (`release-notes@si:sol`), whatever kind of
    // conversation this is. `recipient_id` keeps its older meaning, which names the group
    // in a group, so a routed group message is only recognisable by `routed_to`.
    let routed_to = value["recipient_id"].clone();
    let recipient = if public.starts_with("g:") {
        json!(public)
    } else if !value["recipient_id"].is_null() {
        value["recipient_id"].clone()
    } else {
        let sender_kind = value["sender"]["type"].as_str().unwrap_or("");
        let sender_id = value["sender"]["id"].as_str().unwrap_or("");
        let recipients: Vec<&str> = facts
            .participants
            .get(&conversation)
            .into_iter()
            .flatten()
            .filter(|(kind, id)| !(kind == sender_kind && id == sender_id))
            .map(|(_, id)| id.as_str())
            .collect();
        if recipients.len() == 1 {
            json!(recipients[0])
        } else {
            json!(public)
        }
    };
    let mut reply = Value::Null;
    if let Some(reply_id) = uuid_of(&value["reply_to_message_id"]) {
        let (owner, original) = facts.replies.get(&reply_id).ok_or(AppError::NotFound)?;
        if *owner != conversation {
            return Err(AppError::NotFound);
        }
        reply = quote(original);
    }
    let message_id = uuid_of(&value["id"]);
    let mut history = Vec::new();
    if value["deleted_at"].is_null() {
        for entry in value["history"].as_array().into_iter().flatten() {
            let mut snapshot = public_content(&entry["content"]);
            speech_status(
                message_id,
                conversation,
                &entry["content"]["speech"],
                &mut snapshot["speech"],
                facts,
            );
            snapshot["created_at"] = entry["created_at"].clone();
            if let Some(reply_id) = uuid_of(&entry["content"]["reply_to_message_id"]) {
                let (_, original) = facts.replies.get(&reply_id).ok_or(AppError::NotFound)?;
                snapshot["reply"] = quote(original);
            } else {
                snapshot["reply"] = Value::Null;
            }
            history.push(snapshot);
        }
    }
    if let Some(id) = value.get_mut("bundle").and_then(|b| b.get_mut("id")) {
        bundle_reference(id, facts)?;
    }
    let mut content = public_content(value);
    if value["deleted_at"].is_null() {
        speech_status(
            message_id,
            conversation,
            &value["speech"],
            &mut content["speech"],
            facts,
        );
    }
    let mut output = json!({
        "message-id":silicon_dm_protocol::message_code(sequence), "conversation_id":public,
        "recipient_id":recipient, "routed_to":routed_to, "sender":value["sender"], "message":content["message"],
        "attachments":content["attachments"], "voice_transcript":content["voice_transcript"],
        "attachment_details":content["attachment_details"], "voice":content["voice"],
        "gif":content["gif"], "speech":content["speech"], "metadata":content["metadata"],
        "reply":if value["deleted_at"].is_null() {reply} else {Value::Null}, "bundle":value["bundle"], "history":history, "created_at":value["created_at"],
        "updated_at":value["updated_at"],
        "deleted_at":value["deleted_at"], "delivered_at":value["delivered_at"], "read_at":value["read_at"]
    });
    // Sent for its sender by another app with a User verification proof.
    if let Some(app) = value.get("via_app").filter(|app| app.is_string()) {
        output["via_app"] = app.clone();
    }
    // Transport fields are moved into the envelope by the realtime adapter.
    for key in [
        "delivery_id",
        "delivery_sequence",
        "actor_id",
        "idempotency_key",
    ] {
        if let Some(v) = value.get(key) {
            output[key] = v.clone();
        }
    }
    *value = output;
    Ok(())
}

/// Fills in where a speech's rendering stands (`status`, `duration_milliseconds`, `peaks`)
/// from its render job or the cache.
fn speech_status(
    message_id: Option<Uuid>,
    conversation: Uuid,
    stored: &Value,
    rendered: &mut Value,
    facts: &Facts,
) {
    if !rendered.is_object() {
        return;
    }
    let state = speech_version(message_id, conversation, stored)
        .and_then(|version| facts.speech.get(&version));
    let Some(state) = state else {
        rendered["status"] = json!("failed");
        return;
    };
    rendered["status"] = json!(state.status.as_str());
    rendered["duration_milliseconds"] = json!(state.duration_milliseconds);
    rendered["peaks"] = json!(state.peaks);
}

/// What a reply quotes: the original's words and links.
fn reply_content(value: &Value) -> Value {
    let content = public_content(value);
    json!({"message":content["message"],"attachments":content["attachments"],"voice_transcript":content["voice_transcript"]})
}

/// The public content of a message or of one of its history snapshots. `attachments` stays a
/// list of URLs (attachments, then the voice note and the GIF, each once) for older clients;
/// `attachment_details` describes the same URLs in the same order, `voice`, `gif` and
/// `speech` the rest. Speech rendering facts are filled in by `speech_status`.
fn public_content(value: &Value) -> Value {
    if !value["deleted_at"].is_null() {
        return json!({"message":null,"attachments":[],"attachment_details":[],
            "voice_transcript":null,"voice":null,"gif":null,"speech":null,"metadata":{}});
    }
    let mut urls: Vec<String> = Vec::new();
    let mut details: Vec<Value> = Vec::new();
    for item in value["attachments"].as_array().into_iter().flatten() {
        let Some(url) = item.as_str().or_else(|| item["permanent_url"].as_str()) else {
            continue;
        };
        let mut detail = json!({"kind":"attachment","url":url});
        for field in ["name", "content_type", "size"] {
            if !item[field].is_null() {
                detail[field] = item[field].clone();
            }
        }
        urls.push(url.to_owned());
        details.push(detail);
    }
    let voice = value["voice"]["permanent_url"].as_str().map(|url| {
        // `permanent_url` repeats `url` for clients older than 0.17, which read voice
        // objects with that field.
        let mut voice = json!({"url":url,"permanent_url":url});
        for field in ["name", "content_type", "size", "duration_milliseconds"] {
            if !value["voice"][field].is_null() {
                voice[field] = value["voice"][field].clone();
            }
        }
        if !urls.iter().any(|known| known == url) {
            let mut detail = json!({"kind":"voice","url":url});
            for field in ["name", "content_type", "size"] {
                if !value["voice"][field].is_null() {
                    detail[field] = value["voice"][field].clone();
                }
            }
            urls.push(url.to_owned());
            details.push(detail);
        }
        voice
    });
    let gif = value["gif"]["url"].as_str().map(|url| {
        let mut gif = json!({"provider_id":value["gif"]["provider_id"],"url":url});
        for field in ["preview_url", "title"] {
            if !value["gif"][field].is_null() {
                gif[field] = value["gif"][field].clone();
            }
        }
        if !urls.iter().any(|known| known == url) {
            let mut detail = json!({"kind":"gif","url":url});
            if let Some(title) = value["gif"]["title"].as_str() {
                detail["name"] = json!(title);
            }
            urls.push(url.to_owned());
            details.push(detail);
        }
        gif
    });
    let speech = value["speech"]["script"].as_str().map(|script| {
        let pinned = &value["speech"]["pinned"];
        json!({
            "voice": if pinned["key"].is_string() { pinned["key"].clone() } else { value["speech"]["voice"].clone() },
            "voice_name": pinned["name"],
            "script": script,
            "model": pinned["model"],
            "status": "pending",
            "duration_milliseconds": null,
            "peaks": null,
        })
    });
    let metadata = if value["metadata"].is_object() {
        value["metadata"].clone()
    } else {
        json!({})
    };
    json!({"message":value["message"].as_str().unwrap_or(""),"attachments":urls,
        "attachment_details":details,"voice_transcript":value["voice_transcript"],
        "voice":voice,"gif":gif,"speech":speech,"metadata":metadata})
}
