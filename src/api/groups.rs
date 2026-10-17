//! Groups over HTTP. Owners are co-equal (edit settings, invite, remove, promote, cancel
//! invitations); invitations must be accepted; members leave; the last owner cannot.
//! A Silicon's custodian manages the groups its Silicon owns and may take its Silicon out
//! of any group, but never accepts, sends or speaks as it.
use super::extract::{
    ApiJson, ApiPath, ApiQuery, Authenticated, ConversationAccess, Idempotency, IfMatch,
};
use crate::{
    AppError, AppResult,
    application::{auth::AuthContext, state::AppState},
    domain::{Account, ActorId, ActorRef, Conversation, GroupCreate, GroupDetails, GroupSettings},
    infrastructure::postgres::groups::{Invitation, Invitee},
};
use axum::{Json, extract::State, http::StatusCode};
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
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ForQuery {
    #[serde(rename = "for")]
    for_account: Option<String>,
}

const MAX_MEMBERS_PER_REQUEST: usize = 100;

pub(super) async fn get(
    State(state): State<AppState>,
    Authenticated(auth): Authenticated,
    access: ConversationAccess,
    ApiPath(path): ApiPath<GroupPath>,
) -> AppResult<Json<Conversation>> {
    if access == ConversationAccess::Invitee {
        return Err(AppError::refused(
            StatusCode::FORBIDDEN,
            "invitation_pending",
            "You are invited to this group but have not accepted yet: see GET /api/v1/invitations, then POST /api/v1/groups/{group}/accept.",
        ));
    }
    Ok(Json(
        state
            .store
            .get_conversation(&auth.actor, path.group_id)
            .await?,
    ))
}

pub(super) async fn create(
    State(state): State<AppState>,
    Authenticated(auth): Authenticated,
    Idempotency(key): Idempotency,
    ApiJson(input): ApiJson<GroupCreate>,
) -> AppResult<(StatusCode, Json<Conversation>)> {
    auth.require_own_session("Creating a group")?;
    let invitees = invitees(&state, &auth, &input.member_ids).await?;
    let conversation = state
        .store
        .create_group(&auth.actor, input.settings, invitees, &key)
        .await?;
    Ok((StatusCode::CREATED, Json(conversation)))
}

pub(super) async fn update(
    State(state): State<AppState>,
    Authenticated(auth): Authenticated,
    ApiPath(path): ApiPath<GroupPath>,
    IfMatch(version): IfMatch,
    Idempotency(key): Idempotency,
    ApiJson(settings): ApiJson<GroupSettings>,
) -> AppResult<Json<GroupDetails>> {
    require_manager(&state, &auth, path.group_id).await?;
    let version = version.ok_or_else(|| {
        AppError::invalid(
            "Changing a group needs an If-Match header with the version you read, so a change someone made since is not overwritten.",
        )
        .with_hint("Read the group (GET /api/v1/groups/{group}, dm groups show), then send If-Match: its version.")
    })?;
    let group = state
        .store
        .update_group(&auth.actor, path.group_id, settings, version, &key)
        .await?;
    Ok(Json(group))
}

/// `POST /groups/{group}/members`: owners invite accounts.
pub(super) async fn invite(
    State(state): State<AppState>,
    Authenticated(auth): Authenticated,
    ApiPath(path): ApiPath<GroupPath>,
    Idempotency(key): Idempotency,
    ApiJson(input): ApiJson<Members>,
) -> AppResult<Json<GroupDetails>> {
    require_manager(&state, &auth, path.group_id).await?;
    let invitees = invitees(&state, &auth, &input.member_ids).await?;
    let group = state
        .store
        .invite_to_group(&auth.actor, path.group_id, invitees, &key)
        .await?;
    Ok(Json(group))
}

/// `DELETE /groups/{group}/members`: owners remove members or cancel invitations; a
/// custodian may take its own Silicons out.
pub(super) async fn remove(
    State(state): State<AppState>,
    Authenticated(auth): Authenticated,
    ApiPath(path): ApiPath<GroupPath>,
    Idempotency(key): Idempotency,
    ApiJson(input): ApiJson<Members>,
) -> AppResult<Json<GroupDetails>> {
    let members = resolve(&state, &input.member_ids).await?;
    if !state
        .store
        .manages_group(&auth.actor, path.group_id)
        .await?
        && !members
            .iter()
            .all(|member| member.custodian.as_deref() == Some(auth.actor.id.as_str()))
    {
        return Err(not_manager());
    }
    let refs: Vec<ActorRef> = members.iter().map(Account::actor).collect();
    let group = state
        .store
        .remove_from_group(&auth.actor, path.group_id, &refs, &key)
        .await?;
    Ok(Json(group))
}

/// `POST /groups/{group}/owners` / `DELETE …/owners`: promote members, demote owners.
pub(super) async fn add_owners(
    state: State<AppState>,
    auth: Authenticated,
    path: ApiPath<GroupPath>,
    key: Idempotency,
    input: ApiJson<Members>,
) -> AppResult<Json<GroupDetails>> {
    owners(state, auth, path, key, input, true).await
}

