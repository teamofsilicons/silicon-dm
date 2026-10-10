//! Public conversation addresses: group ids and direct addresses ↔ conversation UUIDs.
use super::PostgresStore;
use crate::{AppError, AppResult};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

impl PostgresStore {
    /// The conversation of a group id (`g:slug` or the older `g:org:slug`).
    pub(crate) async fn group_by_public_id(&self, public_id: &str) -> AppResult<Option<Uuid>> {
        Ok(
            sqlx::query_scalar("SELECT conversation_id FROM groups WHERE public_id = $1")
                .bind(public_id)
                .fetch_optional(self.pool())
                .await?,
        )
    }

    /// Whether a conversation is a group.
    pub(crate) async fn is_group(&self, id: Uuid) -> AppResult<bool> {
        Ok(
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM groups WHERE conversation_id = $1)")
                .bind(id)
                .fetch_one(self.pool())
                .await?,
        )
    }

    /// Exact match on a stored direct address: only needed for conversations with an
    /// IAM-era identity that was never linked to an account (its id stays as stored).
    pub(crate) async fn conversation_by_stored_address(
        &self,
        address: &str,
    ) -> AppResult<Option<Uuid>> {
        Ok(
            sqlx::query_scalar(
                "SELECT id FROM conversation_addresses WHERE public_id = $1 LIMIT 1",
            )
            .bind(address)
            .fetch_optional(self.pool())
            .await?,
        )
    }

    pub(crate) async fn public_group_id(&self, id: Uuid) -> AppResult<String> {
        sqlx::query_scalar("SELECT public_id FROM groups WHERE conversation_id=$1")
            .bind(id)
            .fetch_optional(self.pool())
            .await?
            .ok_or(AppError::NotFound)
    }

    /// Translate only contract-owned conversation id fields, never user metadata, text or
    /// attachments.
    pub(crate) async fn public_conversation_ids(&self, value: &mut Value) -> AppResult<()> {
        let mut ids = HashSet::new();
        visit_ids(value, &mut |field| {
            if let Some(id) = field.as_str().and_then(|s| Uuid::parse_str(s).ok()) {
                ids.insert(id);
            }
        });
        if ids.is_empty() {
            return Ok(());
        }
        let ids: Vec<_> = ids.into_iter().collect();
        let rows: Vec<(Uuid, String)> =
            sqlx::query_as("SELECT id,public_id FROM conversation_addresses WHERE id=ANY($1)")
                .bind(ids)
                .fetch_all(self.pool())
                .await?;
        let ids: HashMap<_, _> = rows.into_iter().collect();
        visit_ids(value, &mut |field| {
            if let Some(public) = field
                .as_str()
                .and_then(|s| Uuid::parse_str(s).ok())
                .and_then(|id| ids.get(&id))
            {
                *field = Value::String(public.clone());
            }
        });
        Ok(())
    }
}

fn visit_ids(value: &mut Value, visitor: &mut impl FnMut(&mut Value)) {
    match value {
        Value::Array(items) => {
            for item in items {
                visit_ids(item, visitor);
            }
        }
        Value::Object(object) => {
            if object.contains_key("participants") && object.contains_key("updated_at") {
                if let Some(id) = object.get_mut("id") {
                    visitor(id);
                }
                if let Some(id) = object.get_mut("superseded_by") {
                    visitor(id);
                }
                if let Some(Value::Array(ids)) = object.get_mut("supersedes") {
                    for id in ids {
                        visitor(id);
                    }
                }
            }
            if let Some(id) = object.get_mut("conversation_id") {
                visitor(id);
            }
            for key in [
                "items",
                "events",
                "data",
                "last_message",
                "display_message",
                "original_messages",
                "conversation",
            ] {
                if let Some(child) = object.get_mut(key) {
                    visit_ids(child, visitor);
                }
            }
        }
        _ => {}
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_contract_ids_change() {
        let id = Uuid::nil().to_string();
        let mut value = serde_json::json!({"id":id,"participants":[],"updated_at":"x","superseded_by":id,"last_message":{
            "id":id,"conversation_id":id,"metadata":{"conversation_id":id,"data":{"conversation_id":id}},
            "message":id,"attachments":[{"conversation_id":id}]}});
        visit_ids(&mut value, &mut |v| {
            *v = Value::String("g:product-design".into());
        });
        assert_eq!(value["id"], "g:product-design");
        assert_eq!(value["superseded_by"], "g:product-design");
        assert_eq!(value["last_message"]["conversation_id"], "g:product-design");
        assert_eq!(value["last_message"]["id"], id);
        assert_eq!(value["last_message"]["metadata"]["conversation_id"], id);
        assert_eq!(
            value["last_message"]["metadata"]["data"]["conversation_id"],
            id
        );
        assert_eq!(
            value["last_message"]["attachments"][0]["conversation_id"],
            id
        );
    }
}
