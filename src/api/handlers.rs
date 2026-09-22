//! Contracted HTTP request handlers.

use std::collections::{BTreeMap, BTreeSet};

use axum::{Json, extract::State, http::StatusCode};
use serde::Deserialize;
use uuid::Uuid;

use super::extract::{ApiJson, ApiPath, ApiQuery, Authenticated, Idempotency, IfMatch};
use crate::{
    AppError, AppResult,
    application::{
        auth::AuthContext,
        commands::{
            CreateBundleCommand, CreateConversationCommand, PutDraftCommand, PutDraftOutcome,
            RecordReceiptCommand, SendMessageCommand,
        },
        messaging::{prepare_message_content, validate_device_id},
        state::AppState,
    },
    domain::{
        ActorId, ActorRef, ActorType, Bundle, BundleCreate, BundleDetail, Conversation,
        ConversationPage, Draft, DraftInput, GifPage, MAX_CONVERSATION_PARTICIPANTS, Message,
        MessageCreate, MessagePage, PageRequest, Presence, ReceiptStatus,
    },
};

/// Lists conversations visible to the IAM-authenticated actor.
pub(super) async fn list_conversations(
    State(state): State<AppState>,
    Authenticated(context): Authenticated,
    ApiQuery(page): ApiQuery<PageRequest>,
) -> AppResult<Json<ConversationPage>> {
    page.validated_limit()?;
    state
        .store
        .list_conversations_scoped(
            &context.organization_id,
            &context.actor,
            &page,
            &context
                .tag_ids
                .iter()
                .flatten()
                .copied()
                .collect::<Vec<_>>(),
            false,
        )
        .await
        .map(Json)
}

/// Reads the latest version of an accessible conversation message.
pub(super) async fn get_message(
    State(state): State<AppState>,
    Authenticated(context): Authenticated,
    ApiPath(path): ApiPath<MessagePath>,
) -> AppResult<Json<Message>> {
    state
        .store
        .get_message(
            &context.organization_id,
            &context.actor,
            path.conversation_id,
            path.message_id,
        )
        .await
        .map(Json)
}

/// Replaces content and appends the previous content to history.
pub(super) async fn edit_message(
    State(state): State<AppState>,
    Authenticated(context): Authenticated,
    ApiPath(path): ApiPath<MessagePath>,
    Idempotency(key): Idempotency,
    ApiJson(input): ApiJson<MessageCreate>,
) -> AppResult<Json<Message>> {
    state
        .store
        .revise_message(
            &context.organization_id,
            &context.actor,
            path.conversation_id,
            path.message_id,
            &key,
            Some(input),
        )
        .await
        .map(Json)
}

/// Marks the sender's message deleted without adding history.
pub(super) async fn delete_message(
    State(state): State<AppState>,
    Authenticated(context): Authenticated,
    ApiPath(path): ApiPath<MessagePath>,
    Idempotency(key): Idempotency,
) -> AppResult<Json<Message>> {
    state
        .store
        .revise_message(
            &context.organization_id,
            &context.actor,
            path.conversation_id,
            path.message_id,
            &key,
            None,
        )
        .await
        .map(Json)
}

