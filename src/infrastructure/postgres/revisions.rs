//! Message content history and independent deletion state.

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

/// Who is revising a message.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Reviser {
    /// The message's sender (edits and tombstones).
    Sender,
    /// The custodian of the Silicon that sent it (tombstones only).
    CustodianOfSender,
}

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
        actor: &ActorRef,
        conversation: Uuid,
        id: Uuid,
    ) -> AppResult<Message> {
        self.require_reader(actor, conversation).await?;
        let message = self.load_message(id).await?;
        if message.conversation_id != conversation {
            return Err(AppError::NotFound);
        }
        Ok(message)
    }

    /// Saves the previous content on edit, or marks deletion without adding history.
    ///
    /// The original sender is the only editor; the sender, or the custodian of the Silicon
    /// that sent it, may tombstone it. History and idempotency are checked while holding the
    /// message lock; delivery fan-out commits in that same transaction. Deleted messages
    /// cannot be edited or resurrected.
    ///
    /// # Errors
    /// Rejects non-authors, deleted messages, changed retry bodies, and invalid replies.
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    pub async fn revise_message(
        &self,
        actor: &ActorRef,
        reviser: Reviser,
        conversation: Uuid,
        id: Uuid,
        key: &IdempotencyKey,
        content: Option<MessageCreate>,
    ) -> AppResult<Message> {
        if reviser == Reviser::CustodianOfSender && content.is_some() {
            return Err(AppError::forbidden(
                "custodian_read_only",
                "A custodian may tombstone its Silicon's messages but never edit them: DM never lets anyone write as a Silicon.",
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
        let org: OrganizationId = match reviser {
            Reviser::Sender => self.require_participant(actor, conversation).await?,
            Reviser::CustodianOfSender => {
                self.require_reader(actor, conversation).await?;
                self.partition_of(conversation).await?
            }
        };
        let org = &org;
        let operation = if content.is_some() {
            "messages.edit"
        } else {
            "messages.delete"
        };
        let hash = request_hash(&(
            id,
            conversation,
            content.as_ref().map(MessageCreate::idempotency_content),
        ))?;
        let mut tx = self.pool().begin().await?;
        let original = sqlx::query_as::<_, Original>(
            "SELECT sender_kind::text AS sender_kind, sender_id, sender_address, recipient_address FROM messages WHERE id=$1 AND conversation_id=$2 AND organization_id=$3 FOR UPDATE"
        ).bind(id).bind(conversation).bind(org.as_str()).fetch_optional(&mut *tx).await?.ok_or(AppError::NotFound)?;
        let allowed = match reviser {
            Reviser::Sender => {
                original.sender_kind == actor.actor_type.as_str()
                    && original.sender_id == actor.id.as_str()
            }
            Reviser::CustodianOfSender => {
                original.sender_kind == "silicon"
                    && sqlx::query_scalar::<_, bool>(
                        "SELECT EXISTS (SELECT 1 FROM accounts WHERE uuid = $1 AND custodian_uuid = $2)",
                    )
                    .bind(&original.sender_id)
                    .bind(actor.id.as_str())
                    .fetch_one(&mut *tx)
                    .await?
            }
        };
        if !allowed {
            return Err(AppError::forbidden(
                "not_message_sender",
                "Only the message's sender can change it (and the custodian of a Silicon can tombstone that Silicon's messages).",
            ));
        }
        // Transaction-local provenance for the Ting handoffs of this revision; the reviser
        // needs an anchor row in the conversation's partition (a custodian may have none).
        super::directory::refresh_directory_in(&mut tx, org, std::slice::from_ref(actor)).await?;
        sqlx::query("SELECT set_config('dm.ting_originator',$1,true)")
            .bind(
                serde_json::json!({
                    "organization_id": org.as_str(),
                    "actor_kind": actor.actor_type.as_str(),
                    "actor_id": actor.id.as_str(),
                })
                .to_string(),
            )
            .execute(&mut *tx)
            .await?;
        if let Some(content) = &content {
            let sender = content
                .sender_id
                .as_ref()
                .filter(|id| id.address_parts().is_ok_and(|(_, isi)| isi.is_some()));
            // Routing stored before the identity re-key names the IAM-era id; compare it
            // through its link to the account.
            let stored_sender = linked_route(&mut tx, original.sender_address.as_deref()).await?;
            let stored_recipient =
                linked_route(&mut tx, original.recipient_address.as_deref()).await?;
            if sender.is_some_and(|id| Some(id.as_str()) != stored_sender.as_deref())
                || content
                    .recipient_id
                    .as_ref()
                    .is_some_and(|id| Some(id.as_str()) != stored_recipient.as_deref())
            {
                return Err(AppError::validation(
                    "message routing cannot change in an edit; send a new message",
                ));
            }
        }
        let accounts = OrganizationId::accounts();
        super::directory::refresh_directory_in(&mut tx, &accounts, std::slice::from_ref(actor))
            .await?;
        match claim(&mut tx, &accounts, actor, operation, key, &hash).await? {
            IdempotencyClaim::Replay(_) => {
                tx.commit().await?;
                drop(content);
                return self.load_message(id).await;
            }
            IdempotencyClaim::Acquired => {}
        }
        let deleted: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM message_history WHERE message_id=$1 AND deleted_at IS NOT NULL)")
            .bind(id).fetch_one(&mut *tx).await?;
        if deleted {
            return Err(AppError::conflict("message has been deleted"));
        }
        if let Some(reply) = content
            .as_ref()
            .and_then(|content| content.reply_to_message_id)
        {
            if reply == id {
                return Err(AppError::validation("a message cannot reply to itself"));
            }
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM messages WHERE id=$1 AND conversation_id=$2)",
            )
            .bind(reply)
            .bind(conversation)
            .fetch_one(&mut *tx)
            .await?;
            if !exists {
                return Err(AppError::NotFound);
            }
        }
        // The parent lock serializes edits; each accepted edit preserves the content it replaces.
        if let Some(content) = &content {
            sqlx::query("INSERT INTO message_history(message_id,content,history,updated_at) SELECT id,$2,jsonb_build_array(jsonb_build_object('content',NULL,'created_at',created_at)),clock_timestamp() FROM messages WHERE id=$1 ON CONFLICT(message_id) DO UPDATE SET history=message_history.history || jsonb_build_array(jsonb_build_object('content',message_history.content,'created_at',COALESCE(message_history.updated_at,(SELECT created_at FROM messages WHERE id=$1)))),content=$2,updated_at=clock_timestamp()")
                .bind(id).bind(sqlx::types::Json(content)).execute(&mut *tx).await?;
        } else {
            sqlx::query("INSERT INTO message_history(message_id,deleted_at) VALUES($1,clock_timestamp()) ON CONFLICT(message_id) DO UPDATE SET deleted_at=clock_timestamp()")
                .bind(id).execute(&mut *tx).await?;
        }
        // Private outbox discriminator prevents delivery deduplication from hiding later edits/deletions.
        let next: i64 = sqlx::query_scalar("SELECT COALESCE(max(delivery_revision),0)+1 FROM actor_deliveries WHERE message_id=$1 AND delivery_kind='message'")
            .bind(id).fetch_one(&mut *tx).await?;
        sqlx::query("UPDATE conversations SET updated_at=clock_timestamp() WHERE id=$1")
            .bind(conversation)
            .execute(&mut *tx)
            .await?;
        // Include sender devices so every authenticated view converges.
        let targets: Vec<(String, String)> = sqlx::query_as(
            "SELECT actor_kind::text, actor_id FROM effective_conversation_participants WHERE conversation_id=$1 ORDER BY actor_kind, actor_id"
        ).bind(conversation).fetch_all(&mut *tx).await?;
        for (kind, target) in targets {
            sqlx::query("INSERT INTO actor_deliveries(id,organization_id,target_kind,target_id,sequence,delivery_kind,conversation_id,message_id,delivery_revision) VALUES($1,$2,$3::text::actor_kind,$4,1,'message',$5,$6,$7)")
                .bind(Uuid::now_v7()).bind(org.as_str()).bind(&kind).bind(&target).bind(conversation).bind(id).bind(next)
                .execute(&mut *tx).await.map_err(map_constraint_error)?;
            sqlx::query("SELECT pg_notify('dm_delivery', $1)")
                .bind(serde_json::json!({ "account": target }).to_string())
                .execute(&mut *tx)
                .await?;
        }
        complete(
            &mut tx,
            &accounts,
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

/// A stored routing address in storage form: `isi@<uuid>` (or `<uuid>`); an IAM-era base
/// (`isi@si:cos`) is mapped through `identity_links` when it was linked.
async fn linked_route(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    address: Option<&str>,
) -> AppResult<Option<String>> {
    let Some(address) = address else {
        return Ok(None);
    };
    let (isi, base) = match address.rsplit_once('@') {
        Some((isi, base)) => (Some(isi), base),
        None => (None, address),
    };
    if !base.contains(':') {
        return Ok(Some(address.to_owned()));
    }
    let linked: Option<String> =
        sqlx::query_scalar("SELECT accounts_uuid FROM identity_links WHERE iam_public_id = $1")
            .bind(base)
            .fetch_optional(&mut **tx)
            .await?;
    Ok(Some(match (linked, isi) {
        (Some(uuid), Some(isi)) => format!("{isi}@{uuid}"),
        (Some(uuid), None) => uuid,
        (None, _) => address.to_owned(),
    }))
}
