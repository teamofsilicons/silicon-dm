//! Conversation persistence and pagination.

use std::collections::HashMap;

use serde::Serialize;
use sqlx::FromRow;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{
    AppError, AppResult,
    application::commands::CreateConversationCommand,
    domain::{
        ActorRef, ActorType, Conversation, ConversationPage, Cursor, MAX_CONVERSATION_PARTICIPANTS,
        OrganizationId, PageRequest,
    },
};

use super::{
    PostgresStore,
    directory::refresh_directory_in,
    idempotency::{IdempotencyClaim, IdempotencyResult, claim, complete, request_hash},
    map_constraint_error,
    rows::{parse_actor_id, parse_actor_type},
};

const CREATE_CONVERSATION_OPERATION: &str = "conversations.create";

#[derive(Debug, FromRow)]
struct ConversationRecord {
    id: Uuid,
    organization_id: String,
    last_message_id: Option<Uuid>,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
}

#[derive(Debug, FromRow)]
struct ParticipantRecord {
    conversation_id: Uuid,
    actor_kind: String,
    actor_id: String,
}

#[derive(Serialize)]
struct ConversationIdempotencyContent<'a> {
    participants: &'a [ActorRef],
}

impl PostgresStore {
    /// Initializes empty direct chats with IAM-disclosed active organization members.
    /// Existing chats retain their IDs and activity; no message or delivery is created.
    ///
    /// # Errors
    /// Fails closed on ambiguous IAM identities or database errors.
    pub async fn initialize_member_conversations(
        &self,
        organization: &OrganizationId,
        actor: &ActorRef,
    ) -> AppResult<()> {
        let mut tx = self.pool().begin().await?;
        // The same active projections authorize explicit conversation creation.
        // Lock them through insertion so a concurrent removal cannot create a new chat.
        let members: Vec<(ActorType, String)> = sqlx::query_as(
            "SELECT member.actor_kind, member.actor_id
             FROM iam_membership_projections member
             JOIN organization_snapshots org USING (organization_id)
             JOIN iam_membership_projections caller ON caller.organization_id=org.organization_id
             WHERE org.organization_id=$1 AND org.status='active'
               AND caller.actor_kind=$2::text::actor_kind AND caller.actor_id=$3 AND caller.status='active'
               AND member.status='active'
               AND (member.actor_kind<>caller.actor_kind OR member.actor_id<>caller.actor_id)
             ORDER BY member.membership_id
             FOR SHARE OF member, caller, org",
        )
        .bind(organization.as_str()).bind(actor.actor_type.as_str()).bind(actor.id.as_str())
        .fetch_all(&mut *tx).await?;
        let mut unique = std::collections::HashSet::new();
        let mut candidates = Vec::with_capacity(members.len());
        for (kind, id) in members {
            if !unique.insert(id.clone()) {
                return Err(AppError::Forbidden);
            }
            let member = ActorRef {
                actor_type: kind,
                id: parse_actor_id(&id)?,
            };
            let mut participants = vec![actor.clone(), member.clone()];
            canonicalize_participants(&mut participants);
            let hash = participant_set_hash(&participants);
            candidates.push(serde_json::json!({
                "id": Uuid::now_v7(), "hash": blake3::Hash::from(hash).to_hex().to_string(),
                "actor_kind": member.actor_type.as_str(), "actor_id": member.id.as_str(),
            }));
        }
        if !candidates.is_empty() {
            sqlx::query(
                "WITH candidates AS (
                   SELECT * FROM jsonb_to_recordset($4) AS c(id uuid, hash text, actor_kind text, actor_id text)
                 ), created AS (
                   INSERT INTO conversations (id,organization_id,participant_set_hash,created_by_kind,created_by_id)
                   SELECT id,$1,decode(hash,'hex'),$2::text::actor_kind,$3 FROM candidates ORDER BY hash
                   ON CONFLICT (organization_id,participant_set_hash) DO NOTHING RETURNING id
                 )
                 INSERT INTO conversation_participants (conversation_id,organization_id,actor_kind,actor_id)
                 SELECT id,$1,$2::text::actor_kind,$3 FROM created
                 UNION ALL
                 SELECT c.id,$1,c.actor_kind::actor_kind,c.actor_id FROM candidates c JOIN created USING(id)",
            )
            .bind(organization.as_str()).bind(actor.actor_type.as_str()).bind(actor.id.as_str())
            .bind(serde_json::Value::Array(candidates)).execute(&mut *tx).await?;
        }
        tx.commit().await.map_err(map_constraint_error)
    }

    /// Creates or resolves the exact participant set idempotently.
    ///
    /// # Errors
    ///
    /// Rejects fewer than two unique participants, a missing creator, key reuse
    /// with different content, or a durable database invariant failure.
    #[allow(
        clippy::too_many_lines,
        reason = "the exact-participant-set transaction and idempotency decision are reviewed as one atomic workflow"
    )]
    pub async fn create_conversation(
        &self,
        mut command: CreateConversationCommand,
    ) -> AppResult<Conversation> {
        canonicalize_participants(&mut command.participants);
        if command.participants.len() < 2 {
            return Err(AppError::validation(
                "a conversation requires at least two unique participants",
            ));
        }
        if command.participants.len() > MAX_CONVERSATION_PARTICIPANTS {
            return Err(AppError::validation(
                "a conversation may contain at most 100 unique participants",
            ));
        }
        if !command.participants.contains(&command.creator) {
            return Err(AppError::Forbidden);
        }
        let participant_hash = participant_set_hash(&command.participants);
        let idempotency_hash = request_hash(&ConversationIdempotencyContent {
            participants: &command.participants,
        })?;
        let mut transaction = self.pool().begin().await?;
        refresh_directory_in(
            &mut transaction,
            &command.organization_id,
            &command.participants,
        )
        .await?;
        let claim = claim(
            &mut transaction,
            &command.organization_id,
            &command.creator,
            CREATE_CONVERSATION_OPERATION,
            &command.idempotency_key,
            &idempotency_hash,
        )
        .await?;

        let conversation_id = match claim {
            IdempotencyClaim::Replay(resource_id) => resource_id,
            IdempotencyClaim::Acquired => {
                let candidate_id = Uuid::now_v7();
                let inserted = sqlx::query_scalar::<_, Uuid>(
                    r#"
                    INSERT INTO conversations (
                        id,
                        organization_id,
                        participant_set_hash,
                        created_by_kind,
                        created_by_id
                    )
                    VALUES ($1, $2, $3, $4::text::actor_kind, $5)
                    ON CONFLICT (organization_id, participant_set_hash) DO NOTHING
                    RETURNING id
                    "#,
                )
                .bind(candidate_id)
                .bind(command.organization_id.as_str())
                .bind(participant_hash.as_slice())
                .bind(command.creator.actor_type.as_str())
                .bind(command.creator.id.as_str())
                .fetch_optional(&mut *transaction)
                .await
                .map_err(map_constraint_error)?;

                let (conversation_id, was_created) = if let Some(id) = inserted {
                    (id, true)
                } else {
                    let id = sqlx::query_scalar::<_, Uuid>(
                        r#"
                            SELECT id
                            FROM conversations
                            WHERE organization_id = $1
                              AND participant_set_hash = $2
                            "#,
                    )
                    .bind(command.organization_id.as_str())
                    .bind(participant_hash.as_slice())
                    .fetch_one(&mut *transaction)
                    .await?;
                    (id, false)
                };

                if was_created {
                    for participant in &command.participants {
                        sqlx::query(
                            r#"
                            INSERT INTO conversation_participants (
                                conversation_id,
                                organization_id,
                                actor_kind,
                                actor_id
                            )
                            VALUES ($1, $2, $3::text::actor_kind, $4)
                            "#,
                        )
                        .bind(conversation_id)
                        .bind(command.organization_id.as_str())
                        .bind(participant.actor_type.as_str())
                        .bind(participant.id.as_str())
                        .execute(&mut *transaction)
                        .await
                        .map_err(map_constraint_error)?;
                    }
                }
                complete(
                    &mut transaction,
                    &command.organization_id,
                    &command.creator,
                    CREATE_CONVERSATION_OPERATION,
                    &command.idempotency_key,
                    IdempotencyResult {
                        resource_type: "conversation",
                        resource_id: conversation_id,
                        response_status: 201,
                    },
                )
                .await?;
                conversation_id
            }
        };
        transaction.commit().await.map_err(map_constraint_error)?;
        self.get_conversation(&command.organization_id, &command.creator, conversation_id)
            .await
    }

    /// Lists conversations visible to a participant in newest-activity order.
    ///
    /// # Errors
    ///
    /// Rejects an invalid cursor or page length and propagates database/data
    /// corruption errors.
    pub async fn list_conversations(
        &self,
        organization_id: &OrganizationId,
        actor: &ActorRef,
        page: &PageRequest,
    ) -> AppResult<ConversationPage> {
        let limit = page.validated_limit()?;
        let cursor = page
            .cursor
            .as_deref()
            .map(|value| Cursor::decode(value, "conversations"))
            .transpose()?;
        let (cursor_time, cursor_id) = cursor
            .as_ref()
            .and_then(Cursor::activity_position)
            .map_or((None, None), |(time, id)| (Some(time), Some(id)));
        if cursor
            .as_ref()
            .is_some_and(|value| value.activity_position().is_none())
        {
            return Err(AppError::validation(
                "cursor is invalid for conversation pagination",
            ));
        }
        let records = sqlx::query_as::<_, ConversationRecord>(
            r#"
            SELECT
                conversation.id,
                conversation.organization_id,
                (
                    SELECT message.id
                    FROM messages AS message
                    LEFT JOIN message_bundle_items AS bundle_item
                      ON bundle_item.message_id = message.id
                    WHERE message.conversation_id = conversation.id
                      AND (bundle_item.role IS NULL OR bundle_item.role = 'display')
                    ORDER BY message.sequence DESC
                    LIMIT 1
                ) AS last_message_id,
                conversation.created_at,
                conversation.updated_at
            FROM conversations AS conversation
            JOIN conversation_participants AS participant
              ON participant.conversation_id = conversation.id
             AND participant.organization_id = conversation.organization_id
            WHERE conversation.organization_id = $1
              AND participant.actor_kind = $2::text::actor_kind
              AND participant.actor_id = $3
              AND (
                    $4::timestamptz IS NULL
                    OR (conversation.updated_at, conversation.id) < ($4, $5)
              )
            ORDER BY conversation.updated_at DESC, conversation.id DESC
            LIMIT $6
            "#,
        )
        .bind(organization_id.as_str())
        .bind(actor.actor_type.as_str())
        .bind(actor.id.as_str())
        .bind(cursor_time)
        .bind(cursor_id)
        .bind(i64::from(limit) + 1)
        .fetch_all(self.pool())
        .await?;
        self.conversation_page(records, limit).await
    }

    /// Fetches one conversation after participant authorization in SQL.
    ///
    /// # Errors
    ///
    /// Returns not found for absent, cross-tenant, or non-participant access.
    pub async fn get_conversation(
        &self,
        organization_id: &OrganizationId,
        actor: &ActorRef,
        conversation_id: Uuid,
    ) -> AppResult<Conversation> {
        let record = sqlx::query_as::<_, ConversationRecord>(
            r#"
            SELECT
                conversation.id,
                conversation.organization_id,
                (
                    SELECT message.id
                    FROM messages AS message
                    LEFT JOIN message_bundle_items AS bundle_item
                      ON bundle_item.message_id = message.id
                    WHERE message.conversation_id = conversation.id
                      AND (bundle_item.role IS NULL OR bundle_item.role = 'display')
                    ORDER BY message.sequence DESC
                    LIMIT 1
                ) AS last_message_id,
                conversation.created_at,
                conversation.updated_at
            FROM conversations AS conversation
            JOIN conversation_participants AS participant
              ON participant.conversation_id = conversation.id
             AND participant.organization_id = conversation.organization_id
            WHERE conversation.id = $1
              AND conversation.organization_id = $2
              AND participant.actor_kind = $3::text::actor_kind
              AND participant.actor_id = $4
            "#,
        )
        .bind(conversation_id)
        .bind(organization_id.as_str())
        .bind(actor.actor_type.as_str())
        .bind(actor.id.as_str())
        .fetch_optional(self.pool())
        .await?
        .ok_or(AppError::NotFound)?;
        let page = self.conversation_page(vec![record], 1).await?;
        page.items.into_iter().next().ok_or(AppError::NotFound)
    }

    /// Checks participant membership without exposing whether a cross-tenant
    /// conversation exists.
    ///
    /// # Errors
    ///
    /// Returns not-found when the actor is not a participant and propagates
    /// database failures.
    pub async fn require_participant(
        &self,
        organization_id: &OrganizationId,
        actor: &ActorRef,
        conversation_id: Uuid,
    ) -> AppResult<()> {
        let allowed = sqlx::query_scalar::<_, bool>(
            r#"
            SELECT EXISTS (
                SELECT 1
                FROM conversation_participants
                WHERE conversation_id = $1
                  AND organization_id = $2
                  AND actor_kind = $3::text::actor_kind
                  AND actor_id = $4
            )
            "#,
        )
        .bind(conversation_id)
        .bind(organization_id.as_str())
        .bind(actor.actor_type.as_str())
        .bind(actor.id.as_str())
        .fetch_one(self.pool())
        .await?;
        if allowed {
            Ok(())
        } else {
            Err(AppError::NotFound)
        }
    }

    async fn conversation_page(
        &self,
        mut records: Vec<ConversationRecord>,
        limit: u16,
    ) -> AppResult<ConversationPage> {
        let total_records = records.len();
        let has_next = total_records > usize::from(limit);
        if has_next {
            records.truncate(usize::from(limit));
        }
        let conversation_ids = records.iter().map(|record| record.id).collect::<Vec<_>>();
        let participant_records = if conversation_ids.is_empty() {
            Vec::new()
        } else {
            sqlx::query_as::<_, ParticipantRecord>(
                r#"
                SELECT conversation_id, actor_kind::text AS actor_kind, actor_id
                FROM conversation_participants
                WHERE conversation_id = ANY($1)
                ORDER BY conversation_id, actor_kind, actor_id
                "#,
            )
            .bind(&conversation_ids)
            .fetch_all(self.pool())
            .await?
        };
        let mut participants: HashMap<Uuid, Vec<ActorRef>> = HashMap::new();
        for record in participant_records {
            let conversation_id = record.conversation_id;
            participants
                .entry(conversation_id)
                .or_default()
                .push(map_participant(&record)?);
        }
        let mut items = Vec::with_capacity(records.len());
        let mut payload_bytes: usize = 0;
        for record in &records {
            let last_message = match record.last_message_id {
                Some(id) => Some(self.load_message(id).await?),
                None => None,
            };
            let message_bytes = last_message
                .as_ref()
                .map(super::rows::message_payload_bytes)
                .transpose()?
                .unwrap_or(0);
            if !items.is_empty()
                && payload_bytes.saturating_add(message_bytes)
                    > super::rows::MESSAGE_PAGE_BYTE_BUDGET
            {
                break;
            }
            payload_bytes = payload_bytes.saturating_add(message_bytes);
            items.push(Conversation {
                id: record.id,
                org_id: record
                    .organization_id
                    .parse()
                    .map_err(|error| super::rows::data_error("organization ID", error))?,
                participants: participants.remove(&record.id).unwrap_or_default(),
                last_message,
                created_at: record.created_at,
                updated_at: record.updated_at,
            });
            if payload_bytes >= super::rows::MESSAGE_PAGE_BYTE_BUDGET {
                break;
            }
        }
        let has_next = items.len() < total_records;
        let next_cursor = if has_next {
            items
                .last()
                .map(|item| Cursor::activity("conversations", item.updated_at, item.id))
                .map(|cursor| cursor.encode())
                .transpose()?
        } else {
            None
        };
        Ok(ConversationPage { items, next_cursor })
    }
}

