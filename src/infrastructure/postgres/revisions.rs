//! Message content history and independent deletion state.

use sqlx::FromRow;
use uuid::Uuid;

use super::{
    PostgresStore,
    idempotency::{IdempotencyClaim, IdempotencyResult, claim, complete, request_hash},
    map_constraint_error,
    speech::SpeechRenderJob,
};
use crate::{
    AppError, AppResult,
    domain::{ActorRef, IdempotencyKey, Message, MessageCreate, OrganizationId},
};

/// How the account revising a message reaches its conversation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Reviser {
    /// A participant: edits and tombstones its own messages, and tombstones those of a
    /// Silicon it looks after.
    Participant,
    /// The custodian of a Silicon in the conversation, not in it itself: tombstones that
    /// Silicon's messages, never edits.
    Custodian,
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
    /// that sent it (in the conversation or not), may tombstone it. `on_behalf_of` is the
    /// Silicon a custodian named (`?for=si:x`): then only that Silicon's messages go. History
    /// and idempotency are checked while holding the message lock; delivery fan-out commits
    /// in that same transaction. Deleted messages cannot be edited or resurrected. An edit
    /// that speaks queues its rendering (`speech`) in the same transaction.
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
        speech: Option<SpeechRenderJob>,
        on_behalf_of: Option<&ActorRef>,
    ) -> AppResult<Message> {
        if reviser == Reviser::Custodian && content.is_some() {
            return Err(AppError::forbidden(
                "custodian_read_only",
                "A custodian may tombstone its Silicon's messages but never edit them: DM never lets anyone write as a Silicon.",
            ));
        }
        if let Some(content) = &content {
            content.validate().map_err(crate::domain::invalid_content)?;
            if content
                .sender_id
                .as_ref()
                .is_some_and(|sender| !sender.addresses(actor))
            {
                return Err(AppError::Forbidden);
            }
        }
        let org: OrganizationId = match reviser {
            Reviser::Participant => self.require_participant(actor, conversation).await?,
            Reviser::Custodian => {
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
        // The directory first, in the order a send takes it (the conversation's partition,
        // then @accounts), and only then the message: a send replying to this message holds
        // the directory while its reply waits for the message, so locking the message first
        // deadlocked against it. The reviser also needs an anchor row in the conversation's
        // partition (a custodian may have none) for the provenance of its Ting handoffs.
        super::directory::refresh_directory_in(&mut tx, org, std::slice::from_ref(actor)).await?;
        let accounts = OrganizationId::accounts();
        super::directory::refresh_directory_in(&mut tx, &accounts, std::slice::from_ref(actor))
            .await?;
        let original = sqlx::query_as::<_, Original>(
            "SELECT sender_kind::text AS sender_kind, sender_id, sender_address, recipient_address FROM messages WHERE id=$1 AND conversation_id=$2 AND organization_id=$3 FOR UPDATE"
        ).bind(id).bind(conversation).bind(org.as_str()).fetch_optional(&mut *tx).await?.ok_or(AppError::NotFound)?;
        let sent_by = |account: &ActorRef| {
            original.sender_kind == account.actor_type.as_str()
                && original.sender_id == account.id.as_str()
        };
        let is_sender = reviser == Reviser::Participant && sent_by(actor);
        let looks_after_sender = original.sender_kind == "silicon"
            && sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS (SELECT 1 FROM accounts WHERE uuid = $1 AND custodian_uuid = $2)",
            )
            .bind(&original.sender_id)
            .bind(actor.id.as_str())
            .fetch_one(&mut *tx)
            .await?;
        let allowed = match (&content, on_behalf_of) {
            (Some(_), _) => is_sender,
            (None, Some(silicon)) => sent_by(silicon) && looks_after_sender,
            (None, None) => is_sender || looks_after_sender,
        };
        if !allowed {
            let refusal = Refusal {
                editing: content.is_some(),
                custodian_of_sender: looks_after_sender,
                on_behalf_of: match on_behalf_of {
                    Some(silicon) => Some(public_id(&mut tx, silicon.id.as_str()).await?),
                    None => None,
                },
            };
            return Err(refusal.error(&mut tx, &original).await?);
        }
        // Transaction-local provenance for the Ting handoffs of this revision.
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
            // The code stays `conflict` (clients branch on it); the words say which case.
            let (message, hint) = if content.is_some() {
                (
                    "This message was deleted, so it cannot be edited.",
                    "Send a new message instead.",
                )
            } else {
                (
                    "This message was already deleted: there is nothing left to delete.",
                    "Its tombstone stays in the conversation (GET …/messages/{message}); repeating the first delete with its Idempotency-Key answers that tombstone.",
                )
            };
            return Err(
                AppError::refused(http::StatusCode::CONFLICT, "conflict", message).with_hint(hint),
            );
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
        if let (Some(job), Some(_)) = (&speech, &content)
            && let Err(limit) =
                super::speech::queue_speech_render_in(&mut tx, id, actor, job).await?
        {
            return Err(super::speech::limit_error(
                limit,
                super::messages::limit_allowance(limit, job),
            ));
        }
        // Private outbox discriminator prevents delivery deduplication from hiding later
        // edits/deletions. Revision 1 is the send; the outbox may have let a message's older
        // deliveries go (retention), so a revision is never below 2 here.
        let next: i64 = sqlx::query_scalar("SELECT COALESCE(max(delivery_revision),1)+1 FROM actor_deliveries WHERE message_id=$1 AND delivery_kind='message'")
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

/// Why an account may not change a message, in words that name who may.
struct Refusal {
    /// An edit (else a tombstone).
    editing: bool,
    /// The account looks after the Silicon that sent the message.
    custodian_of_sender: bool,
    /// The Silicon a custodian named with `for`, as its current id.
    on_behalf_of: Option<String>,
}

impl Refusal {
    async fn error(
        self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        original: &Original,
    ) -> AppResult<AppError> {
        let sender = public_id(tx, &original.sender_id).await?;
        let custodian: Option<String> = if original.sender_kind == "silicon" {
            match sqlx::query_scalar::<_, Option<String>>(
                "SELECT custodian_uuid FROM accounts WHERE uuid = $1",
            )
            .bind(&original.sender_id)
            .fetch_optional(&mut **tx)
            .await?
            .flatten()
            {
                Some(uuid) => Some(public_id(tx, &uuid).await?),
                None => None,
            }
        } else {
            None
        };
        let (message, hint) = if self.editing {
            if self.custodian_of_sender {
                (
                    format!(
                        "{sender} sent this message, and only a message's sender edits it: as {sender}'s custodian you may delete it, never edit it, because DM never lets anyone write as a Silicon."
                    ),
                    format!(
                        "Delete it instead (dm messages delete CONVERSATION MESSAGE, or DELETE /api/v1/conversations/{{conversation}}/messages/{{message}}?for={sender}), or send a message of your own."
                    ),
                )
            } else {
                (
                    format!("{sender} sent this message, and only a message's sender edits it."),
                    "Send a new message instead; replying to this one quotes it.".to_owned(),
                )
            }
        } else if let Some(silicon) = &self.on_behalf_of {
            (
                format!(
                    "{sender} sent this message, not {silicon}: with for={silicon} you delete only {silicon}'s messages."
                ),
                "Drop for to delete a message you sent yourself.".to_owned(),
            )
        } else {
            let (message, ask) = match &custodian {
                Some(custodian) => (
                    format!(
                        "Only {sender}, who sent this message, or its custodian {custodian} can delete it."
                    ),
                    format!("{sender} or {custodian}"),
                ),
                None => (
                    format!("Only {sender}, who sent this message, can delete it."),
                    sender.clone(),
                ),
            };
            (
                message,
                format!(
                    "You can delete the messages you sent and those of the Silicons you look after; to have this one removed, ask {ask}."
                ),
            )
        };
        Ok(AppError::forbidden("not_message_sender", message)
            .with_hint(hint)
            .with_details(serde_json::json!({ "sender": sender, "sender_custodian": custodian })))
    }
}

/// An account's current public id (`c:ada`, `si:sol`), or its storage key when DM has none.
async fn public_id(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    storage_key: &str,
) -> AppResult<String> {
    let current: Option<Option<String>> =
        sqlx::query_scalar("SELECT current_id FROM accounts WHERE uuid = $1")
            .bind(storage_key)
            .fetch_optional(&mut **tx)
            .await?;
    Ok(current.flatten().unwrap_or_else(|| storage_key.to_owned()))
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
