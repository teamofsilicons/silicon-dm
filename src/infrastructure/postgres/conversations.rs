//! Conversations: one canonical direct conversation per exact set of accounts, groups,
//! listing, and who may use a conversation (participant, custodian, invitee).

use std::collections::HashMap;

use serde::Serialize;
use sha2::{Digest as _, Sha256};
use sqlx::FromRow;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{
    AppError, AppResult,
    application::commands::CreateConversationCommand,
    domain::{
        ActorId, ActorRef, ActorType, Conversation, ConversationAccess, ConversationPage, Cursor,
        MAX_CONVERSATION_PARTICIPANTS, OrganizationId, PageRequest,
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
    last_message_id: Option<Uuid>,
    superseded_by: Option<Uuid>,
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

const CONVERSATION_COLUMNS: &str = r"
    conversation.id,
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
    conversation.superseded_by,
    conversation.created_at,
    conversation.updated_at";

impl PostgresStore {
    /// Creates or resolves the canonical direct conversation of an exact participant set,
    /// whatever storage partition it lives in. Retries with the same key replay.
    ///
    /// # Errors
    /// Rejects fewer than two or more than 100 participants, a creator who is not one of
    /// them, key reuse with different content, or a durable invariant failure.
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
        let idempotency_hash = request_hash(&ConversationIdempotencyContent {
            participants: &command.participants,
        })?;
        let mut transaction = self.pool().begin().await?;
        let partition = OrganizationId::accounts();
        refresh_directory_in(&mut transaction, &partition, &command.participants).await?;
        let claim = claim(
            &mut transaction,
            &partition,
            &command.creator,
            CREATE_CONVERSATION_OPERATION,
            &command.idempotency_key,
            &idempotency_hash,
        )
        .await?;
        let conversation_id = match claim {
            IdempotencyClaim::Replay(resource_id) => resource_id,
            IdempotencyClaim::Acquired => {
                let id = canonical_or_insert_in(
                    &mut transaction,
                    &command.creator,
                    &command.participants,
                )
                .await?;
                complete(
                    &mut transaction,
                    &partition,
                    &command.creator,
                    CREATE_CONVERSATION_OPERATION,
                    &command.idempotency_key,
                    IdempotencyResult {
                        resource_type: "conversation",
                        resource_id: id,
                        response_status: 201,
                    },
                )
                .await?;
                id
            }
        };
        transaction.commit().await.map_err(map_constraint_error)?;
        self.get_conversation(&command.creator, conversation_id)
            .await
    }

    /// Idempotently creates the canonical direct conversation of two accounts (DM does this
    /// by itself only for a Silicon and its custodian).
    ///
    /// # Errors
    /// Durable invariant or database failures.
    pub async fn ensure_direct_conversation(&self, a: &ActorRef, b: &ActorRef) -> AppResult<Uuid> {
        let mut participants = vec![a.clone(), b.clone()];
        canonicalize_participants(&mut participants);
        if participants.len() != 2 {
            return Err(AppError::validation(
                "a conversation needs two different accounts",
            ));
        }
        let mut transaction = self.pool().begin().await?;
        refresh_directory_in(&mut transaction, &OrganizationId::accounts(), &participants).await?;
        let id = canonical_or_insert_in(&mut transaction, a, &participants).await?;
        transaction.commit().await.map_err(map_constraint_error)?;
        Ok(id)
    }

    /// The canonical direct conversation of this exact participant set, if there is one.
    ///
    /// # Errors
    /// Database failures.
    pub async fn canonical_direct_conversation(
        &self,
        participants: &[ActorRef],
    ) -> AppResult<Option<Uuid>> {
        let mut participants = participants.to_vec();
        canonicalize_participants(&mut participants);
        Ok(sqlx::query_scalar(
            "SELECT id FROM conversations
             WHERE participant_set_hash = $1 AND NOT is_group AND superseded_by IS NULL",
        )
        .bind(participant_set_hash(&participants).as_slice())
        .fetch_optional(self.pool())
        .await?)
    }

    /// How many direct conversations this account started in the last hour.
    ///
    /// # Errors
    /// Database failures.
    pub async fn conversations_started_last_hour(&self, actor: &ActorRef) -> AppResult<i64> {
        Ok(sqlx::query_scalar(
            "SELECT count(*) FROM conversations
             WHERE created_by_kind = $1::text::actor_kind AND created_by_id = $2
               AND NOT is_group AND created_at > clock_timestamp() - interval '1 hour'",
        )
        .bind(actor.actor_type.as_str())
        .bind(actor.id.as_str())
        .fetch_one(self.pool())
        .await?)
    }

    /// How `caller` may use a conversation: a participant (group: accepted member), the
    /// custodian of a Silicon in it, a pending group invitee (or the custodian of a pending
    /// Silicon invitee) — or not at all (`None`).
    ///
    /// # Errors
    /// Database failures.
    pub async fn conversation_access(
        &self,
        caller: &ActorRef,
        conversation_id: Uuid,
    ) -> AppResult<Option<ConversationAccess>> {
        let row: Option<(bool, bool, bool)> = sqlx::query_as(
            "SELECT
                EXISTS (SELECT 1 FROM effective_conversation_participants p
                        WHERE p.conversation_id = c.id AND p.actor_kind = $2::text::actor_kind
                          AND p.actor_id = $3),
                EXISTS (SELECT 1 FROM effective_conversation_participants p
                        JOIN accounts a ON a.uuid = p.actor_id
                        WHERE p.conversation_id = c.id AND p.actor_kind = 'silicon'
                          AND a.custodian_uuid = $3 AND $2 = 'carbon'),
                EXISTS (SELECT 1 FROM group_invitations i
                        WHERE i.conversation_id = c.id AND i.status = 'pending'
                          AND ((i.actor_kind = $2::text::actor_kind AND i.actor_id = $3)
                            OR ($2 = 'carbon' AND i.actor_kind = 'silicon' AND EXISTS (
                                SELECT 1 FROM accounts a WHERE a.uuid = i.actor_id AND a.custodian_uuid = $3))))
             FROM conversations c WHERE c.id = $1",
        )
        .bind(conversation_id)
        .bind(caller.actor_type.as_str())
        .bind(caller.id.as_str())
        .fetch_optional(self.pool())
        .await?;
        Ok(match row {
            Some((true, _, _)) => Some(ConversationAccess::Participant),
            Some((false, true, _)) => Some(ConversationAccess::Custodian),
            Some((false, false, true)) => Some(ConversationAccess::Invitee),
            _ => None,
        })
    }

    /// Requires `viewer` to be able to read the conversation: a participant, or the
    /// custodian of a Silicon in it.
    ///
    /// # Errors
    /// Not found for anyone else; database failures.
    pub async fn require_reader(&self, viewer: &ActorRef, conversation_id: Uuid) -> AppResult<()> {
        match self.conversation_access(viewer, conversation_id).await? {
            Some(ConversationAccess::Participant | ConversationAccess::Custodian) => Ok(()),
            _ => Err(AppError::NotFound),
        }
    }

    /// The storage partition of a conversation.
    ///
    /// # Errors
    /// Not found, or database failures.
    pub async fn partition_of(&self, conversation_id: Uuid) -> AppResult<OrganizationId> {
        let partition: String =
            sqlx::query_scalar("SELECT organization_id FROM conversations WHERE id = $1")
                .bind(conversation_id)
                .fetch_optional(self.pool())
                .await?
                .ok_or(AppError::NotFound)?;
        partition
            .parse()
            .map_err(|error| super::rows::data_error("conversation partition", error))
    }

    /// Refuses a write into a conversation that was superseded by a canonical one, or a
    /// direct conversation whose other account was deleted (its history stays readable).
    ///
    /// # Errors
    /// `conversation_superseded` (409) naming the conversation to use instead;
    /// `recipient_deleted` (409).
    pub async fn require_current(&self, conversation_id: Uuid) -> AppResult<()> {
        let closed: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM conversations c
                JOIN conversation_participants p ON p.conversation_id = c.id
                JOIN accounts a ON a.uuid = p.actor_id AND a.status = 'deleted'
                WHERE c.id = $1 AND NOT c.is_group)",
        )
        .bind(conversation_id)
        .fetch_one(self.pool())
        .await?;
        if closed {
            return Err(AppError::refused(
                http::StatusCode::CONFLICT,
                "recipient_deleted",
                "The other account in this conversation was deleted. Its history stays readable; it takes no new messages.",
            ));
        }
        let superseded: Option<String> = sqlx::query_scalar(
            "SELECT address.public_id FROM conversations c
             JOIN conversation_addresses address ON address.id = c.superseded_by
             WHERE c.id = $1 AND c.superseded_by IS NOT NULL",
        )
        .bind(conversation_id)
        .fetch_optional(self.pool())
        .await?;
        match superseded {
            Some(canonical) => Err(AppError::refused(
                http::StatusCode::CONFLICT,
                "conversation_superseded",
                format!(
                    "This conversation is kept for its history; the same accounts now talk in {canonical}. Send there."
                ),
            )),
            None => Ok(()),
        }
    }
}

