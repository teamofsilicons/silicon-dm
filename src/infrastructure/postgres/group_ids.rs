//! Translation between public group addresses and durable conversation keys.
use super::PostgresStore;
use crate::{AppError, AppResult, domain::OrganizationId};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

impl PostgresStore {
    /// Resolve a direct recipient using the same IAM checks as explicit chat creation.
    pub(crate) async fn resolve_destination(
        &self,
        auth: &crate::application::auth::AuthContext,
        value: &str,
        create: bool,
        identity: &dyn crate::application::ports::IdentityProvider,
    ) -> AppResult<Uuid> {
        if Uuid::parse_str(value).is_ok() || value.starts_with("g:") || value.contains("::") {
            return self
                .resolve_conversation_id(&auth.organization_id, value)
                .await;
        }
        let recipient: crate::domain::ActorId = value
            .parse()
            .map_err(|_| AppError::validation("invalid recipient"))?;
        let recipient = recipient.base_actor_id().map_err(AppError::validation)?;
        if recipient == auth.actor.id {
            return Err(AppError::validation("recipient must be another account"));
        }
        let actors = vec![auth.actor.id.clone(), recipient.clone()];
        let participants = identity.authorize_participants(auth, &actors).await?;
        let participants = crate::api::handlers::verify_resolved_actors(&actors, participants)?;
        if !participants.contains(&auth.actor) {
            return Err(AppError::Forbidden);
        }
        if !create {
            let mut ordered = participants;
            ordered.sort_by(|a, b| {
                (a.actor_type.as_str(), a.id.as_str()).cmp(&(b.actor_type.as_str(), b.id.as_str()))
            });
            let address = ordered
                .iter()
                .map(|a| a.id.as_str())
                .collect::<Vec<_>>()
                .join("::");
            return self
                .resolve_conversation_id(&auth.organization_id, &address)
                .await;
        }
        let key = format!(
            "recipient-chat:{}",
            blake3::hash(recipient.as_str().as_bytes()).to_hex()
        );
        let conversation = self
            .create_conversation(crate::application::commands::CreateConversationCommand {
                organization_id: auth.organization_id.clone(),
                creator: auth.actor.clone(),
                participants,
                idempotency_key: key
                    .parse()
                    .map_err(|_| AppError::validation("invalid idempotency key"))?,
            })
            .await?;
        Ok(conversation.id)
    }

    /// Resolve an address in the authenticated organization. UUID aliases keep old links usable.
    pub(crate) async fn resolve_conversation_id(
        &self,
        org: &OrganizationId,
        value: &str,
    ) -> AppResult<Uuid> {
        if let Ok(id) = Uuid::parse_str(value) {
            return Ok(id);
        }
        if !silicon_dm_protocol::valid_group_id(value) && !value.contains("::") {
            return Err(AppError::validation(
                "expected a direct conversation address, UUID or g:org:group-slug",
            ));
        }
        sqlx::query_scalar(
            "SELECT id FROM conversation_addresses WHERE organization_id=$1 AND public_id=$2",
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
