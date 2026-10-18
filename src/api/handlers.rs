//! Contracted HTTP request handlers.

use axum::{Json, extract::State, http::StatusCode};
use serde::Deserialize;
use uuid::Uuid;

use super::extract::{
    ApiJson, ApiPath, ApiQuery, Authenticated, ConversationAccess, Idempotency, IfMatch,
};
use crate::{
    AppError, AppResult,
    application::{
        auth::AuthContext,
        commands::{
            CreateBundleCommand, PutDraftCommand, PutDraftOutcome, RecordReceiptCommand,
            SendMessageCommand,
        },
        messaging::{prepare_message_content, validate_device_id},
        speech::prepare_message_speech,
        state::AppState,
    },
    domain::{
        ActorId, ActorRef, ActorType, Bundle, BundleCreate, BundleDetail, Conversation,
        ConversationPage, Draft, DraftInput, GifPage, MAX_CONVERSATION_PARTICIPANTS, Message,
        MessageCreate, MessagePage, PageRequest, Presence, ReceiptStatus,
    },
    infrastructure::postgres::{ActivityView, Reviser},
};

/// `GET /api/v1/conversations[?for=si:x]`: the caller's conversations, or (for a
/// custodian) those of a Silicon it looks after.
pub(super) async fn list_conversations(
    State(state): State<AppState>,
    Authenticated(context): Authenticated,
    ApiQuery(query): ApiQuery<ListConversationsQuery>,
) -> AppResult<Json<ConversationPage>> {
    let page = PageRequest {
        cursor: query.cursor,
        limit: query.limit,
    };
    page.validated_limit()?;
    let subject = match &query.for_account {
        Some(silicon) => looked_after(&state, &context, silicon).await?,
        None => context.actor.clone(),
    };
    let first = page.cursor.is_none();
    let mut listed = state
        .store
        .list_conversations(&context.actor, &subject, &page, false)
        .await?;
    // The first page says what is unread in all of them, so a badge or a window title need
    // not page through the list.
    if first {
        listed.unread = Some(state.store.unread_total(&subject).await?);
    }
    Ok(Json(listed))
}

/// `GET /api/v1/conversations/{conversation}[?for=si:x]`: one conversation summary, with
/// the unread facts of the caller (or of the Silicon a custodian names with `for`).
pub(super) async fn get_conversation(
    State(state): State<AppState>,
    Authenticated(context): Authenticated,
    ApiPath(path): ApiPath<ConversationPath>,
    ApiQuery(query): ApiQuery<ForQuery>,
) -> AppResult<Json<Conversation>> {
    let subject = match &query.for_account {
        Some(silicon) => looked_after(&state, &context, silicon).await?,
        None => context.actor.clone(),
    };
    state
        .store
        .get_conversation_for(&context.actor, &subject, path.conversation_id)
        .await
        .map(Json)
}

/// Resolves `for=si:x` and requires the caller to be that Silicon's custodian.
pub(super) async fn looked_after(
    state: &AppState,
    context: &AuthContext,
    selector: &str,
) -> AppResult<ActorRef> {
    context.require_own_session("Acting as a Silicon's custodian")?;
    let account = state.directory().resolve(selector).await?;
    if account.uuid == context.actor.id.as_str() {
        return Ok(context.actor.clone());
    }
    if !account.is_silicon() || account.custodian.as_deref() != Some(context.actor.id.as_str()) {
        return Err(AppError::forbidden(
            "not_custodian",
            format!(
                "You are not the custodian of {}; `for` only works for Silicons you look after.",
                account.display_id()
            ),
        )
        .with_hint(
            "Drop for to act as yourself. GET /api/v1/me (dm whoami) lists the Silicons you look after in custodian_of.",
        ));
    }
    Ok(account.actor())
}

/// Reads the latest version of an accessible conversation message.
pub(super) async fn get_message(
    State(state): State<AppState>,
    Authenticated(context): Authenticated,
    ApiPath(path): ApiPath<MessagePath>,
) -> AppResult<Json<Message>> {
    state
        .store
        .get_message(&context.actor, path.conversation_id, path.message_id)
        .await
        .map(Json)
}