/// Finds the canonical conversation of the set, or creates it in the @accounts partition.
async fn canonical_or_insert_in(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    creator: &ActorRef,
    participants: &[ActorRef],
) -> AppResult<Uuid> {
    let hash = participant_set_hash(participants);
    if let Some(id) = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM conversations
         WHERE participant_set_hash = $1 AND NOT is_group AND superseded_by IS NULL",
    )
    .bind(hash.as_slice())
    .fetch_optional(&mut **transaction)
    .await?
    {
        return Ok(id);
    }
    let candidate = Uuid::now_v7();
    let inserted = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO conversations (id, organization_id, participant_set_hash, created_by_kind, created_by_id)
         VALUES ($1, '@accounts', $2, $3::text::actor_kind, $4)
         ON CONFLICT (participant_set_hash) WHERE NOT is_group AND superseded_by IS NULL DO NOTHING
         RETURNING id",
    )
    .bind(candidate)
    .bind(hash.as_slice())
    .bind(creator.actor_type.as_str())
    .bind(creator.id.as_str())
    .fetch_optional(&mut **transaction)
    .await
    .map_err(map_constraint_error)?;
    let Some(id) = inserted else {
        // A concurrent creator won; its row is visible once it commits.
        return sqlx::query_scalar(
            "SELECT id FROM conversations
             WHERE participant_set_hash = $1 AND NOT is_group AND superseded_by IS NULL",
        )
        .bind(hash.as_slice())
        .fetch_optional(&mut **transaction)
        .await?
        .ok_or_else(|| AppError::conflict("the conversation is being created; retry"));
    };
    for participant in participants {
        sqlx::query(
            "INSERT INTO conversation_participants (conversation_id, organization_id, actor_kind, actor_id)
             VALUES ($1, '@accounts', $2::text::actor_kind, $3)",
        )
        .bind(id)
        .bind(participant.actor_type.as_str())
        .bind(participant.id.as_str())
        .execute(&mut **transaction)
        .await
        .map_err(map_constraint_error)?;
    }
    Ok(id)
}