pub(super) async fn remove_owners(
    state: State<AppState>,
    auth: Authenticated,
    path: ApiPath<GroupPath>,
    key: Idempotency,
    input: ApiJson<Members>,
) -> AppResult<Json<GroupDetails>> {
    owners(state, auth, path, key, input, false).await
}

async fn owners(
    State(state): State<AppState>,
    Authenticated(auth): Authenticated,
    ApiPath(path): ApiPath<GroupPath>,
    Idempotency(key): Idempotency,
    ApiJson(input): ApiJson<Members>,
    promote: bool,
) -> AppResult<Json<GroupDetails>> {
    require_manager(&state, &auth, path.group_id).await?;
    let members = resolve(&state, &input.member_ids).await?;
    let refs: Vec<ActorRef> = members.iter().map(Account::actor).collect();
    state
        .store
        .set_group_owners(&auth.actor, path.group_id, &refs, promote, &key)
        .await
        .map(Json)
}

/// `POST /groups/{group}/accept`: the invitee joins (and sees the full history).
pub(super) async fn accept(
    State(state): State<AppState>,
    Authenticated(auth): Authenticated,
    ApiPath(path): ApiPath<GroupPath>,
) -> AppResult<Json<Conversation>> {
    auth.require_own_session("Accepting an invitation")?;
    state
        .store
        .respond_to_invitation(&auth.actor, path.group_id, true)
        .await?;
    Ok(Json(
        state
            .store
            .get_conversation(&auth.actor, path.group_id)
            .await?,
    ))
}

/// `POST /groups/{group}/decline[?for=si:x]`: the invitee (or its custodian) declines.
pub(super) async fn decline(
    State(state): State<AppState>,
    Authenticated(auth): Authenticated,
    ApiPath(path): ApiPath<GroupPath>,
    ApiQuery(query): ApiQuery<ForQuery>,
) -> AppResult<StatusCode> {
    let invitee = match &query.for_account {
        Some(silicon) => super::handlers::looked_after(&state, &auth, silicon).await?,
        None => auth.actor.clone(),
    };
    state
        .store
        .respond_to_invitation(&invitee, path.group_id, false)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /groups/{group}/leave`: a member leaves (the last owner cannot).
pub(super) async fn leave(
    State(state): State<AppState>,
    Authenticated(auth): Authenticated,
    ApiPath(path): ApiPath<GroupPath>,
) -> AppResult<StatusCode> {
    auth.require_own_session("Leaving a group")?;
    state.store.leave_group(&auth.actor, path.group_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/v1/invitations[?for=si:x]`: pending group invitations.
pub(super) async fn invitations(
    State(state): State<AppState>,
    Authenticated(auth): Authenticated,
    ApiQuery(query): ApiQuery<ForQuery>,
) -> AppResult<Json<serde_json::Value>> {
    let invitee = match &query.for_account {
        Some(silicon) => super::handlers::looked_after(&state, &auth, silicon).await?,
        None => auth.actor.clone(),
    };
    let items: Vec<Invitation> = state.store.invitations_for(&invitee).await?;
    Ok(Json(serde_json::json!({ "items": items })))
}

async fn require_manager(state: &AppState, auth: &AuthContext, id: Uuid) -> AppResult<()> {
    auth.require_own_session("Managing a group")?;
    if state.store.manages_group(&auth.actor, id).await? {
        Ok(())
    } else {
        Err(not_manager())
    }
}

fn not_manager() -> AppError {
    AppError::forbidden(
        "not_group_owner",
        "Only the group's owners (and the custodian of a Silicon owner) manage it. Ask an owner to make you an owner.",
    )
}

async fn resolve(state: &AppState, ids: &[ActorId]) -> AppResult<Vec<Account>> {
    if ids.is_empty() || ids.len() > MAX_MEMBERS_PER_REQUEST {
        return Err(AppError::validation(
            "member_ids must name 1 to 100 accounts per request",
        ));
    }
    state.directory().resolve_all(ids).await
}

/// Resolves invitees and applies the reach rules: a Silicon is invited only by its
/// circle or an account on its allow-list; a custodian's own Silicons join at once.
async fn invitees(
    state: &AppState,
    auth: &AuthContext,
    ids: &[ActorId],
) -> AppResult<Vec<Invitee>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut invitees = Vec::new();
    for account in resolve(state, ids).await? {
        if account.deleted {
            return Err(AppError::account_deleted(account.display_id()));
        }
        state
            .directory()
            .require_may_reach(&auth.actor, &account)
            .await?;
        let accepted = account.custodian.as_deref() == Some(auth.actor.id.as_str());
        invitees.push(Invitee {
            actor: account.actor(),
            accepted,
        });
    }
    Ok(invitees)
}
