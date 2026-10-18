//! Conversation events: per-account references to a conversation that appeared, changed or
//! went away for that account, written in the transaction that made the change and read by
//! `/api/v1/sync?include=conversations` in the same stream as message events.

use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use super::{map_constraint_error, rows::parse_actor_type};
use crate::{
    AppResult,
    domain::{ActorId, ActorRef, ConversationChange},
};

/// Records `events` (account, change) for one conversation and wakes those accounts' sync
/// long-polls after commit. See [`record_many_in`].
///
/// # Errors
/// Database failures.
pub(crate) async fn record_in(
    tx: &mut Transaction<'_, Postgres>,
    conversation_id: Uuid,
    events: &[(ActorRef, ConversationChange)],
) -> AppResult<()> {
    let events: Vec<(Uuid, ActorRef, ConversationChange)> = events
        .iter()
        .map(|(account, change)| (conversation_id, account.clone(), *change))
        .collect();
    record_many_in(tx, &events).await
}

/// Records (conversation, account, change) events and wakes those accounts' sync long-polls
/// after commit. Exact duplicates are dropped. Rows go in (kind, id) order across all of them,
/// the order message deliveries take the same stream locks in, so concurrent writers never
/// wait on each other in a cycle.
///
/// # Errors
/// Database failures.
pub(crate) async fn record_many_in(
    tx: &mut Transaction<'_, Postgres>,
    events: &[(Uuid, ActorRef, ConversationChange)],
) -> AppResult<()> {
    let mut events: Vec<&(Uuid, ActorRef, ConversationChange)> = events.iter().collect();
    events.sort_by(
        |(left_conversation, left, left_change), (right_conversation, right, right_change)| {
            (
                left.actor_type.as_str(),
                left.id.as_str(),
                left_conversation,
                left_change,
            )
                .cmp(&(
                    right.actor_type.as_str(),
                    right.id.as_str(),
                    right_conversation,
                    right_change,
                ))
        },
    );
    events.dedup();
    if events.is_empty() {
        return Ok(());
    }
    let ids: Vec<Uuid> = events.iter().map(|_| Uuid::now_v7()).collect();
    let conversations: Vec<Uuid> = events
        .iter()
        .map(|(conversation, _, _)| *conversation)
        .collect();
    let kinds: Vec<&str> = events
        .iter()
        .map(|(_, account, _)| account.actor_type.as_str())
        .collect();
    let targets: Vec<&str> = events
        .iter()
        .map(|(_, account, _)| account.id.as_str())
        .collect();
    let changes: Vec<&str> = events
        .iter()
        .map(|(_, _, change)| change.as_str())
        .collect();
    sqlx::query(
        "INSERT INTO conversation_events (id, target_kind, target_id, conversation_id, change, sequence)
         SELECT e.id, e.kind::actor_kind, e.target, e.conversation, e.change, 1
         FROM unnest($1::uuid[], $2::text[], $3::text[], $4::uuid[], $5::text[])
             WITH ORDINALITY AS e(id, kind, target, conversation, change, position)
         ORDER BY e.kind::actor_kind, e.target, e.position",
    )
    .bind(&ids)
    .bind(&kinds)
    .bind(&targets)
    .bind(&conversations)
    .bind(&changes)
    .execute(&mut **tx)
    .await
    .map_err(map_constraint_error)?;
    let mut woken: Vec<&str> = Vec::new();
    for target in targets {
        if !woken.contains(&target) {
            woken.push(target);
            super::messages::notify_account_in(tx, target).await?;
        }
    }
    Ok(())
}

/// The accounts in a group now (accepted members of accounts that still exist).
///
/// # Errors
/// Database failures.
pub(crate) async fn group_members_in(
    tx: &mut Transaction<'_, Postgres>,
    conversation_id: Uuid,
) -> AppResult<Vec<ActorRef>> {
    accounts_in(
        tx,
        "SELECT DISTINCT actor_kind::text, actor_id FROM eligible_group_members
         WHERE conversation_id = $1",
        conversation_id,
    )
    .await
}

/// The accounts with a pending invitation to a group.
///
/// # Errors
/// Database failures.
pub(crate) async fn group_invitees_in(
    tx: &mut Transaction<'_, Postgres>,
    conversation_id: Uuid,
) -> AppResult<Vec<ActorRef>> {
    accounts_in(
        tx,
        "SELECT actor_kind::text, actor_id FROM group_invitations
         WHERE conversation_id = $1 AND status = 'pending'",
        conversation_id,
    )
    .await
}

/// The accounts of a conversation that still exist (direct conversations included).
///
/// # Errors
/// Database failures.
pub(crate) async fn participants_in(
    tx: &mut Transaction<'_, Postgres>,
    conversation_id: Uuid,
) -> AppResult<Vec<ActorRef>> {
    accounts_in(
        tx,
        "SELECT DISTINCT actor_kind::text, actor_id FROM effective_conversation_participants
         WHERE conversation_id = $1",
        conversation_id,
    )
    .await
}

async fn accounts_in(
    tx: &mut Transaction<'_, Postgres>,
    sql: &'static str,
    conversation_id: Uuid,
) -> AppResult<Vec<ActorRef>> {
    let rows: Vec<(String, String)> = sqlx::query_as(sql)
        .bind(conversation_id)
        .fetch_all(&mut **tx)
        .await?;
    rows.into_iter()
        .map(|(kind, id)| {
            Ok(ActorRef {
                actor_type: parse_actor_type(&kind)?,
                id: ActorId::from_storage_key(id),
            })
        })
        .collect()
}

/// `change` for each of `accounts`, except those in `except` (they get an event of their own).
#[must_use]
pub(crate) fn each(
    accounts: &[ActorRef],
    change: ConversationChange,
    except: &[ActorRef],
) -> Vec<(ActorRef, ConversationChange)> {
    accounts
        .iter()
        .filter(|account| !except.contains(account))
        .map(|account| (account.clone(), change))
        .collect()
}