/// Replaces content and appends the previous content to history (the sender only).
pub(super) async fn edit_message(
    State(state): State<AppState>,
    Authenticated(context): Authenticated,
    ApiPath(path): ApiPath<MessagePath>,
    Idempotency(key): Idempotency,
    ApiJson(mut input): ApiJson<MessageCreate>,
) -> AppResult<Json<Message>> {
    resolve_routing(&state, &context, &mut input).await?;
    let previous = match &input.speech {
        Some(_) => Some(
            state
                .store
                .get_message(&context.actor, path.conversation_id, path.message_id)
                .await?,
        ),
        None => None,
    };
    let speech = prepare_message_speech(
        &state,
        &context.actor,
        path.conversation_id,
        &mut input,
        previous
            .as_ref()
            .and_then(|message| message.speech.as_ref()),
    )
    .await?;
    state
        .store
        .revise_message(
            &context.actor,
            Reviser::Participant,
            path.conversation_id,
            path.message_id,
            &key,
            Some(input),
            speech,
            None,
        )
        .await
        .map(Json)
}

/// `DELETE …/messages/{message}[?for=si:x]`: tombstones a message. Its sender may, and so
/// may the custodian of the Silicon that sent it, whether or not the custodian is in the
/// conversation; with `for` (a custodian's view of its Silicon) only that Silicon's messages.
pub(super) async fn delete_message(
    State(state): State<AppState>,
    Authenticated(context): Authenticated,
    access: ConversationAccess,
    ApiPath(path): ApiPath<MessagePath>,
    ApiQuery(query): ApiQuery<ForQuery>,
    Idempotency(key): Idempotency,
) -> AppResult<Json<Message>> {
    let reviser = match access {
        ConversationAccess::Participant => Reviser::Participant,
        ConversationAccess::Custodian => Reviser::Custodian,
        ConversationAccess::Invitee => return Err(AppError::NotFound),
    };
    let silicon = match &query.for_account {
        Some(selector) => Some(looked_after(&state, &context, selector).await?)
            .filter(|silicon| *silicon != context.actor),
        None => None,
    };
    state
        .store
        .revise_message(
            &context.actor,
            reviser,
            path.conversation_id,
            path.message_id,
            &key,
            None,
            None,
            silicon.as_ref(),
        )
        .await
        .map(Json)
}

/// Creates or resolves the canonical direct conversation of an exact participant set.
pub(super) async fn create_conversation(
    State(state): State<AppState>,
    Authenticated(context): Authenticated,
    Idempotency(idempotency_key): Idempotency,
    ApiJson(request): ApiJson<CreateConversationRequest>,
) -> AppResult<(StatusCode, Json<Conversation>)> {
    if request.participant_ids.is_empty() {
        return Err(AppError::validation(
            "participant_ids must contain at least one account id",
        ));
    }
    if request.participant_ids.len() >= MAX_CONVERSATION_PARTICIPANTS {
        return Err(AppError::validation(
            "a conversation may contain at most 100 unique participants",
        ));
    }
    let accounts = state
        .directory()
        .resolve_all(&request.participant_ids)
        .await?;
    let others: Vec<_> = accounts
        .into_iter()
        .filter(|account| account.uuid != context.actor.id.as_str())
        .collect();
    if others.is_empty() {
        return Err(AppError::validation(
            "a conversation requires at least two unique participants",
        ));
    }
    let id =
        super::access::start_conversation(&state, &context, &others, Some(idempotency_key)).await?;
    let conversation = state.store.get_conversation(&context.actor, id).await?;
    Ok((StatusCode::CREATED, Json(conversation)))
}

/// Lists one conversation's newest durable messages.
pub(super) async fn list_messages(
    State(state): State<AppState>,
    Authenticated(context): Authenticated,
    ApiPath(path): ApiPath<ConversationPath>,
    ApiQuery(query): ApiQuery<MessageListQuery>,
) -> AppResult<Json<MessagePage>> {
    let page = PageRequest {
        cursor: query.cursor,
        limit: query.limit,
    };
    page.validated_limit()?;
    state
        .store
        .list_messages(
            &context.actor,
            path.conversation_id,
            &page,
            query.include_bundled_members,
        )
        .await
        .map(Json)
}