fn canonicalize_participants(participants: &mut Vec<ActorRef>) {
    participants.sort_by(|left, right| {
        (left.actor_type.as_str(), left.id.as_str())
            .cmp(&(right.actor_type.as_str(), right.id.as_str()))
    });
    participants.dedup();
}

fn participant_set_hash(participants: &[ActorRef]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    for participant in participants {
        let kind = participant.actor_type.as_str().as_bytes();
        let id = participant.id.as_str().as_bytes();
        let kind_length = u64::try_from(kind.len()).unwrap_or(u64::MAX);
        let id_length = u64::try_from(id.len()).unwrap_or(u64::MAX);
        hasher.update(&kind_length.to_be_bytes());
        hasher.update(kind);
        hasher.update(&id_length.to_be_bytes());
        hasher.update(id);
    }
    *hasher.finalize().as_bytes()
}

fn map_participant(record: &ParticipantRecord) -> AppResult<ActorRef> {
    Ok(ActorRef {
        actor_type: parse_actor_type(&record.actor_kind)?,
        id: parse_actor_id(&record.actor_id)?,
    })
}

#[cfg(test)]
mod tests {
    use crate::domain::{ActorRef, ActorType};

    use super::{canonicalize_participants, participant_set_hash};

    #[test]
    fn participant_hash_is_order_independent() -> Result<(), Box<dyn std::error::Error>> {
        let carbon = ActorRef {
            actor_type: ActorType::Carbon,
            id: "carbon-1".parse()?,
        };
        let silicon = ActorRef {
            actor_type: ActorType::Silicon,
            id: "silicon-1".parse()?,
        };
        let mut first = vec![carbon.clone(), silicon.clone()];
        let mut second = vec![silicon, carbon];
        canonicalize_participants(&mut first);
        canonicalize_participants(&mut second);
        assert_eq!(participant_set_hash(&first), participant_set_hash(&second));
        Ok(())
    }

    #[test]
    fn duplicate_participants_are_removed() -> Result<(), Box<dyn std::error::Error>> {
        let actor = ActorRef {
            actor_type: ActorType::Carbon,
            id: "carbon-1".parse()?,
        };
        let mut participants = vec![actor.clone(), actor];
        canonicalize_participants(&mut participants);
        assert_eq!(participants.len(), 1);
        Ok(())
    }
}