impl PostgresStore {
    /// Lists the conversations of `subject` (the caller, or a Silicon the caller is
    /// custodian of) in newest-activity order. Superseded conversations are left out;
    /// the canonical one names them in `supersedes`.
    ///
    /// # Errors
    /// Rejects an invalid cursor or page length; propagates database failures.
    pub async fn list_conversations(
        &self,
        viewer: &ActorRef,
        subject: &ActorRef,
        page: &PageRequest,
        groups_only: bool,
    ) -> AppResult<ConversationPage> {
        let limit = page.validated_limit()?;
        let cursor = page
            .cursor
            .as_deref()
            .map(|value| Cursor::decode(value, "conversations"))
            .transpose()?;
        if cursor
            .as_ref()
            .is_some_and(|value| value.activity_position().is_none())
        {
            return Err(AppError::validation(
                "cursor is invalid for conversation pagination",
            ));
        }
        let (cursor_time, cursor_id) = cursor
            .as_ref()
            .and_then(Cursor::activity_position)
            .map_or((None, None), |(time, id)| (Some(time), Some(id)));
        let records = sqlx::query_as::<_, ConversationRecord>(sqlx::AssertSqlSafe(format!(
            r"
            SELECT {CONVERSATION_COLUMNS}
            FROM conversations AS conversation
            JOIN effective_conversation_participants AS participant
              ON participant.conversation_id = conversation.id
             AND participant.organization_id = conversation.organization_id
            WHERE participant.actor_kind = $1::text::actor_kind
              AND participant.actor_id = $2
              AND conversation.superseded_by IS NULL
              AND (NOT $6 OR conversation.is_group)
              AND ($3::timestamptz IS NULL OR (conversation.updated_at, conversation.id) < ($3, $4))
            ORDER BY conversation.updated_at DESC, conversation.id DESC
            LIMIT $5
            "
        )))
        .bind(subject.actor_type.as_str())
        .bind(subject.id.as_str())
        .bind(cursor_time)
        .bind(cursor_id)
        .bind(i64::from(limit) + 1)
        .bind(groups_only)
        .fetch_all(self.pool())
        .await?;
        self.conversation_page(viewer, subject, records, limit)
            .await
    }