/// Validates, persists, and durably queues a message.
pub(super) async fn send_message(
    State(state): State<AppState>,
    Authenticated(context): Authenticated,
    ApiPath(path): ApiPath<ConversationPath>,
    Idempotency(idempotency_key): Idempotency,
    ApiJson(body): ApiJson<MessageCreate>,
) -> AppResult<(StatusCode, Json<Message>)> {
    send(
        &state,
        &context,
        path.conversation_id,
        idempotency_key,
        body,
    )
    .await
}

async fn send(
    state: &AppState,
    context: &AuthContext,
    conversation_id: Uuid,
    idempotency_key: crate::domain::IdempotencyKey,
    mut body: MessageCreate,
) -> AppResult<(StatusCode, Json<Message>)> {
    state.store.require_current(conversation_id).await?;
    resolve_routing(state, context, &mut body).await?;
    let speech =
        prepare_message_speech(state, &context.actor, conversation_id, &mut body, None).await?;
    let prepared = prepare_message_content(state, body)?;
    let message = state
        .store
        .send_message(SendMessageCommand {
            conversation_id,
            sender: context.actor.clone(),
            content: prepared,
            idempotency_key,
            via_app: context.via_app().map(str::to_owned),
            speech,
        })
        .await?;
    Ok((StatusCode::ACCEPTED, Json(message)))
}

/// Records a monotonic device-aware receipt.
pub(super) async fn record_message_receipt(
    State(state): State<AppState>,
    Authenticated(context): Authenticated,
    ApiPath(path): ApiPath<MessagePath>,
    ApiJson(request): ApiJson<ReceiptRequest>,
) -> AppResult<Json<Message>> {
    validate_device_id(&request.device_id)?;
    state
        .store
        .record_receipt(RecordReceiptCommand {
            conversation_id: path.conversation_id,
            message_id: path.message_id,
            recipient: context.actor,
            device_id: request.device_id,
            status: request.status,
        })
        .await
        .map(Json)
}

/// Creates a flat, non-destructive Silicon message bundle.
pub(super) async fn create_message_bundle(
    State(state): State<AppState>,
    Authenticated(context): Authenticated,
    ApiPath(path): ApiPath<ConversationPath>,
    Idempotency(idempotency_key): Idempotency,
    ApiJson(mut bundle): ApiJson<BundleCreate>,
) -> AppResult<(StatusCode, Json<Bundle>)> {
    if context.actor.actor_type != ActorType::Silicon {
        return Err(AppError::forbidden(
            "silicon_only",
            "Only Silicons bundle messages.",
        ));
    }
    bundle.validate().map_err(AppError::validation)?;
    resolve_routing(&state, &context, &mut bundle.display_message).await?;
    let speech = prepare_message_speech(
        &state,
        &context.actor,
        path.conversation_id,
        &mut bundle.display_message,
        None,
    )
    .await?;
    let display_message = prepare_message_content(&state, bundle.display_message)?;
    bundle.display_message = display_message;
    let created = state
        .store
        .create_bundle(CreateBundleCommand {
            conversation_id: path.conversation_id,
            creator: context.actor,
            bundle,
            idempotency_key,
            speech,
        })
        .await?;
    Ok((StatusCode::CREATED, Json(created)))
}

/// Expands a bundle into its display and original messages.
pub(super) async fn get_message_bundle(
    State(state): State<AppState>,
    Authenticated(context): Authenticated,
    ApiPath(path): ApiPath<BundlePath>,
) -> AppResult<Json<BundleDetail>> {
    state
        .store
        .get_bundle(&context.actor, path.conversation_id, path.bundle_id)
        .await
        .map(Json)
}

/// The caller's synchronized draft, or (`?for=si:x`, custodians) its Silicon's.
pub(super) async fn get_draft(
    State(state): State<AppState>,
    Authenticated(context): Authenticated,
    ApiPath(path): ApiPath<ConversationPath>,
    ApiQuery(query): ApiQuery<ForQuery>,
) -> AppResult<Json<Draft>> {
    let owner = match &query.for_account {
        Some(silicon) => looked_after(&state, &context, silicon).await?,
        None => context.actor.clone(),
    };
    state
        .store
        .get_draft(&owner, path.conversation_id)
        .await
        .map(Json)
        .map_err(|error| {
            if matches!(error, AppError::NotFound) {
                AppError::not_found("There is no draft in this conversation.").with_hint(
                    "PUT /api/v1/conversations/{conversation}/draft saves one (dm drafts put); sending the drafted message clears it.",
                )
            } else {
                error
            }
        })
}

