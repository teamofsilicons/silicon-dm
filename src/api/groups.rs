//! Named-group HTTP operations. All message operations reuse the conversation UUID.
use super::{
    extract::{ApiJson, ApiPath, ApiQuery, Authenticated, Idempotency, IfMatch},
    handlers::verify_resolved_actors,
};
use crate::{
    AppError, AppResult,
    application::{auth::AuthContext, state::AppState},
    domain::{
        ActorId, ActorRef, Conversation, ConversationPage, GroupCreate, GroupDetails,
        GroupSettings, PageRequest,
    },
    infrastructure::postgres::groups::require_group_admin,
};
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
};
use serde::Deserialize;
use uuid::Uuid;

#[derive(Deserialize)]
pub(super) struct GroupPath {
    group_id: Uuid,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Members {
    member_ids: Vec<ActorId>,
}

pub(super) async fn list(
    State(state): State<AppState>,
    Authenticated(auth): Authenticated,
    ApiQuery(page): ApiQuery<PageRequest>,
) -> AppResult<Json<ConversationPage>> {
    state
        .store
        .list_conversations_scoped(
            &auth.organization_id,
            &auth.actor,
            &page,
            &auth.tag_ids.iter().flatten().copied().collect::<Vec<_>>(),
            true,
        )
        .await
        .map(Json)
}
pub(super) async fn get(
    State(state): State<AppState>,
    Authenticated(auth): Authenticated,
    ApiPath(path): ApiPath<GroupPath>,
) -> AppResult<Json<Conversation>> {
    state.store.check_group_access(&auth, path.group_id).await?;
    let conversation = state
        .store
        .get_conversation(&auth.organization_id, &auth.actor, path.group_id)
        .await?;
    if conversation.group.is_none() {
        return Err(AppError::NotFound);
    }
    Ok(Json(conversation))
}
pub(super) async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Authenticated(auth): Authenticated,
    Idempotency(key): Idempotency,
    ApiJson(input): ApiJson<GroupCreate>,
) -> AppResult<(StatusCode, Json<Conversation>)> {
    require_group_admin(&auth)?;
    let members = resolve(&state, &auth, &input.member_ids).await?;
    let conversation = state
        .store
        .create_group(&auth, input.settings, members, &key)
        .await?;
    event(
        &state,
        &headers,
        "group.created",
        conversation.id,
        conversation.group.as_ref().map(|g| &g.settings),
        input.member_ids.len(),
    );
    Ok((StatusCode::CREATED, Json(conversation)))
}
pub(super) async fn update(
    State(state): State<AppState>,
    headers: HeaderMap,
    Authenticated(auth): Authenticated,
    ApiPath(path): ApiPath<GroupPath>,
    IfMatch(version): IfMatch,
    Idempotency(key): Idempotency,
    ApiJson(settings): ApiJson<GroupSettings>,
) -> AppResult<Json<GroupDetails>> {
    let version = version
        .ok_or_else(|| AppError::validation("If-Match is required when updating a group"))?;
    let group = state
        .store
        .update_group(&auth, path.group_id, settings, version, &key)
        .await?;
    event(
        &state,
        &headers,
        "group.updated",
        path.group_id,
        Some(&group.settings),
        group.invited_members.len(),
    );
    Ok(Json(group))
}
pub(super) async fn invite(
    State(state): State<AppState>,
    headers: HeaderMap,
    Authenticated(auth): Authenticated,
    ApiPath(path): ApiPath<GroupPath>,
    Idempotency(key): Idempotency,
    ApiJson(input): ApiJson<Members>,
) -> AppResult<Json<GroupDetails>> {
    require_group_admin(&auth)?;
    let members = resolve(&state, &auth, &input.member_ids).await?;
    let group = state
        .store
        .change_group_members(&auth, path.group_id, members, false, &key)
        .await?;
    event(
        &state,
        &headers,
        "group.members_invited",
        path.group_id,
        None,
        input.member_ids.len(),
    );
    Ok(Json(group))
}
pub(super) async fn remove(
    State(state): State<AppState>,
    headers: HeaderMap,
    Authenticated(auth): Authenticated,
    ApiPath(path): ApiPath<GroupPath>,
    Idempotency(key): Idempotency,
    ApiJson(input): ApiJson<Members>,
) -> AppResult<Json<GroupDetails>> {
    require_group_admin(&auth)?;
    validate_members(&input.member_ids)?;
    let ids = input
        .member_ids
        .iter()
        .map(|id| {
            id.base_actor_id()
                .map(|id| id.to_string())
                .map_err(AppError::validation)
        })
        .collect::<AppResult<Vec<_>>>()?;
    // Snapshot identities survive departures and invitation deletion, keeping retries stable.
    let rows: Vec<(crate::domain::ActorType, String)> = sqlx::query_as("SELECT actor_kind,actor_id FROM actor_snapshots WHERE organization_id=$1 AND actor_id=ANY($2)")
        .bind(auth.organization_id.as_str()).bind(&ids).fetch_all(state.store.pool()).await?;
    let members = rows
        .into_iter()
        .map(|(actor_type, id)| {
            Ok(ActorRef {
                actor_type,
                id: id.parse().map_err(|_| AppError::NotFound)?,
            })
        })
        .collect::<AppResult<Vec<_>>>()?;
    let members = verify_resolved_actors(&input.member_ids, members)?;
    let group = state
        .store
        .change_group_members(&auth, path.group_id, members, true, &key)
        .await?;
    event(
        &state,
        &headers,
        "group.invitations_removed",
        path.group_id,
        None,
        input.member_ids.len(),
    );
    Ok(Json(group))
}
fn validate_members(ids: &[ActorId]) -> AppResult<()> {
    if ids.len() > 100 {
        return Err(AppError::validation(
            "at most 100 explicit invitations per request",
        ));
    }
    Ok(())
}
async fn resolve(
    state: &AppState,
    auth: &AuthContext,
    ids: &[ActorId],
) -> AppResult<Vec<ActorRef>> {
    validate_members(ids)?;
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    verify_resolved_actors(ids, state.identity.authorize_participants(auth, ids).await?)
}
fn event(
    state: &AppState,
    headers: &HeaderMap,
    name: &str,
    id: Uuid,
    settings: Option<&GroupSettings>,
    count: usize,
) {
    if crate::telemetry::requested(headers) {
        crate::telemetry::record(
            state,
            "backend",
            name,
            serde_json::json!({"group_id":id,"is_public":settings.map(|s|s.is_public),"count":count,"success":true}),
        );
    }
}
