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
        resolve_address(state, value).await
    } else {
        recipient_conversation(state, context, value, create).await
    }
    .map_err(|error| name_missing_conversation(error, value))?;
    let access = state
        .store
        .conversation_access(&context.actor, id)
        .await?
        .ok_or_else(|| conversation_not_found(value))?;
    Ok((id, access))
}

/// What to show for a value people type: at most 200 characters.
fn shown(value: &str) -> String {
    let mut shown: String = value.chars().take(200).collect();
    if shown.len() < value.len() {
        shown.push('…');
    }
    shown
}

/// 404 for a conversation the caller cannot see: it does not exist, or the caller is not
/// in it. DM does not say which, so nobody can probe for conversations they cannot read.
fn conversation_not_found(value: &str) -> AppError {
    let (what, hint) = if value.starts_with("g:") {
        (
            "group",
            "dm groups list shows the groups you are in, and dm groups invitations the ones waiting for you.",
        )
    } else {
        (
            "conversation",
            "dm conversations list shows yours (GET /api/v1/conversations). A direct address is two current ids joined by ::, such as c:ada::si:scout; a group's is its id, such as g:product-design.",
        )
    };
    AppError::not_found(format!(
        "No {what} {} is visible to you: it does not exist, or you are not in it (DM does not say which).",
        shown(value)
    ))
    .with_hint(hint)
}

/// Gives a bare not-found from resolving an address its words; every other error stays.
/// A UUID that is neither a conversation the caller sees nor an account keeps its code
/// (`account_not_found`, what it has always answered) but says both were looked for.
fn name_missing_conversation(error: AppError, value: &str) -> AppError {
    if matches!(error, AppError::NotFound) {
        conversation_not_found(value)
    } else if error.code() == "account_not_found" && Uuid::parse_str(value).is_ok() {
        AppError::refused(
            axum::http::StatusCode::NOT_FOUND,
            "account_not_found",
            format!(
                "Neither a conversation you can see nor a Silicon Accounts account has the id {value}."
            ),
        )
        .with_hint("dm conversations list shows your conversations and their addresses; an account is named by its current c:/si: id or its uuid.")
    } else {
        error
    }
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
            .ok_or_else(|| group_not_found(value))?
    } else {
        return Err(not_a_group_id(value));
    };
    if !state.store.is_group(id).await? {
        return Err(group_not_found(value));
    }
    let access = state
        .store
        .conversation_access(&context.actor, id)
        .await?
        .ok_or_else(|| group_not_found(value))?;
    Ok((id, access))
}

/// 404 for a group the caller cannot see: it does not exist, or the caller is neither a
/// member nor invited. DM does not say which.
fn group_not_found(value: &str) -> AppError {
    AppError::not_found(format!(
        "No group {} is visible to you: it does not exist, or you are not a member of it (DM does not say which).",
        shown(value)
    ))
    .with_hint(
        "dm groups list shows the groups you are in (GET /api/v1/conversations lists them with your other conversations), and dm groups invitations the ones waiting for you.",
    )
}

fn not_a_group_id(value: &str) -> AppError {
    AppError::invalid(format!(
        "{} is not a group id: a group id is g: and the group's name in lowercase letters, digits and dashes, such as g:product-design (groups from before contract 4 keep a three-part id such as g:tos:product-design).",
        shown(value)
    ))
    .with_hint("dm groups list shows each group with its id.")
}

/// A UUID, a group id or a direct address.
async fn resolve_address(state: &AppState, value: &str) -> AppResult<Uuid> {
    if let Ok(id) = Uuid::parse_str(value) {
        return Ok(id);
    }
    if value.starts_with("g:") {
        if !silicon_dm_protocol::valid_group_id(value) {
            return Err(not_a_group_id(value));
        }
        return state
            .store
            .group_by_public_id(value)
            .await?
            .ok_or(AppError::NotFound);
    }
    let parts: Vec<&str> = value.split("::").collect();
    if parts.len() != 2 || parts.iter().any(|part| part.is_empty()) {
        return Err(AppError::invalid(format!(
            "{} is not a direct address: a direct address is two account ids joined by ::, for example c:ada::si:scout.",
            shown(value)
        ))
        .with_hint("dm conversations list shows each conversation with its address."));
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
        return Err(AppError::not_found(format!(
            "You have no conversation with {} yet.",
            recipient.display_id()
        ))
        .with_hint(format!(
            "Send a message to start it: dm messages send {} --text '…' (POST /api/v1/messages with recipient_id).",
            recipient.display_id()
        )));
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
    if let Some(wait) = state
        .store
        .first_contact_wait(&context.actor, FIRST_CONTACTS_PER_HOUR)
        .await?
    {
        return Err(first_contact_limit(wait));
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

/// 429 for an account that started its hour's new conversations, saying when one fits.
fn first_contact_limit(wait_seconds: f64) -> AppError {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to 1..=3600 first"
    )]
    let seconds = wait_seconds.ceil().clamp(1.0, 3600.0) as u64;
    let when = if seconds < 90 {
        format!("{seconds} seconds")
    } else {
        format!("{} minutes", seconds.div_ceil(60))
    };
    AppError::refused(
        http::StatusCode::TOO_MANY_REQUESTS,
        "first_contact_limit",
        format!(
            "You started {FIRST_CONTACTS_PER_HOUR} new conversations in the last hour, the most DM allows. Existing conversations keep working; start new ones later."
        ),
    )
    .with_hint(format!(
        "The next new conversation fits in {when} (Retry-After). Messages to conversations you already have are not limited."
    ))
    .with_retry_after(seconds)
    .with_details(serde_json::json!({
        "limit": FIRST_CONTACTS_PER_HOUR,
        "window_seconds": 3600,
        "retry_after_seconds": seconds,
    }))
}

/// Whether a path segment names one account (as opposed to a conversation address).
#[must_use]
pub(crate) fn names_one_account(value: &str) -> bool {
    !(value.starts_with("g:") || value.contains("::"))
        && (value.contains(':') || is_account_uuid(value))
}

#[cfg(test)]
mod tests {
    use axum::response::IntoResponse as _;

    #[tokio::test]
    async fn the_first_contact_limit_says_when_the_next_conversation_fits()
    -> Result<(), Box<dyn std::error::Error>> {
        let response = super::first_contact_limit(702.2).into_response();
        assert_eq!(response.status(), http::StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers()["retry-after"], "703");
        let body: serde_json::Value =
            serde_json::from_slice(&axum::body::to_bytes(response.into_body(), 4096).await?)?;
        assert_eq!(body["error"]["code"], "first_contact_limit");
        assert_eq!(body["error"]["details"]["limit"], 100);
        assert_eq!(body["error"]["details"]["retry_after_seconds"], 703);
        assert!(
            body["error"]["hint"]
                .as_str()
                .is_some_and(|hint| hint.contains("12 minutes")),
            "{body}"
        );
        Ok(())
    }
}