    /// One conversation, for a participant or the custodian of a Silicon in it.
    ///
    /// # Errors
    /// Not found when the viewer may not read it; database failures.
    pub async fn get_conversation(
        &self,
        viewer: &ActorRef,
        conversation_id: Uuid,
    ) -> AppResult<Conversation> {
        self.get_conversation_for(viewer, viewer, conversation_id)
            .await
    }

    /// One conversation as `viewer` may see it, with the unread facts of `subject` (the
    /// viewer, or a Silicon it looks after).
    ///
    /// # Errors
    /// Not found when the viewer may not read it; database failures.
    pub async fn get_conversation_for(
        &self,
        viewer: &ActorRef,
        subject: &ActorRef,
        conversation_id: Uuid,
    ) -> AppResult<Conversation> {
        match self.conversation_access(viewer, conversation_id).await? {
            Some(ConversationAccess::Participant | ConversationAccess::Custodian) => {}
            _ => return Err(AppError::NotFound),
        }
        let record = sqlx::query_as::<_, ConversationRecord>(sqlx::AssertSqlSafe(format!(
            "SELECT {CONVERSATION_COLUMNS} FROM conversations AS conversation WHERE conversation.id = $1"
        )))
        .bind(conversation_id)
        .fetch_optional(self.pool())
        .await?
        .ok_or(AppError::NotFound)?;
        let page = self
            .conversation_page(viewer, subject, vec![record], 1)
            .await?;
        page.items.into_iter().next().ok_or(AppError::NotFound)
    }

