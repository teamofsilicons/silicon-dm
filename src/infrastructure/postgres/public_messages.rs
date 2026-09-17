//! Public message addresses and fixed message snapshots. Durable keys stay private.
use super::PostgresStore;
use crate::{AppError, AppResult, domain::OrganizationId};
use serde_json::{Value, json};
use uuid::Uuid;

impl PostgresStore {
    pub(crate) async fn resolve_bundle_id(
        &self,
        org: &OrganizationId,
        conversation: Uuid,
        value: &str,
    ) -> AppResult<Uuid> {
        let sequence = silicon_dm_protocol::bundle_sequence(value).ok_or_else(|| {
            AppError::validation("expected a conversation-local bundle code such as 001")
        })?;
        sqlx::query_scalar("SELECT id FROM message_bundles WHERE organization_id=$1 AND conversation_id=$2 AND sequence=$3")
            .bind(org.as_str()).bind(conversation).bind(sequence).fetch_optional(self.pool()).await?.ok_or(AppError::NotFound)
    }
    async fn public_bundle_reference(&self, value: &mut Value) -> AppResult<()> {
        if let Some(id) = value.as_str().and_then(|v| Uuid::parse_str(v).ok()) {
            let sequence: i64 =
                sqlx::query_scalar("SELECT sequence FROM message_bundles WHERE id=$1")
                    .bind(id)
                    .fetch_one(self.pool())
                    .await?;
            *value = json!(
                silicon_dm_protocol::bundle_code(sequence)
                    .ok_or_else(|| AppError::validation("bundle code overflow"))?
            );
        }
        Ok(())
    }
    pub(crate) async fn resolve_message_id(
        &self,
        org: &OrganizationId,
        conversation: Uuid,
        value: &str,
    ) -> AppResult<Uuid> {
        if let Ok(id) = Uuid::parse_str(value) {
            return Ok(id);
        }
        let sequence = silicon_dm_protocol::message_sequence(value).ok_or_else(|| {
            AppError::validation("expected a conversation-local message code such as 00a")
        })?;
        sqlx::query_scalar("SELECT id FROM messages WHERE organization_id=$1 AND conversation_id=$2 AND sequence=$3")
            .bind(org.as_str()).bind(conversation).bind(sequence).fetch_optional(self.pool()).await?.ok_or(AppError::NotFound)
    }

    /// Normalize reference fields only, never text, metadata or quoted client content.
    pub(crate) async fn resolve_message_input(
        &self,
        org: &OrganizationId,
        conversation: Uuid,
        value: &mut Value,
    ) -> AppResult<()> {
        if let Some(reply) = value.get("reply").filter(|v| !v.is_null()) {
            let id = reply
                .get("message-id")
                .and_then(Value::as_str)
                .ok_or_else(|| AppError::validation("reply requires message-id"))?
                .to_owned();
            value["reply_to_message_id"] =
                json!(self.resolve_message_id(org, conversation, &id).await?);
        } else if let Some(id) = value
            .get("reply_to_message_id")
            .and_then(Value::as_str)
            .map(str::to_owned)
        {
            value["reply_to_message_id"] =
                json!(self.resolve_message_id(org, conversation, &id).await?);
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
                json!(self.resolve_message_id(org, conversation, &id).await?);
        }
        if let Some(ids) = value.get_mut("message_ids").and_then(Value::as_array_mut) {
            for id in ids {
                let raw = id
                    .as_str()
                    .ok_or_else(|| AppError::validation("message_ids must contain strings"))?;
                *id = json!(self.resolve_message_id(org, conversation, raw).await?);
            }
        }
        Ok(())
    }