/// Creates or conditionally replaces a synchronized draft.
pub(super) async fn put_draft(
    State(state): State<AppState>,
    Authenticated(context): Authenticated,
    access: ConversationAccess,
    ApiPath(path): ApiPath<ConversationPath>,
    IfMatch(expected_version): IfMatch,
    ApiJson(input): ApiJson<DraftInput>,
) -> AppResult<(StatusCode, Json<serde_json::Value>)> {
    require_participant_access(access)?;
    input.validate().map_err(crate::domain::invalid_content)?;
    let outcome = state
        .store
        .put_draft(PutDraftCommand {
            conversation_id: path.conversation_id,
            actor: context.actor,
            expected_version,
            input,
        })
        .await?;
    Ok(match outcome {
        PutDraftOutcome::Saved(draft) => (
            StatusCode::OK,
            Json(serde_json::to_value(draft).map_err(AppError::internal)?),
        ),
        PutDraftOutcome::Conflict(draft) => {
            let mut body = serde_json::to_value(draft).map_err(AppError::internal)?;
            body["error"] = serde_json::json!({
                "code": "draft_conflict",
                "message": "The draft changed since the supplied version. Your save was not applied. Resolve against this draft and retry with its current version.",
                "hint": "This answer carries the current draft: merge your text into it and save again with If-Match set to its version."
            });
            (StatusCode::CONFLICT, Json(body))
        }
    })
}

