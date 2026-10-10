//! Who am I, how to sign in, how updates reach me, and a Silicon's allow-list.

use axum::{Json, extract::State};
use serde::Deserialize;
use serde_json::{Value, json};

use super::extract::{ApiJson, ApiQuery, Authenticated};
use crate::{
    AppError, AppResult,
    application::{auth::AuthContext, state::AppState},
    domain::{ActorId, ActorRef, ActorType},
};

/// `GET /api/v1/me`: the signed-in account, its custodian or the Silicons it looks after,
/// and how DM delivers to it.
pub(super) async fn me(
    State(state): State<AppState>,
    Authenticated(auth): Authenticated,
) -> AppResult<Json<Value>> {
    let stored = state
        .store
        .stored_account(auth.actor.id.as_str())
        .await?
        .ok_or(AppError::NotFound)?;
    let silicons: Vec<Value> = if auth.actor.actor_type == ActorType::Carbon {
        state
            .store
            .silicons_of(auth.actor.id.as_str())
            .await?
            .into_iter()
            .map(|uuid| reference(ActorType::Silicon, &uuid))
            .collect()
    } else {
        Vec::new()
    };
    Ok(Json(json!({
        "account": reference(auth.actor.actor_type, auth.actor.id.as_str()),
        "custodian": stored.custodian_uuid.map(|uuid| reference(ActorType::Carbon, &uuid)),
        "custodian_of": silicons,
        "scopes": auth.scopes,
        "via_app": auth.via_app(),
        "delivery": delivery(&state, &auth).await?,
    })))
}

/// `GET /api/v1/accounts` (no sign-in needed): how to sign in to DM.
pub(super) async fn accounts(State(state): State<AppState>) -> Json<Value> {
    let settings = &state.settings;
    Json(json!({
        "app_id": settings.accounts.app_id,
        "accounts_url": settings.accounts.issuer,
        "api_base_url": settings.server.public_base_url,
        "contract_version": super::contracts::HTTP_CONTRACT_VERSION,
        "sign_in": {
            "carbons": "device flow (`dm login`) or the DM website",
            "silicons": "short-lived token: `silicon-accounts login --app dm -q | dm login --slt-stdin`",
            "device_flow": true,
            "slt_public_client": true,
        },
        "proof_scopes": crate::config::PROOF_SCOPES,
        "delivery": super::contracts::delivery(&state),
    }))
}

/// `GET /api/v1/delivery`: whether updates go through Ting and how to sync without it.
pub(super) async fn delivery_status(
    State(state): State<AppState>,
    Authenticated(auth): Authenticated,
) -> AppResult<Json<Value>> {
    Ok(Json(delivery(&state, &auth).await?))
}

async fn delivery(state: &AppState, auth: &AuthContext) -> AppResult<Value> {
    let enrolled = if state.ting_enabled() {
        state
            .store
            .ting_enrolled(&state.settings.accounts.app_id, auth.actor.id.as_str())
            .await?
    } else {
        false
    };
    let mut value = super::contracts::delivery(state);
    value["enrolled"] = json!(enrolled);
    Ok(value)
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ForQuery {
    #[serde(rename = "for")]
    for_account: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AccountIds {
    account_ids: Vec<ActorId>,
}

/// The Silicon whose allow-list the caller manages: itself, or one it looks after.
async fn list_owner(state: &AppState, auth: &AuthContext, query: &ForQuery) -> AppResult<ActorRef> {
    auth.require_own_session("Managing an allow-list")?;
    let silicon = match &query.for_account {
        Some(selector) => super::handlers::looked_after(state, auth, selector).await?,
        None => auth.actor.clone(),
    };
    if silicon.actor_type != ActorType::Silicon {
        return Err(AppError::forbidden(
            "carbon_has_no_allow_list",
            "Only Silicons have an allow-list: any account signed in to DM can message a Carbon. A custodian manages a Silicon's list with ?for=si:handle.",
        ));
    }
    Ok(silicon)
}

async fn allow_list_view(state: &AppState, silicon: &ActorRef) -> AppResult<Json<Value>> {
    let items = state.store.allow_list(silicon.id.as_str()).await?;
    Ok(Json(json!({
        "silicon": reference(ActorType::Silicon, silicon.id.as_str()),
        "items": items,
        "note": "Accounts on this list may start conversations with the Silicon and invite it to groups. Its custodian and the custodian's other Silicons never need to be listed.",
    })))
}

/// `GET /api/v1/allow-list[?for=si:x]`.
pub(super) async fn allow_list(
    State(state): State<AppState>,
    Authenticated(auth): Authenticated,
    ApiQuery(query): ApiQuery<ForQuery>,
) -> AppResult<Json<Value>> {
    let silicon = list_owner(&state, &auth, &query).await?;
    allow_list_view(&state, &silicon).await
}

/// `POST /api/v1/allow-list[?for=si:x]` `{"account_ids": [...]}`.
pub(super) async fn allow(
    State(state): State<AppState>,
    Authenticated(auth): Authenticated,
    ApiQuery(query): ApiQuery<ForQuery>,
    ApiJson(input): ApiJson<AccountIds>,
) -> AppResult<Json<Value>> {
    let silicon = list_owner(&state, &auth, &query).await?;
    validate_count(&input.account_ids)?;
    for account in state.directory().resolve_all(&input.account_ids).await? {
        if account.uuid == silicon.id.as_str() {
            return Err(AppError::validation(
                "a Silicon is always allowed to reach itself",
            ));
        }
        state
            .store
            .allow_contact(
                silicon.id.as_str(),
                &account.actor(),
                auth.actor.id.as_str(),
            )
            .await?;
    }
    allow_list_view(&state, &silicon).await
}

/// `DELETE /api/v1/allow-list[?for=si:x]` `{"account_ids": [...]}`. Existing conversations
/// keep working.
pub(super) async fn disallow(
    State(state): State<AppState>,
    Authenticated(auth): Authenticated,
    ApiQuery(query): ApiQuery<ForQuery>,
    ApiJson(input): ApiJson<AccountIds>,
) -> AppResult<Json<Value>> {
    let silicon = list_owner(&state, &auth, &query).await?;
    validate_count(&input.account_ids)?;
    for account in state.directory().resolve_all(&input.account_ids).await? {
        state
            .store
            .disallow_contact(silicon.id.as_str(), &account.actor())
            .await?;
    }
    allow_list_view(&state, &silicon).await
}

fn validate_count(ids: &[ActorId]) -> AppResult<()> {
    if ids.is_empty() || ids.len() > 100 {
        return Err(AppError::validation(
            "account_ids must name 1 to 100 accounts",
        ));
    }
    Ok(())
}

fn reference(kind: ActorType, uuid: &str) -> Value {
    json!({ "type": kind.as_str(), "id": uuid })
}