/// Creates or resolves the conversation for an exact participant set.
pub(super) async fn create_conversation(
    State(state): State<AppState>,
    Authenticated(context): Authenticated,
    Idempotency(idempotency_key): Idempotency,
    ApiJson(request): ApiJson<CreateConversationRequest>,
) -> AppResult<(StatusCode, Json<Conversation>)> {
    if request.participant_ids.is_empty() {
        return Err(AppError::validation(
            "participant_ids must contain at least one actor",
        ));
    }

    let mut actor_ids = request.participant_ids.into_iter().collect::<BTreeSet<_>>();
    actor_ids.insert(context.actor.id.clone());
    if actor_ids.len() < 2 {
        return Err(AppError::validation(
            "a conversation requires at least two unique participants",
        ));
    }
    if actor_ids.len() > MAX_CONVERSATION_PARTICIPANTS {
        return Err(AppError::validation(
            "a conversation may contain at most 100 unique participants",
        ));
    }
    let actor_ids = actor_ids.into_iter().collect::<Vec<_>>();
    let participants = state
        .identity
        .authorize_participants(&context, &actor_ids)
        .await?;
    let participants = verify_resolved_actors(&actor_ids, participants)?;
    if !participants.contains(&context.actor) {
        return Err(iam_contract_error());
    }

    let conversation = state
        .store
        .create_conversation(CreateConversationCommand {
            organization_id: context.organization_id,
            creator: context.actor,
            participants,
            idempotency_key,
        })
        .await?;
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
            &context.organization_id,
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
    Authenticated(authority): Authenticated,
    ApiPath(path): ApiPath<ConversationPath>,
    Idempotency(idempotency_key): Idempotency,
    ApiJson(content): ApiJson<MessageCreate>,
) -> AppResult<(StatusCode, Json<Message>)> {
    let sender = resolve_sender(&state, &authority, content.sender_id.as_ref()).await?;
    state
        .store
        .require_participant(&authority.organization_id, &sender, path.conversation_id)
        .await?;
    let content = prepare_message_content(&state, content)?;
    let message = state
        .store
        .send_message_as(
            SendMessageCommand {
                organization_id: authority.organization_id,
                conversation_id: path.conversation_id,
                sender,
                content,
                idempotency_key,
            },
            &authority.actor,
        )
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
            organization_id: context.organization_id,
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
        return Err(AppError::Forbidden);
    }
    bundle.validate().map_err(AppError::validation)?;
    if bundle
        .display_message
        .sender_id
        .as_ref()
        .is_some_and(|sender_id| !sender_id.addresses(&context.actor))
    {
        return Err(AppError::Forbidden);
    }
    state
        .store
        .require_participant(
            &context.organization_id,
            &context.actor,
            path.conversation_id,
        )
        .await?;
    let display_message = prepare_message_content(&state, bundle.display_message)?;
    bundle.display_message = display_message;
    let created = state
        .store
        .create_bundle(CreateBundleCommand {
            organization_id: context.organization_id,
            conversation_id: path.conversation_id,
            creator: context.actor,
            bundle,
            idempotency_key,
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
        .get_bundle(
            &context.organization_id,
            &context.actor,
            path.conversation_id,
            path.bundle_id,
        )
        .await
        .map(Json)
}

/// Returns the authenticated actor's synchronized draft.
pub(super) async fn get_draft(
    State(state): State<AppState>,
    Authenticated(context): Authenticated,
    ApiPath(path): ApiPath<ConversationPath>,
) -> AppResult<Json<Draft>> {
    state
        .store
        .get_draft(
            &context.organization_id,
            &context.actor,
            path.conversation_id,
        )
        .await
        .map(Json)
}

/// Creates or conditionally replaces a synchronized draft.
pub(super) async fn put_draft(
    State(state): State<AppState>,
    Authenticated(context): Authenticated,
    ApiPath(path): ApiPath<ConversationPath>,
    IfMatch(expected_version): IfMatch,
    ApiJson(input): ApiJson<DraftInput>,
) -> AppResult<(StatusCode, Json<serde_json::Value>)> {
    state
        .store
        .require_participant(
            &context.organization_id,
            &context.actor,
            path.conversation_id,
        )
        .await?;
    input.validate().map_err(AppError::validation)?;

    let outcome = state
        .store
        .put_draft(PutDraftCommand {
            organization_id: context.organization_id,
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
                "message": "The draft changed since the supplied version. Your save was not applied. Resolve against this draft and retry with its current version."
            });
            (StatusCode::CONFLICT, Json(body))
        }
    })
}