    /// Translate only known message containers. Recursion never enters caller metadata.
    pub(crate) fn public_messages<'a>(
        &'a self,
        value: &'a mut Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = AppResult<()>> + Send + 'a>> {
        Box::pin(async move {
            if value.get("sender").is_some()
                && value.get("sequence").is_some()
                && value.get("conversation_id").is_some()
            {
                self.public_message(value).await?;
                return Ok(());
            }
            match value {
                Value::Array(items) => {
                    for item in items {
                        self.public_messages(item).await?;
                    }
                }
                Value::Object(object) => {
                    if object.contains_key("original_message_ids")
                        && let Some(id) = object.get_mut("id")
                    {
                        self.public_bundle_reference(id).await?;
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
                    if let Some(conversation) = object
                        .get("conversation_id")
                        .and_then(Value::as_str)
                        .and_then(|id| Uuid::parse_str(id).ok())
                    {
                        if let Some(ids) = object
                            .get_mut("original_message_ids")
                            .and_then(Value::as_array_mut)
                        {
                            for id in ids {
                                self.public_message_reference(conversation, id).await?;
                            }
                        }
                        if let Some(id) = object.get_mut("reply_to_message_id") {
                            self.public_message_reference(conversation, id).await?;
                        }
                    }
                    for key in [
                        "items",
                        "data",
                        "last_message",
                        "display_message",
                        "original_messages",
                    ] {
                        if let Some(child) = object.get_mut(key) {
                            self.public_messages(child).await?;
                        }
                    }
                }
                _ => {}
            }
            Ok(())
        })
    }

    async fn public_message_reference(
        &self,
        conversation: Uuid,
        value: &mut Value,
    ) -> AppResult<()> {
        if let Some(id) = value.as_str().and_then(|id| Uuid::parse_str(id).ok()) {
            let sequence: Option<i64> = sqlx::query_scalar(
                "SELECT sequence FROM messages WHERE conversation_id=$1 AND id=$2",
            )
            .bind(conversation)
            .bind(id)
            .fetch_optional(self.pool())
            .await?;
            *value = json!(sequence.and_then(silicon_dm_protocol::message_code));
        }
        Ok(())
    }

    async fn public_message(&self, value: &mut Value) -> AppResult<()> {
        let sequence = value["sequence"].as_i64().ok_or(AppError::NotFound)?;
        let conversation = value["conversation_id"]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok())
            .ok_or(AppError::NotFound)?;
        let public: Option<String> =
            sqlx::query_scalar("SELECT public_id FROM conversation_addresses WHERE id=$1")
                .bind(conversation)
                .fetch_optional(self.pool())
                .await?;
        let public = public.unwrap_or_else(|| conversation.to_string());
        let recipient = if public.starts_with("g:") {
            json!(public)
        } else if !value["recipient_id"].is_null() {
            value["recipient_id"].clone()
        } else {
            let recipients: Vec<String> = sqlx::query_scalar("SELECT actor_id FROM conversation_participants WHERE conversation_id=$1 AND NOT (actor_kind::text=$2 AND actor_id=$3) ORDER BY actor_id")
                .bind(conversation).bind(value["sender"]["type"].as_str().unwrap_or("")).bind(value["sender"]["id"].as_str().unwrap_or("")).fetch_all(self.pool()).await?;
            if recipients.len() == 1 {
                json!(recipients[0])
            } else {
                json!(public)
            }
        };
        let mut reply = Value::Null;
        if let Some(reply_id) = value["reply_to_message_id"]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok())
        {
            let original = self.load_message(reply_id).await?;
            if original.conversation_id != conversation {
                return Err(AppError::NotFound);
            }
            let original = serde_json::to_value(original).map_err(AppError::internal)?;
            reply = json!({"message-id":silicon_dm_protocol::message_code(original["sequence"].as_i64().unwrap_or(0)), "sender":original["sender"], "content": if original["deleted_at"].is_null() { public_content(&original) } else { Value::Null }});
        }
        let mut history = Vec::new();
        if value["deleted_at"].is_null() {
            for entry in value["history"].as_array().into_iter().flatten() {
                let mut snapshot = public_content(&entry["content"]);
                snapshot["created_at"] = entry["created_at"].clone();
                if let Some(reply_id) = entry["content"]["reply_to_message_id"]
                    .as_str()
                    .and_then(|id| Uuid::parse_str(id).ok())
                {
                    let original = self.load_message(reply_id).await?;
                    let original = serde_json::to_value(original).map_err(AppError::internal)?;
                    snapshot["reply"] = json!({"message-id":silicon_dm_protocol::message_code(original["sequence"].as_i64().unwrap_or(0)),"sender":original["sender"],"content":if original["deleted_at"].is_null() {public_content(&original)} else {Value::Null}});
                } else {
                    snapshot["reply"] = Value::Null;
                }
                history.push(snapshot);
            }
        }
        if let Some(id) = value.get_mut("bundle").and_then(|b| b.get_mut("id")) {
            self.public_bundle_reference(id).await?;
        }
        let content = public_content(value);
        let mut output = json!({
            "message-id":silicon_dm_protocol::message_code(sequence), "conversation_id":public,
            "recipient_id":recipient, "sender":value["sender"], "message":content["message"],
            "attachments":content["attachments"], "voice_transcript":content["voice_transcript"],
            "reply":if value["deleted_at"].is_null() {reply} else {Value::Null}, "bundle":value["bundle"], "history":history, "created_at":value["created_at"],
            "updated_at":value["updated_at"],
            "deleted_at":value["deleted_at"], "delivered_at":value["delivered_at"], "read_at":value["read_at"]
        });
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
}

fn public_content(value: &Value) -> Value {
    if !value["deleted_at"].is_null() {
        return json!({"message":null,"attachments":[],"voice_transcript":null});
    }
    let mut urls = value["attachments"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v| {
            v.as_str()
                .map(str::to_owned)
                .or_else(|| v["permanent_url"].as_str().map(str::to_owned))
        })
        .collect::<Vec<_>>();
    for (key, field) in [("voice", "permanent_url"), ("gif", "url")] {
        if let Some(url) = value[key][field].as_str()
            && !urls.iter().any(|v| v == url)
        {
            urls.push(url.to_owned());
        }
    }
    json!({"message":value["message"].as_str().unwrap_or(""),"attachments":urls,"voice_transcript":value["voice_transcript"]})
}
