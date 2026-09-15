//! Translation between public group addresses and durable conversation keys.
use super::PostgresStore;
use crate::{AppError, AppResult, domain::OrganizationId};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

impl PostgresStore {
    /// Resolve an address in the authenticated organization. UUID aliases keep old links usable.
    pub(crate) async fn resolve_conversation_id(
        &self,
        org: &OrganizationId,
        value: &str,
    ) -> AppResult<Uuid> {
        if let Ok(id) = Uuid::parse_str(value) {
            return Ok(id);
        }
        if !silicon_dm_protocol::valid_group_id(value) {
            return Err(AppError::validation(
                "expected a conversation UUID or g:org:group-slug",
            ));
        }
        sqlx::query_scalar(
            "SELECT conversation_id FROM groups WHERE organization_id=$1 AND public_id=$2",
        )
        .bind(org.as_str())
        .bind(value)
        .fetch_optional(self.pool())
        .await?
        .ok_or(AppError::NotFound)
    }
    pub(crate) async fn public_group_id(&self, id: Uuid) -> AppResult<String> {
        sqlx::query_scalar("SELECT public_id FROM groups WHERE conversation_id=$1")
            .bind(id)
            .fetch_optional(self.pool())
            .await?
            .ok_or(AppError::NotFound)
    }
    /// Translate only contract-owned ID fields, never user metadata, text or attachments.
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
        let rows: Vec<(Uuid, String)> = sqlx::query_as(
            "SELECT conversation_id,public_id FROM groups WHERE conversation_id=ANY($1)",
        )
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
            if object.contains_key("participants")
                && object.contains_key("org_id")
                && let Some(id) = object.get_mut("id")
            {
                visitor(id);
            }
            if let Some(id) = object.get_mut("conversation_id") {
                visitor(id);
            }
            for key in [
                "items",
                "data",
                "last_message",
                "display_message",
                "original_messages",
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
        let mut value = serde_json::json!({"id":id,"org_id":"tos","participants":[],"last_message":{
            "id":id,"conversation_id":id,"metadata":{"conversation_id":id,"data":{"conversation_id":id}},
            "message":id,"attachments":[{"conversation_id":id}]}});
        visit_ids(&mut value, &mut |v| {
            *v = Value::String("g:tos:product-design".into());
        });
        assert_eq!(value["id"], "g:tos:product-design");
        assert_eq!(
            value["last_message"]["conversation_id"],
            "g:tos:product-design"
        );
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