/// Deletes the authenticated actor's synchronized draft idempotently.
pub(super) async fn delete_draft(
    State(state): State<AppState>,
    Authenticated(context): Authenticated,
    ApiPath(path): ApiPath<ConversationPath>,
) -> AppResult<StatusCode> {
    state
        .store
        .delete_draft(
            &context.organization_id,
            &context.actor,
            path.conversation_id,
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Returns IAM-authorized online and activity state for an actor.
pub(super) async fn get_presence(
    State(state): State<AppState>,
    Authenticated(context): Authenticated,
    ApiPath(path): ApiPath<PresencePath>,
) -> AppResult<Json<Presence>> {
    let target = state
        .identity
        .authorize_presence(&context, &path.actor_id)
        .await?;
    state
        .store
        .refresh_directory(&context.organization_id, &[context.actor.clone(), target])
        .await?;
    state
        .store
        .get_presence(&context.organization_id, &context.actor, &path.actor_id)
        .await
        .map(Json)
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
    state
        .store
        .recent_gifs(&context.organization_id, &context.actor)
        .await
        .map(Json)
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CreateConversationRequest {
    participant_ids: Vec<ActorId>,
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
    conversation_id: Uuid,
}

#[derive(Debug, Deserialize)]
pub(super) struct MessagePath {
    conversation_id: Uuid,
    message_id: Uuid,
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

async fn resolve_sender(
    state: &AppState,
    context: &AuthContext,
    requested_id: Option<&ActorId>,
) -> AppResult<ActorRef> {
    let Some(requested_id) = requested_id else {
        return Ok(context.actor.clone());
    };
    let base_id = requested_id.base_actor_id().map_err(AppError::validation)?;
    let requested_id = &base_id;
    if *requested_id == context.actor.id {
        return Ok(context.actor.clone());
    }
    if !context.may_represent(requested_id) {
        return Err(AppError::Forbidden);
    }
    let requested = [requested_id.clone()];
    let mut resolved = state
        .identity
        .authorize_participants(context, &requested)
        .await?;
    if resolved.len() != 1 {
        return Err(iam_contract_error());
    }
    let actor = resolved.pop().ok_or_else(iam_contract_error)?;
    if actor.id != *requested_id {
        return Err(iam_contract_error());
    }
    Ok(actor)
}

pub(crate) fn verify_resolved_actors(
    requested: &[ActorId],
    resolved: Vec<ActorRef>,
) -> AppResult<Vec<ActorRef>> {
    let mut by_id = BTreeMap::new();
    for actor in resolved {
        if by_id.insert(actor.id.clone(), actor).is_some() {
            return Err(iam_contract_error());
        }
    }
    let requested = requested.iter().collect::<BTreeSet<_>>();
    let returned = by_id.keys().collect::<BTreeSet<_>>();
    if requested != returned {
        return Err(iam_contract_error());
    }
    Ok(by_id.into_values().collect())
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

fn iam_contract_error() -> AppError {
    AppError::DependencyUnavailable { dependency: "iam" }
}

/// Sends to an account or group without a client-side conversation lookup.
pub(super) async fn send_to_recipient(
    State(state): State<AppState>,
    Authenticated(authority): Authenticated,
    Idempotency(idempotency_key): Idempotency,
    ApiJson(mut input): ApiJson<serde_json::Value>,
) -> AppResult<(StatusCode, Json<Message>)> {
    let recipient = input["recipient_id"]
        .as_str()
        .ok_or_else(|| AppError::validation("recipient_id is required"))?
        .to_owned();
    let conversation_id = state
        .store
        .resolve_destination(&authority, &recipient, true, state.identity.as_ref())
        .await?;
    state
        .store
        .check_group_access(&authority, conversation_id)
        .await?;
    state
        .store
        .resolve_message_input(&authority.organization_id, conversation_id, &mut input)
        .await?;
    if recipient.starts_with("g:")
        || recipient.contains("::")
        || Uuid::parse_str(&recipient).is_ok()
    {
        input
            .as_object_mut()
            .ok_or_else(|| AppError::validation("message must be an object"))?
            .remove("recipient_id");
    }
    let content: MessageCreate = serde_json::from_value(input)
        .map_err(|_| AppError::validation("invalid message content"))?;
    let sender = resolve_sender(&state, &authority, content.sender_id.as_ref()).await?;
    state
        .store
        .require_participant(&authority.organization_id, &sender, conversation_id)
        .await?;
    let content = prepare_message_content(&state, content)?;
    let message = state
        .store
        .send_message_as(
            SendMessageCommand {
                organization_id: authority.organization_id,
                conversation_id,
                sender,
                content,
                idempotency_key,
            },
            &authority.actor,
        )
        .await?;
    Ok((StatusCode::ACCEPTED, Json(message)))
}

#[cfg(test)]
mod tests {
    use std::str::FromStr as _;

    use super::{validate_gif_query, verify_resolved_actors};
    use crate::domain::{ActorId, ActorRef, ActorType};

    #[test]
    fn iam_actor_resolution_must_exactly_match_requested_set()
    -> Result<(), Box<dyn std::error::Error>> {
        let carbon_id = ActorId::from_str("carbon-1")?;
        let silicon_id = ActorId::from_str("silicon-1")?;
        let carbon = ActorRef {
            actor_type: ActorType::Carbon,
            id: carbon_id.clone(),
        };
        let silicon = ActorRef {
            actor_type: ActorType::Silicon,
            id: silicon_id.clone(),
        };

        let exact = verify_resolved_actors(std::slice::from_ref(&carbon_id), vec![carbon.clone()])?;
        assert_eq!(exact, vec![carbon.clone()]);

        let missing =
            verify_resolved_actors(&[carbon_id.clone(), silicon_id], vec![carbon.clone()]);
        assert!(missing.is_err());

        let extra = verify_resolved_actors(
            std::slice::from_ref(&carbon_id),
            vec![carbon.clone(), silicon],
        );
        assert!(extra.is_err());

        let duplicate = verify_resolved_actors(
            std::slice::from_ref(&carbon_id),
            vec![carbon.clone(), carbon],
        );
        assert!(duplicate.is_err());
        Ok(())
    }

    #[test]
    fn gif_query_bounds_reject_control_characters() {
        assert!(validate_gif_query("celebration").is_ok());
        assert!(validate_gif_query("\n").is_err());
    }
}