/// Deletes the caller's synchronized draft idempotently.
pub(super) async fn delete_draft(
    State(state): State<AppState>,
    Authenticated(context): Authenticated,
    access: ConversationAccess,
    ApiPath(path): ApiPath<ConversationPath>,
) -> AppResult<StatusCode> {
    require_participant_access(access)?;
    state
        .store
        .delete_draft(&context.actor, path.conversation_id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

fn require_participant_access(access: ConversationAccess) -> AppResult<()> {
    match access {
        ConversationAccess::Participant => Ok(()),
        ConversationAccess::Custodian => Err(AppError::forbidden(
            "custodian_read_only",
            "A custodian can read its Silicon's drafts (`?for=si:x`) but never write or delete them.",
        )),
        ConversationAccess::Invitee => Err(AppError::NotFound),
    }
}

/// `GET /api/v1/presence/{account}[?conversation=…][&for=si:x]`: availability, last seen and
/// what the account is doing, seen by the caller (or, for a custodian's `for`, by the Silicon
/// it looks after). Activity is about one conversation and shown only to accounts in it:
/// with `conversation`, only activity about that conversation (none when the viewer is not in
/// it); without, activity about no conversation in particular or about one the viewer is in.
pub(super) async fn get_presence(
    State(state): State<AppState>,
    Authenticated(context): Authenticated,
    ApiPath(path): ApiPath<PresencePath>,
    ApiQuery(query): ApiQuery<PresenceQuery>,
) -> AppResult<Json<Presence>> {
    let viewer = match &query.for_account {
        Some(silicon) => looked_after(&state, &context, silicon).await?,
        None => context.actor.clone(),
    };
    let account = state.directory().resolve(path.actor_id.as_str()).await?;
    let target = account.actor();
    if !state.store.may_see_presence(&viewer, &target).await? {
        return Err(AppError::not_found(format!(
            "{} shares its presence only with the accounts it has a conversation with (and a Silicon with its custodian).",
            account.display_id()
        ))
        .with_hint("Presence follows conversations: once you share one, GET /api/v1/presence/{account} answers."));
    }
    let view = match query.conversation.as_deref().map(str::trim) {
        Some(value) => {
            ActivityView::Conversation(viewer_conversation(&state, &context, &viewer, value).await?)
        }
        None => ActivityView::Viewer(&viewer),
    };
    state.store.get_presence(&target, view).await.map(Json)
}

/// The conversation `?conversation=` names, when the viewer is in it; `None` when the viewer
/// may read it but is not in it (a custodian's view without `for`, an invitation): no activity
/// is shown then. One the caller cannot see at all is refused, as everywhere.
async fn viewer_conversation(
    state: &AppState,
    context: &AuthContext,
    viewer: &ActorRef,
    value: &str,
) -> AppResult<Option<Uuid>> {
    const HINT: &str = "Name a conversation you are in by its address or UUID (GET /api/v1/conversations, dm conversations list), or leave conversation out for availability and last seen alone.";
    if value.is_empty() {
        return Err(AppError::invalid(
            "conversation is empty: it names the conversation whose activity you want to see.",
        )
        .with_hint(HINT)
        .with_details(serde_json::json!({"field": "conversation"})));
    }
    let (id, access) = super::access::resolve_conversation_path(state, context, value, false)
        .await
        .map_err(|error| super::sync::named_field(error, "conversation", HINT))?;
    let inside = if *viewer == context.actor {
        access == ConversationAccess::Participant
    } else {
        state.store.conversation_access(viewer, id).await? == Some(ConversationAccess::Participant)
    };
    Ok(inside.then_some(id))
}

/// Returns provider-filtered trending GIFs.
pub(super) async fn list_trending_gifs(
    State(state): State<AppState>,
    Authenticated(_context): Authenticated,
) -> AppResult<Json<GifPage>> {
    state
        .gifs
        .trending()
        .await
        .map(|items| Json(GifPage { items }))
}

/// Searches provider-filtered GIFs.
pub(super) async fn search_gifs(
    State(state): State<AppState>,
    Authenticated(_context): Authenticated,
    ApiQuery(query): ApiQuery<GifSearchQuery>,
) -> AppResult<Json<GifPage>> {
    validate_gif_query(&query.q)?;
    state
        .gifs
        .search(query.q.trim())
        .await
        .map(|items| Json(GifPage { items }))
}

/// Returns the authenticated Carbon's durable GIF MRU.
pub(super) async fn list_recent_gifs(
    State(state): State<AppState>,
    Authenticated(context): Authenticated,
) -> AppResult<Json<GifPage>> {
    state.store.recent_gifs(&context.actor).await.map(Json)
}

/// Unauthenticated process liveness; no dependency check is performed.
pub(super) async fn liveness() -> StatusCode {
    StatusCode::NO_CONTENT
}

/// PostgreSQL-backed readiness check.
pub(super) async fn readiness(State(state): State<AppState>) -> StatusCode {
    match state.store.readiness().await {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(error) => {
            tracing::warn!(error = ?error, "readiness dependency check failed");
            StatusCode::SERVICE_UNAVAILABLE
        }
    }
}

/// Sends to an account, a group or a conversation address without a prior lookup; a
/// direct conversation with an account is created on first contact.
pub(super) async fn send_to_recipient(
    State(state): State<AppState>,
    Authenticated(context): Authenticated,
    Idempotency(idempotency_key): Idempotency,
    ApiJson(mut input): ApiJson<serde_json::Value>,
) -> AppResult<(StatusCode, Json<Message>)> {
    let recipient = input["recipient_id"]
        .as_str()
        .ok_or_else(|| AppError::validation("recipient_id is required"))?
        .to_owned();
    let (conversation_id, access) =
        super::access::resolve_conversation_path(&state, &context, &recipient, true).await?;
    match access {
        ConversationAccess::Participant => {}
        // Invited, not yet a member: the same answer the conversation's own route gives.
        ConversationAccess::Invitee => return Err(super::groups::invitation_pending()),
        ConversationAccess::Custodian => {
            return Err(AppError::forbidden(
                "custodian_read_only",
                "You can read this conversation as a custodian but only its participants send to it.",
            ));
        }
    }
    state
        .store
        .resolve_message_input(conversation_id, &mut input)
        .await?;
    if recipient == conversation_id.to_string() || !super::access::names_one_account(&recipient) {
        input
            .as_object_mut()
            .ok_or_else(|| AppError::validation("message must be an object"))?
            .remove("recipient_id");
    }
    let body: MessageCreate = serde_json::from_value(input).map_err(|error| {
        AppError::invalid(format!("invalid message content: {error}"))
            .with_hint("Check the message against the MessageCreate schema in openapi.yaml (dm docs openapi): text in message, attachment objects in attachments, a voice note in voice (with duration_milliseconds), a GIF in gif.")
    })?;
    send(&state, &context, conversation_id, idempotency_key, body).await
}

/// Resolves the ISI routing addresses of a message to storage form: `sender_id` must
/// address the caller (`isi@si:me`), `recipient_id` a participant (`isi@si:them`).
async fn resolve_routing(
    state: &AppState,
    context: &AuthContext,
    message: &mut MessageCreate,
) -> AppResult<()> {
    if let Some(sender) = message.sender_id.take() {
        let (base, isi) = sender.address_parts().map_err(AppError::validation)?;
        let account = state.directory().resolve(base).await?;
        if account.uuid != context.actor.id.as_str() {
            return Err(AppError::forbidden(
                "sender_mismatch",
                format!(
                    "sender_id {sender} is not you; a message is always sent as the signed-in account (sender_id may only add an ISI, such as deliberate@si:you)."
                ),
            ));
        }
        message.sender_id = Some(storage_address(&account.uuid, isi)?);
    }
    if let Some(recipient) = message.recipient_id.take() {
        let (base, isi) = recipient.address_parts().map_err(AppError::validation)?;
        let account = state.directory().resolve(base).await?;
        if isi.is_some() && !account.is_silicon() {
            return Err(AppError::validation(
                "an ISI address (isi@si:handle) can only name a Silicon",
            ));
        }
        message.recipient_id = Some(storage_address(&account.uuid, isi)?);
    }
    Ok(())
}

fn storage_address(uuid: &str, isi: Option<&str>) -> AppResult<ActorId> {
    isi.map_or_else(|| uuid.to_owned(), |isi| format!("{isi}@{uuid}"))
        .parse()
        .map_err(|_| AppError::validation("invalid routing address"))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CreateConversationRequest {
    participant_ids: Vec<ActorId>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ListConversationsQuery {
    cursor: Option<String>,
    limit: Option<u16>,
    #[serde(rename = "for")]
    for_account: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ForQuery {
    #[serde(rename = "for")]
    pub(super) for_account: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct MessageListQuery {
    cursor: Option<String>,
    limit: Option<u16>,
    #[serde(default)]
    include_bundled_members: bool,
}

#[derive(Debug, Deserialize)]
pub(super) struct ConversationPath {
    pub(super) conversation_id: Uuid,
}

#[derive(Debug, Deserialize)]
pub(super) struct MessagePath {
    pub(super) conversation_id: Uuid,
    pub(super) message_id: Uuid,
}

#[derive(Debug, Deserialize)]
pub(super) struct BundlePath {
    conversation_id: Uuid,
    bundle_id: Uuid,
}

#[derive(Debug, Deserialize)]
pub(super) struct PresencePath {
    actor_id: ActorId,
}

/// Unknown query fields stay ignored here, as before DM 0.17 (the route had no query).
#[derive(Debug, Default, Deserialize)]
pub(super) struct PresenceQuery {
    /// The conversation whose activity to show (an address or UUID).
    conversation: Option<String>,
    #[serde(rename = "for")]
    for_account: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReceiptRequest {
    status: ReceiptStatus,
    device_id: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct GifSearchQuery {
    q: String,
}

fn validate_gif_query(query: &str) -> AppResult<()> {
    if query.trim().is_empty() || query.chars().count() > 50 || query.chars().any(char::is_control)
    {
        return Err(AppError::validation(
            "GIF search query must contain 1 to 50 non-control characters",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{storage_address, validate_gif_query};

    #[test]
    fn gif_query_bounds_reject_control_characters() {
        assert!(validate_gif_query("celebration").is_ok());
        assert!(validate_gif_query("\n").is_err());
    }

    #[test]
    fn routing_addresses_are_stored_with_the_account_uuid() -> crate::AppResult<()> {
        assert_eq!(
            storage_address("K1E", Some("deliberate"))?.as_str(),
            "deliberate@K1E"
        );
        assert_eq!(storage_address("zQo", None)?.as_str(), "zQo");
        Ok(())
    }
}
