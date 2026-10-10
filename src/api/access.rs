//! Turning the addresses people use into conversations, and deciding access.
//!
//! A conversation path segment may be a conversation UUID, a group id (`g:slug` or the
//! older `g:org:slug`), a direct address of current public ids (`c:ada::si:scout`), or one
//! recipient (`c:ada`, `isi@si:scout`, or an account uuid) meaning the canonical direct
//! conversation with that account.

use uuid::Uuid;

use crate::{
    AppError, AppResult,
    application::{auth::AuthContext, commands::CreateConversationCommand, state::AppState},
    domain::{Account, ActorRef, ConversationAccess, is_account_uuid},
};

/// First contacts (new direct conversations) one account may start per hour.
pub(crate) const FIRST_CONTACTS_PER_HOUR: i64 = 100;

/// Resolves a conversation path segment and decides access.
///
/// # Errors
/// Not found when the conversation does not exist or the caller may not see it (the
/// difference is not disclosed); refusals for new conversations the caller may not start.
pub(crate) async fn resolve_conversation_path(
    state: &AppState,
    context: &AuthContext,
    value: &str,
    create: bool,
) -> AppResult<(Uuid, ConversationAccess)> {
    // Account UUIDs and conversation UUIDs share a representation. A visible
    // existing conversation wins; otherwise resolve the UUID through Accounts.
    if let Ok(id) = Uuid::parse_str(value)
        && let Some(access) = state.store.conversation_access(&context.actor, id).await?
    {
        return Ok((id, access));
    }
    let id = if value.starts_with("g:") || value.contains("::") {
        resolve_address(state, value).await?
    } else {
        recipient_conversation(state, context, value, create).await?
    };
    let access = state
        .store
        .conversation_access(&context.actor, id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok((id, access))
}

/// Resolves a group path segment (group id or UUID) and decides access.
///
/// # Errors
/// Not found for anything that is not a group the caller may see.
pub(crate) async fn resolve_group_path(
    state: &AppState,
    context: &AuthContext,
    value: &str,
) -> AppResult<(Uuid, ConversationAccess)> {
    let id = if let Ok(id) = Uuid::parse_str(value) {
        id
    } else if silicon_dm_protocol::valid_group_id(value) {
        state
            .store
            .group_by_public_id(value)
            .await?
            .ok_or(AppError::NotFound)?
    } else {
        return Err(AppError::validation(
            "expected a group id such as g:product-design (groups from before contract 4 keep a three-part id such as g:tos:product-design)",
        ));
    };
    if !state.store.is_group(id).await? {
        return Err(AppError::NotFound);
    }
    let access = state
        .store
        .conversation_access(&context.actor, id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok((id, access))
}

/// A UUID, a group id or a direct address.
async fn resolve_address(state: &AppState, value: &str) -> AppResult<Uuid> {
    if let Ok(id) = Uuid::parse_str(value) {
        return Ok(id);
    }
    if value.starts_with("g:") {
        if !silicon_dm_protocol::valid_group_id(value) {
            return Err(AppError::validation(
                "expected a group id such as g:product-design (groups from before contract 4 keep a three-part id such as g:tos:product-design)",
            ));
        }
        return state
            .store
            .group_by_public_id(value)
            .await?
            .ok_or(AppError::NotFound);
    }
    let parts: Vec<&str> = value.split("::").collect();
    if parts.len() != 2 || parts.iter().any(|part| part.is_empty()) {
        return Err(AppError::validation(
            "a direct address is two account ids joined by ::, for example c:ada::si:scout",
        ));
    }
    let mut participants = Vec::with_capacity(2);
    for part in &parts {
        match state.directory().resolve(part).await {
            Ok(account) => participants.push(account.actor()),
            Err(error)
                if matches!(
                    error.code(),
                    "account_not_found" | "account_deleted" | "validation_error"
                ) =>
            {
                // A conversation with an IAM-era identity that was never linked, or with a
                // deleted account (addressed by its uuid): its address is stored as is.
                return state
                    .store
                    .conversation_by_stored_address(value)
                    .await?
                    .ok_or(AppError::NotFound);
            }
            Err(error) => return Err(error),
        }
    }
    state
        .store
        .canonical_direct_conversation(&participants)
        .await?
        .ok_or(AppError::NotFound)
}

/// The canonical direct conversation with one recipient; created when sending.
async fn recipient_conversation(
    state: &AppState,
    context: &AuthContext,
    value: &str,
    create: bool,
) -> AppResult<Uuid> {
    let recipient = state.directory().resolve(value).await?;
    if recipient.uuid == context.actor.id.as_str() {
        return Err(AppError::validation(
            "the recipient must be another account (you cannot message yourself)",
        ));
    }
    let participants = vec![context.actor.clone(), recipient.actor()];
    if let Some(id) = state
        .store
        .canonical_direct_conversation(&participants)
        .await?
    {
        return Ok(id);
    }
    if !create {
        return Err(AppError::NotFound);
    }
    start_conversation(state, context, &[recipient], None).await
}

/// Starts (or resolves) a direct conversation between the caller and `others`, applying
/// the reach rules to a conversation that does not exist yet: Silicons only hear from
/// their circle and their allow-list, and every account has a first-contact budget.
///
/// # Errors
/// `contact_not_allowed`, `first_contact_limit`, or storage failures.
pub(crate) async fn start_conversation(
    state: &AppState,
    context: &AuthContext,
    others: &[Account],
    idempotency_key: Option<crate::domain::IdempotencyKey>,
) -> AppResult<Uuid> {
    let mut participants: Vec<ActorRef> = others.iter().map(Account::actor).collect();
    participants.push(context.actor.clone());
    if let Some(existing) = state
        .store
        .canonical_direct_conversation(&participants)
        .await?
    {
        return Ok(existing);
    }
    for other in others {
        if other.deleted {
            return Err(AppError::account_deleted(other.display_id()));
        }
        state
            .directory()
            .require_may_reach(&context.actor, other)
            .await?;
    }
    if state
        .store
        .conversations_started_last_hour(&context.actor)
        .await?
        >= FIRST_CONTACTS_PER_HOUR
    {
        return Err(AppError::refused(
            http::StatusCode::TOO_MANY_REQUESTS,
            "first_contact_limit",
            format!(
                "You started {FIRST_CONTACTS_PER_HOUR} new conversations in the last hour, the most DM allows. Existing conversations keep working; start new ones later."
            ),
        ));
    }
    let key = if let Some(key) = idempotency_key {
        key
    } else {
        let mut ids: Vec<&str> = participants.iter().map(|actor| actor.id.as_str()).collect();
        ids.sort_unstable();
        format!(
            "recipient-chat:{}",
            blake3::hash(ids.join(",").as_bytes()).to_hex()
        )
        .parse()
        .map_err(|_| AppError::validation("invalid idempotency key"))?
    };
    let conversation = state
        .store
        .create_conversation(CreateConversationCommand {
            creator: context.actor.clone(),
            participants,
            idempotency_key: key,
        })
        .await?;
    Ok(conversation.id)
}

/// Whether a path segment names one account (as opposed to a conversation address).
#[must_use]
pub(crate) fn names_one_account(value: &str) -> bool {
    !(value.starts_with("g:") || value.contains("::"))
        && (value.contains(':') || is_account_uuid(value))
}