    /// Requires `actor` to be an effective participant and returns the conversation's
    /// storage partition.
    ///
    /// # Errors
    /// Not found for anyone else (existence is not disclosed); database failures.
    pub async fn require_participant(
        &self,
        actor: &ActorRef,
        conversation_id: Uuid,
    ) -> AppResult<OrganizationId> {
        let partition: Option<String> = sqlx::query_scalar(
            "SELECT organization_id FROM effective_conversation_participants
             WHERE conversation_id = $1 AND actor_kind = $2::text::actor_kind AND actor_id = $3",
        )
        .bind(conversation_id)
        .bind(actor.actor_type.as_str())
        .bind(actor.id.as_str())
        .fetch_optional(self.pool())
        .await?;
        partition
            .ok_or(AppError::NotFound)?
            .parse()
            .map_err(|error| super::rows::data_error("conversation partition", error))
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one page: participants, custodians, supersession and messages"
    )]
    async fn conversation_page(
        &self,
        viewer: &ActorRef,
        subject: &ActorRef,
        mut records: Vec<ConversationRecord>,
        limit: u16,
    ) -> AppResult<ConversationPage> {
        let total_records = records.len();
        if total_records > usize::from(limit) {
            records.truncate(usize::from(limit));
        }
        let conversation_ids = records.iter().map(|record| record.id).collect::<Vec<_>>();
        let mut participants: HashMap<Uuid, Vec<ActorRef>> = HashMap::new();
        let mut custodians: HashMap<Uuid, Vec<ActorRef>> = HashMap::new();
        let mut supersedes: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
        if !conversation_ids.is_empty() {
            // Direct conversations keep showing a deleted account (rendered `deleted`);
            // groups show their current members.
            for record in sqlx::query_as::<_, ParticipantRecord>(
                "SELECT DISTINCT p.conversation_id, p.actor_kind::text AS actor_kind, p.actor_id
                 FROM conversation_participants p
                 JOIN conversations c ON c.id = p.conversation_id
                 WHERE p.conversation_id = ANY($1)
                   AND (NOT c.is_group OR EXISTS (SELECT 1 FROM effective_conversation_participants e
                        WHERE e.conversation_id = p.conversation_id AND e.actor_kind = p.actor_kind
                          AND e.actor_id = p.actor_id))
                 ORDER BY 1, 2, 3",
            )
            .bind(&conversation_ids)
            .fetch_all(self.pool())
            .await?
            {
                participants
                    .entry(record.conversation_id)
                    .or_default()
                    .push(map_participant(&record)?);
            }
            // Custodians who can read a conversation without taking part in it.
            for (conversation_id, custodian) in sqlx::query_as::<_, (Uuid, String)>(
                "SELECT DISTINCT p.conversation_id, a.custodian_uuid
                 FROM effective_conversation_participants p
                 JOIN accounts a ON a.uuid = p.actor_id
                 WHERE p.conversation_id = ANY($1) AND p.actor_kind = 'silicon'
                   AND a.custodian_uuid IS NOT NULL
                   AND NOT EXISTS (SELECT 1 FROM effective_conversation_participants other
                       WHERE other.conversation_id = p.conversation_id
                         AND other.actor_kind = 'carbon' AND other.actor_id = a.custodian_uuid)
                 ORDER BY 1, 2",
            )
            .bind(&conversation_ids)
            .fetch_all(self.pool())
            .await?
            {
                custodians
                    .entry(conversation_id)
                    .or_default()
                    .push(ActorRef {
                        actor_type: ActorType::Carbon,
                        id: ActorId::from_storage_key(custodian),
                    });
            }
            for (canonical, old) in sqlx::query_as::<_, (Uuid, Uuid)>(
                "SELECT superseded_by, id FROM conversations WHERE superseded_by = ANY($1) ORDER BY id",
            )
            .bind(&conversation_ids)
            .fetch_all(self.pool())
            .await?
            {
                supersedes.entry(canonical).or_default().push(old);
            }
        }
        let mut unread = self.unread_summaries(subject, &conversation_ids).await?;
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
            let summary = unread.remove(&record.id).unwrap_or_default();
            items.push(Conversation {
                id: record.id,
                participants: participants.remove(&record.id).unwrap_or_default(),
                visible_to_custodians: custodians.remove(&record.id).unwrap_or_default(),
                superseded_by: record.superseded_by,
                supersedes: supersedes.remove(&record.id).unwrap_or_default(),
                group: self.group_details(viewer, record.id).await?,
                last_message,
                unread_count: summary.unread_count,
                last_read_message_id: summary
                    .last_read_sequence
                    .and_then(silicon_dm_protocol::message_code),
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

/// Sorts participants by (kind, storage key) and drops duplicates.
pub(crate) fn canonicalize_participants(participants: &mut Vec<ActorRef>) {
    participants.sort_by(|left, right| {
        (left.actor_type.as_str(), left.id.as_str())
            .cmp(&(right.actor_type.as_str(), right.id.as_str()))
    });
    participants.dedup();
}

/// SHA-256 over the length-framed, sorted (kind, storage key) pairs; the same bytes the
/// 0036 migration and `dm-migrate link-identities` compute in SQL.
pub(crate) fn participant_set_hash(participants: &[ActorRef]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    for participant in participants {
        let kind = participant.actor_type.as_str().as_bytes();
        let id = participant.id.as_str().as_bytes();
        let kind_length = u64::try_from(kind.len()).unwrap_or(u64::MAX);
        let id_length = u64::try_from(id.len()).unwrap_or(u64::MAX);
        hasher.update(kind_length.to_be_bytes());
        hasher.update(kind);
        hasher.update(id_length.to_be_bytes());
        hasher.update(id);
    }
    hasher.finalize().into()
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
            id: "zQo".parse()?,
        };
        let silicon = ActorRef {
            actor_type: ActorType::Silicon,
            id: "K1E".parse()?,
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
            id: "zQo".parse()?,
        };
        let mut participants = vec![actor.clone(), actor];
        canonicalize_participants(&mut participants);
        assert_eq!(participants.len(), 1);
        Ok(())
    }

    #[test]
    fn uuids_differing_only_in_case_are_different_accounts()
    -> Result<(), Box<dyn std::error::Error>> {
        let lower = ActorRef {
            actor_type: ActorType::Carbon,
            id: "a8K".parse()?,
        };
        let upper = ActorRef {
            actor_type: ActorType::Carbon,
            id: "A8k".parse()?,
        };
        let mut participants = vec![lower.clone(), upper.clone(), lower];
        canonicalize_participants(&mut participants);
        assert_eq!(participants.len(), 2);
        assert_ne!(
            participant_set_hash(&participants[..1]),
            participant_set_hash(&participants[1..])
        );
        Ok(())
    }
}
