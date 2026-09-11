//! Append-only message edits and deletion tombstones.

use sqlx::FromRow;
use uuid::Uuid;

use super::{
    PostgresStore,
    idempotency::{IdempotencyClaim, IdempotencyResult, claim, complete, request_hash},
    map_constraint_error,
};
use crate::{
    AppError, AppResult,
    domain::{ActorRef, IdempotencyKey, Message, MessageCreate, OrganizationId},
};

#[derive(FromRow)]
struct Original {
    sender_kind: String,
    sender_id: String,
    sender_address: Option<String>,
    recipient_address: Option<String>,
}

impl PostgresStore {
    /// Returns a message after checking membership and its exact conversation.
    ///
    /// # Errors
    /// Returns not found for inaccessible or mismatched message identifiers.
    pub async fn get_message(
        &self,
        org: &OrganizationId,
        actor: &ActorRef,
        conversation: Uuid,
        id: Uuid,
    ) -> AppResult<Message> {
        self.require_participant(org, actor, conversation).await?;
        let message = self.load_message(id).await?;
        if message.conversation_id != conversation {
            return Err(AppError::NotFound);
        }
        Ok(message)
    }

    /// Appends a full content replacement, or a content-free deletion tombstone.
    ///
    /// The original sender is the only editor. Compare-and-swap and idempotency
    /// are checked while holding the message lock; delivery fan-out commits in
    /// that same transaction. Deleted messages cannot be edited or resurrected.
    ///
    /// # Errors
    /// Rejects non-authors, stale versions, changed retry bodies, and invalid replies.
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    pub async fn revise_message(
        &self,
        org: &OrganizationId,
        actor: &ActorRef,
        conversation: Uuid,
        id: Uuid,
        expected_version: i64,
        key: &IdempotencyKey,
        content: Option<MessageCreate>,
    ) -> AppResult<Message> {
        if expected_version < 1 {
            return Err(AppError::validation(
                "If-Match must be a positive message version",
            ));
        }
        if let Some(content) = &content {
            content.validate().map_err(AppError::validation)?;
            if content
                .sender_id
                .as_ref()
                .is_some_and(|sender| !sender.addresses(actor))
            {
                return Err(AppError::Forbidden);
            }
        }
        self.require_participant(org, actor, conversation).await?;
        let operation = if content.is_some() {
            "messages.edit"
        } else {
            "messages.delete"
        };
        let hash = request_hash(&(
            id,
            conversation,
            expected_version,
            content.as_ref().map(MessageCreate::idempotency_content),
        ))?;
        let mut tx = self.pool().begin().await?;
        let original = sqlx::query_as::<_, Original>(
            "SELECT sender_kind::text AS sender_kind, sender_id, sender_address, recipient_address FROM messages WHERE id=$1 AND conversation_id=$2 AND organization_id=$3 FOR UPDATE"
        ).bind(id).bind(conversation).bind(org.as_str()).fetch_optional(&mut *tx).await?.ok_or(AppError::NotFound)?;
        if original.sender_kind != actor.actor_type.as_str()
            || original.sender_id != actor.id.as_str()
        {
            return Err(AppError::Forbidden);
        }
        if let Some(content) = &content {
            let sender = content
                .sender_id
                .as_ref()
                .filter(|id| id.address_parts().is_ok_and(|(_, isi)| isi.is_some()));
            if sender.is_some_and(|id| Some(id.as_str()) != original.sender_address.as_deref())
                || content
                    .recipient_id
                    .as_ref()
                    .is_some_and(|id| Some(id.as_str()) != original.recipient_address.as_deref())
            {
                return Err(AppError::validation(
                    "message routing cannot change in an edit; send a new message",
                ));
            }
        }
        match claim(&mut tx, org, actor, operation, key, &hash).await? {
            IdempotencyClaim::Replay(_) => {
                tx.commit().await?;
                drop(content);
                return self.load_message(id).await;
            }
            IdempotencyClaim::Acquired => {}
        }
        let current: Option<(i64, bool)> = sqlx::query_as(
            "SELECT version, deleted_at IS NOT NULL FROM message_revisions WHERE message_id=$1 ORDER BY version DESC LIMIT 1"
        ).bind(id).fetch_optional(&mut *tx).await?;
        let (version, deleted) = current.unwrap_or((1, false));
        if deleted {
            return Err(AppError::conflict("message has been deleted"));
        }
        if version != expected_version {
            return Err(AppError::conflict(
                "message version changed; fetch the current message before editing",
            ));
        }
        if let Some(reply) = content
            .as_ref()
            .and_then(|content| content.reply_to_message_id)
        {
            if reply == id {
                return Err(AppError::validation("a message cannot reply to itself"));
            }
            let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM messages WHERE id=$1 AND conversation_id=$2 AND organization_id=$3)")
                .bind(reply).bind(conversation).bind(org.as_str()).fetch_one(&mut *tx).await?;
            if !exists {
                return Err(AppError::NotFound);
            }
        }
        let next = version
            .checked_add(1)
            .ok_or_else(|| AppError::conflict("message version exhausted"))?;
        sqlx::query("INSERT INTO message_revisions(id,message_id,version,content,deleted_at) VALUES($1,$2,$3,$4,CASE WHEN $4::jsonb IS NULL THEN clock_timestamp() ELSE NULL END)")
            .bind(Uuid::now_v7()).bind(id).bind(next).bind(content.as_ref().map(sqlx::types::Json))
            .execute(&mut *tx).await.map_err(map_constraint_error)?;
        // Include sender devices so every authenticated view converges.
        let targets: Vec<(String, String)> = sqlx::query_as(
            "SELECT actor_kind::text, actor_id FROM conversation_participants WHERE conversation_id=$1 AND organization_id=$2 ORDER BY actor_kind, actor_id"
        ).bind(conversation).bind(org.as_str()).fetch_all(&mut *tx).await?;
        for (kind, target) in targets {
            sqlx::query("INSERT INTO actor_deliveries(id,organization_id,target_kind,target_id,sequence,delivery_kind,conversation_id,message_id,delivery_revision) VALUES($1,$2,$3::text::actor_kind,$4,1,'message',$5,$6,$7)")
                .bind(Uuid::now_v7()).bind(org.as_str()).bind(kind).bind(target).bind(conversation).bind(id).bind(next)
                .execute(&mut *tx).await.map_err(map_constraint_error)?;
        }
        complete(
            &mut tx,
            org,
            actor,
            operation,
            key,
            IdempotencyResult {
                resource_type: "message",
                resource_id: id,
                response_status: 200,
            },
        )
        .await?;
        tx.commit().await.map_err(map_constraint_error)?;
        drop(content);
        self.load_message(id).await
    }
}
