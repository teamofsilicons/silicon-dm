//! Contracted HTTP and WebSocket-upgrade request handlers.

use std::{
    collections::{BTreeMap, BTreeSet},
    str::FromStr as _,
};

use axum::{
    Json,
    extract::{RawQuery, State, WebSocketUpgrade},
    http::{HeaderMap, StatusCode},
    response::Response,
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use url::{Url, form_urlencoded};
use uuid::Uuid;

use super::extract::{
    ApiJson, ApiPath, ApiQuery, Authenticated, AuthenticatedService, Idempotency, IfMatch,
    realtime_bearer,
};
use crate::{
    AppError, AppResult,
    application::{
        auth::AuthContext,
        commands::{
            AcceptSystemEventCommand, CreateBundleCommand, CreateConversationCommand,
            PutDraftCommand, PutDraftOutcome, RecordReceiptCommand, SendMessageCommand,
        },
        messaging::{
            prepare_message_content, validate_briefcase_permanent_url, validate_device_id,
            validate_draft_urls,
        },
        ports::{AuthenticationRequest, DelegationRequest},
        state::AppState,
    },
    domain::{
        ActorId, ActorRef, ActorType, Bundle, BundleCreate, BundleDetail, Conversation,
        ConversationPage, Draft, DraftInput, GifPage, MAX_CONVERSATION_PARTICIPANTS, Message,
        MessageCreate, MessagePage, OrganizationId, PageRequest, Presence, ReceiptStatus,
        SystemEvent,
    },
    realtime::serve_socket,
};

const HOOK_DELIVERY_CAPABILITY: &str = "dm.hook_events.deliver";

/// Authenticates and upgrades a durable realtime client connection.
pub(super) async fn open_realtime_connection(
    State(state): State<AppState>,
    headers: HeaderMap,
    RawQuery(raw_query): RawQuery,
    upgrade: WebSocketUpgrade,
) -> AppResult<Response> {
    let query = parse_realtime_query(raw_query.as_deref())?;
    let token = realtime_bearer(&headers)?;
    let authority = state
        .identity
        .authenticate(AuthenticationRequest::Bearer {
            token: &token,
            organization_id: &query.organization_id,
        })
        .await?;
    if authority.organization_id != query.organization_id {
        return Err(AppError::Forbidden);
    }
    if query
        .actor_ids
        .iter()
        .any(|actor_id| !authority.may_represent(actor_id))
    {
        return Err(AppError::Forbidden);
    }
    let actors = state
        .identity
        .authorize_participants(&authority, &query.actor_ids)
        .await?;
    let actors = verify_resolved_actors(&query.actor_ids, actors)?;
    let max_message_size = state.settings.server.max_body_bytes;
    Ok(upgrade
        .max_message_size(max_message_size)
        .max_frame_size(max_message_size)
        .on_upgrade(move |socket| serve_socket(socket, state, authority, actors, query.device_id)))
}

/// Lists conversations visible to the IAM-authenticated actor.
pub(super) async fn list_conversations(
    State(state): State<AppState>,
    Authenticated(context): Authenticated,
    ApiQuery(page): ApiQuery<PageRequest>,
) -> AppResult<Json<ConversationPage>> {
    page.validated_limit()?;
    state
        .store
        .list_conversations(&context.organization_id, &context.actor, &page)
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

/// Transcribes (when needed), persists, and durably queues a message.
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
    let (content, voice_duration_milliseconds) =
        prepare_message_content(&state, &authority, content, idempotency_key.as_str()).await?;
    let message = state
        .store
        .send_message(SendMessageCommand {
            organization_id: authority.organization_id,
            conversation_id: path.conversation_id,
            sender,
            content,
            voice_duration_milliseconds,
            idempotency_key,
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
        .is_some_and(|sender_id| *sender_id != context.actor.id)
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
    let (display_message, voice_duration_milliseconds) = prepare_message_content(
        &state,
        &context,
        bundle.display_message,
        idempotency_key.as_str(),
    )
    .await?;
    bundle.display_message = display_message;
    let created = state
        .store
        .create_bundle(CreateBundleCommand {
            organization_id: context.organization_id,
            conversation_id: path.conversation_id,
            creator: context.actor,
            bundle,
            voice_duration_milliseconds,
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
) -> AppResult<(StatusCode, Json<Draft>)> {
    state
        .store
        .require_participant(
            &context.organization_id,
            &context.actor,
            path.conversation_id,
        )
        .await?;
    input.validate().map_err(AppError::validation)?;
    validate_draft_urls(&input, &state.settings.providers.briefcase_base_url)?;

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
        PutDraftOutcome::Saved(draft) => (StatusCode::OK, Json(draft)),
        PutDraftOutcome::Conflict(draft) => (StatusCode::CONFLICT, Json(draft)),
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

/// Exchanges a permanent Briefcase entry URL for a temporary CDN URL.
pub(super) async fn create_attachment_temporary_url(
    State(state): State<AppState>,
    Authenticated(context): Authenticated,
    ApiJson(request): ApiJson<TemporaryUrlRequest>,
) -> AppResult<(StatusCode, Json<TemporaryUrlResponse>)> {
    let entry_id = validate_briefcase_permanent_url(
        &request.permanent_url,
        &state.settings.providers.briefcase_base_url,
    )?;
    let credential = state
        .identity
        .exchange_actor_credential(
            &context,
            &DelegationRequest::briefcase_temporary_url(
                &state.settings.providers.briefcase_iam_audience,
                entry_id,
            ),
        )
        .await?;
    let result = state
        .attachments
        .temporary_url(
            &request.permanent_url,
            &context.organization_id,
            &credential,
        )
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(TemporaryUrlResponse {
            url: result.url,
            expires_at: result.expires_at,
        }),
    ))
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

/// Validates and durably accepts an event from the Silicon Hook service.
pub(super) async fn deliver_hook_event(
    State(state): State<AppState>,
    AuthenticatedService(service): AuthenticatedService,
    ApiJson(event): ApiJson<SystemEvent>,
) -> AppResult<StatusCode> {
    if !service.capabilities.contains(HOOK_DELIVERY_CAPABILITY) {
        return Err(AppError::Forbidden);
    }
    validate_hook_event(&event)?;
    let target = state
        .identity
        .authorize_hook_target(&service, &event.org_id, &event.silicon_id)
        .await?;
    state
        .store
        .accept_system_event(AcceptSystemEventCommand { target, event })
        .await?;
    Ok(StatusCode::ACCEPTED)
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

#[derive(Debug, Eq, PartialEq)]
struct RealtimeConnectQuery {
    organization_id: OrganizationId,
    actor_ids: Vec<ActorId>,
    device_id: String,
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
#[serde(deny_unknown_fields)]
pub(super) struct TemporaryUrlRequest {
    permanent_url: Url,
}

#[derive(Debug, Serialize)]
pub(super) struct TemporaryUrlResponse {
    url: Url,
    #[serde(with = "time::serde::rfc3339")]
    expires_at: OffsetDateTime,
}

#[derive(Debug, Deserialize)]
pub(super) struct GifSearchQuery {
    q: String,
}

fn parse_realtime_query(raw_query: Option<&str>) -> AppResult<RealtimeConnectQuery> {
    let raw_query = raw_query
        .filter(|query| !query.is_empty())
        .ok_or_else(|| AppError::validation("realtime connection query is required"))?;
    let mut organization_id = None;
    let mut actor_ids = Vec::new();
    let mut device_id = None;

    for (name, value) in form_urlencoded::parse(raw_query.as_bytes()) {
        if name.contains('\u{fffd}') || value.contains('\u{fffd}') {
            return Err(AppError::validation(
                "realtime query contains invalid UTF-8",
            ));
        }
        match name.as_ref() {
            "org_id" => {
                if organization_id.is_some() {
                    return Err(AppError::validation("org_id must be supplied exactly once"));
                }
                organization_id = Some(
                    OrganizationId::from_str(&value)
                        .map_err(|_| AppError::validation("org_id is invalid"))?,
                );
            }
            "actors" => {
                if actor_ids.len() == 100 {
                    return Err(AppError::validation(
                        "actors must contain between 1 and 100 unique actor IDs",
                    ));
                }
                actor_ids
                    .push(ActorId::from_str(&value).map_err(|_| {
                        AppError::validation("actors contains an invalid actor ID")
                    })?);
            }
            "device_id" => {
                if device_id.is_some() {
                    return Err(AppError::validation(
                        "device_id must be supplied exactly once",
                    ));
                }
                device_id = Some(value.into_owned());
            }
            _ => {
                return Err(AppError::validation(
                    "realtime query contains an unknown parameter",
                ));
            }
        }
    }

    if actor_ids.is_empty() {
        return Err(AppError::validation(
            "actors must contain between 1 and 100 unique actor IDs",
        ));
    }
    let unique_actors = actor_ids.iter().collect::<BTreeSet<_>>();
    if unique_actors.len() != actor_ids.len() {
        return Err(AppError::validation("actors must contain unique actor IDs"));
    }
    let device_id =
        device_id.ok_or_else(|| AppError::validation("device_id must be supplied exactly once"))?;
    validate_device_id(&device_id)?;
    Ok(RealtimeConnectQuery {
        organization_id: organization_id
            .ok_or_else(|| AppError::validation("org_id must be supplied exactly once"))?,
        actor_ids,
        device_id,
    })
}

async fn resolve_sender(
    state: &AppState,
    context: &AuthContext,
    requested_id: Option<&ActorId>,
) -> AppResult<ActorRef> {
    let Some(requested_id) = requested_id else {
        return Ok(context.actor.clone());
    };
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

fn verify_resolved_actors(
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

fn validate_hook_event(event: &SystemEvent) -> AppResult<()> {
    event.validate().map_err(AppError::validation)?;
    if event.trace_id.as_ref().is_some_and(|trace_id| {
        trace_id.is_empty() || trace_id.len() > 255 || trace_id.chars().any(char::is_control)
    }) {
        return Err(AppError::validation(
            "trace_id must contain 1 to 255 non-control characters",
        ));
    }
    Ok(())
}

fn iam_contract_error() -> AppError {
    AppError::DependencyUnavailable { dependency: "iam" }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr as _;

    use super::{
        parse_realtime_query, validate_gif_query, validate_hook_event, verify_resolved_actors,
    };
    use crate::domain::{ActorId, ActorRef, ActorType, OrganizationId, SystemEvent};

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

    #[test]
    fn hook_trace_identifier_is_bounded() -> Result<(), Box<dyn std::error::Error>> {
        let event = SystemEvent {
            event_id: uuid::Uuid::now_v7(),
            org_id: OrganizationId::from_str("org-1")?,
            silicon_id: ActorId::from_str("silicon-1")?,
            event_type: "calendar.updated.v1".to_owned(),
            trace_id: Some("bad\ntrace".to_owned()),
            payload: serde_json::Map::new(),
        };
        assert!(validate_hook_event(&event).is_err());
        Ok(())
    }

    #[test]
    fn realtime_query_accepts_repeated_unique_actor_parameters()
    -> Result<(), Box<dyn std::error::Error>> {
        let query = parse_realtime_query(Some(
            "org_id=org-1&actors=carbon-1&actors=silicon-1&device_id=device-1",
        ))?;
        assert_eq!(query.organization_id.as_str(), "org-1");
        assert_eq!(query.actor_ids.len(), 2);
        assert_eq!(query.device_id, "device-1");
        Ok(())
    }

    #[test]
    fn realtime_query_rejects_duplicate_and_excess_actor_parameters() {
        assert!(
            parse_realtime_query(Some(
                "org_id=org-1&actors=carbon-1&actors=carbon-1&device_id=device-1"
            ))
            .is_err()
        );

        let actors = (0..101)
            .map(|index| format!("actors=actor-{index}"))
            .collect::<Vec<_>>()
            .join("&");
        let query = format!("org_id=org-1&{actors}&device_id=device-1");
        assert!(parse_realtime_query(Some(&query)).is_err());
    }

    #[test]
    fn realtime_query_accepts_exactly_one_hundred_actors() -> Result<(), Box<dyn std::error::Error>>
    {
        let actors = (0..100)
            .map(|index| format!("actors=actor-{index}"))
            .collect::<Vec<_>>()
            .join("&");
        let query = format!("org_id=org-1&{actors}&device_id=device-1");
        assert_eq!(parse_realtime_query(Some(&query))?.actor_ids.len(), 100);
        Ok(())
    }

    #[test]
    fn realtime_query_requires_single_valid_context_fields() {
        for query in [
            "actors=carbon-1&device_id=device-1",
            "org_id=org-1&org_id=org-2&actors=carbon-1&device_id=device-1",
            "org_id=org-1&actors=carbon-1",
            "org_id=org-1&actors=carbon-1&device_id=one&device_id=two",
            "org_id=org-1&actors=carbon-1&device_id=%0A",
            "org_id=org-1&actors=carbon-1&device_id=device-1&unknown=value",
        ] {
            assert!(
                parse_realtime_query(Some(query)).is_err(),
                "accepted {query}"
            );
        }
    }
}
