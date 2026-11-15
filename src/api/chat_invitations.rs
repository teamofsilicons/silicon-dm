//! Account-bound invitations. Opening an email never authorizes a different account.
use super::extract::{ApiJson, ApiPath, Authenticated, Idempotency};
use crate::{
    AppError, AppResult,
    application::{auth::AuthContext, state::AppState},
    domain::Account,
    infrastructure::postgres::{
        chat_invitations::{ChatInvitation, NewChatInvitation, recipient_required},
        valid_public_id,
    },
};
use axum::{Json, extract::State, http::StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct InviteRequest {
    to: String,
    #[serde(default)]
    with: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct InvitationPath {
    invitation_id: Uuid,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Empty {}

async fn fresh_account(state: &AppState, id: &str) -> AppResult<Account> {
    let account = state.directory().resolve(id).await?;
    let lookup = state.identity.lookup(&account.uuid).await?;
    state.store.record_lookup(&lookup).await?;
    state.directory().account(&account.uuid).await
}

fn response(state: &AppState, invitation: &ChatInvitation) -> AppResult<Value> {
    let mut value = serde_json::to_value(invitation).map_err(AppError::internal)?;
    value["url"] = json!(format!(
        "{}invite/{}",
        state.settings.web_url, invitation.id
    ));
    Ok(value)
}

pub(super) async fn create(
    State(state): State<AppState>,
    Authenticated(context): Authenticated,
    Idempotency(key): Idempotency,
    ApiJson(mut request): ApiJson<InviteRequest>,
) -> AppResult<(StatusCode, Json<Value>)> {
    request.to = request.to.trim().to_lowercase();
    let original = serde_json::to_value(&request).map_err(AppError::internal)?;
    let partner = fresh_account(
        &state,
        request.with.as_deref().unwrap_or(context.actor.id.as_str()),
    )
    .await?;
    let (recipient, email) = if valid_public_id(&request.to) {
        (Some(fresh_account(&state, &request.to).await?), None)
    } else {
        (None, Some(normalize_email(&request.to)?))
    };
    if recipient.as_ref().is_some_and(|a| a.uuid == partner.uuid) {
        return Err(AppError::invalid(
            "Choose someone other than the account they are invited to chat with.",
        ));
    }
    let mail_status = if email.is_none() {
        "none"
    } else if state.settings.environment != crate::config::RuntimeEnvironment::Production {
        "simulated"
    } else if state.settings.reporting.is_some() {
        "queued"
    } else {
        return Err(AppError::DependencyUnavailable {
            dependency: "postmark_configuration",
        });
    };
    let recipient_actor = recipient.as_ref().map(Account::actor);
    let invitation = state
        .store
        .create_chat_invitation(NewChatInvitation {
            inviter: &context.actor,
            partner: &partner.actor(),
            recipient: recipient_actor.as_ref(),
            email: email.as_deref(),
            mail_status,
            key: &key,
            request: &original,
        })
        .await?;
    Ok((StatusCode::CREATED, Json(response(&state, &invitation)?)))
}

pub(super) async fn list(
    State(state): State<AppState>,
    Authenticated(context): Authenticated,
) -> AppResult<Json<Value>> {
    let email = state.identity.verified_email(&context).await?;
    let items = state
        .store
        .list_chat_invitations(&context.actor, email.as_deref())
        .await?;
    Ok(Json(
        json!({"items": items.iter().map(|i| response(&state, i)).collect::<AppResult<Vec<_>>>()?}),
    ))
}

pub(super) async fn get(
    State(state): State<AppState>,
    Authenticated(context): Authenticated,
    ApiPath(path): ApiPath<InvitationPath>,
) -> AppResult<Json<Value>> {
    let invitation = state.store.chat_invitation(path.invitation_id).await?;
    if invitation.inviter.0 != context.actor {
        let email = recipient_email(&state, &context, &invitation).await?;
        if !invitation.is_recipient(&context.actor, email.as_deref()) {
            return Err(recipient_required());
        }
    }
    Ok(Json(response(&state, &invitation)?))
}

async fn recipient_email(
    state: &AppState,
    context: &AuthContext,
    invitation: &ChatInvitation,
) -> AppResult<Option<String>> {
    if invitation.email.is_some() && invitation.answered_by.is_none() {
        state.identity.verified_email(context).await
    } else {
        Ok(None)
    }
}

async fn answer(
    state: AppState,
    context: AuthContext,
    id: Uuid,
    key: crate::domain::IdempotencyKey,
    accept: bool,
) -> AppResult<Json<Value>> {
    let invitation = state.store.chat_invitation(id).await?;
    let email = recipient_email(&state, &context, &invitation).await?;
    if !invitation.is_recipient(&context.actor, email.as_deref()) {
        return Err(recipient_required());
    }
    if accept && invitation.status == "pending" {
        fresh_account(&state, invitation.inviter.id.as_str()).await?;
        fresh_account(&state, invitation.partner.id.as_str()).await?;
    }
    let answered = state
        .store
        .answer_chat_invitation(&invitation, &context.actor, email.as_deref(), accept, &key)
        .await?;
    Ok(Json(response(&state, &answered)?))
}

pub(super) async fn accept(
    State(state): State<AppState>,
    Authenticated(context): Authenticated,
    ApiPath(path): ApiPath<InvitationPath>,
    Idempotency(key): Idempotency,
    ApiJson(_): ApiJson<Empty>,
) -> AppResult<Json<Value>> {
    answer(state, context, path.invitation_id, key, true).await
}

pub(super) async fn decline(
    State(state): State<AppState>,
    Authenticated(context): Authenticated,
    ApiPath(path): ApiPath<InvitationPath>,
    Idempotency(key): Idempotency,
    ApiJson(_): ApiJson<Empty>,
) -> AppResult<Json<Value>> {
    answer(state, context, path.invitation_id, key, false).await
}

fn normalize_email(value: &str) -> AppResult<String> {
    let valid = value.len() <= 254
        && value.is_ascii()
        && value.split_once('@').is_some_and(|(local, domain)| {
            !local.is_empty()
                && local.len() <= 64
                && !local.starts_with('.')
                && !local.ends_with('.')
                && !local.contains("..")
                && local
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b".!#$%&'*+-/=?^_`{|}~".contains(&b))
                && domain.contains('.')
                && domain.split('.').all(|label| {
                    !label.is_empty()
                        && label.len() <= 63
                        && !label.starts_with('-')
                        && !label.ends_with('-')
                        && label
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                })
        });
    if !valid {
        return Err(AppError::invalid(
            "Enter a Carbon ID (c:ada), Silicon ID (si:scout), or one email address.",
        ));
    }
    Ok(value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::normalize_email;
    #[test]
    fn accepts_one_mailbox_and_rejects_headers_and_multiple_destinations() {
        assert!(normalize_email("ada+dm@example.com").is_ok());
        for value in [
            "",
            "c:bad id",
            "a@b.com,b@c.com",
            "a@b.com\r\nBcc: c@d.com",
            "Ada <a@b.com>",
            "a..b@c.com",
            "a@-b.com",
            "a@b..com",
        ] {
            assert!(normalize_email(value).is_err(), "{value:?}");
        }
    }
}
