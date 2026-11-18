//! Direct-message admission and the recipient's request inbox.
use super::extract::{ApiJson, ApiPath, Authenticated};
use crate::{
    AppError, AppResult,
    application::{auth::AuthContext, state::AppState},
    domain::ActorRef,
    infrastructure::contacts,
};
use axum::{Json, extract::State, http::StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Preferences {
    contacts_only: bool,
}

pub(super) async fn preferences(
    State(s): State<AppState>,
    Authenticated(c): Authenticated,
) -> AppResult<Json<Preferences>> {
    let enabled = sqlx::query_scalar("SELECT contacts_only FROM direct_preferences WHERE owner=$1")
        .bind(c.actor.id.as_str())
        .fetch_optional(s.store.pool())
        .await?
        .unwrap_or(false);
    Ok(Json(Preferences {
        contacts_only: enabled,
    }))
}
pub(super) async fn configure(
    State(s): State<AppState>,
    Authenticated(c): Authenticated,
    ApiJson(p): ApiJson<Preferences>,
) -> AppResult<Json<Preferences>> {
    sqlx::query("INSERT INTO direct_preferences(owner,contacts_only) VALUES($1,$2) ON CONFLICT(owner) DO UPDATE SET contacts_only=excluded.contacts_only")
        .bind(c.actor.id.as_str()).bind(p.contacts_only).execute(s.store.pool()).await?;
    Ok(Json(p))
}

pub(super) async fn check_conversation(s: &AppState, c: &AuthContext, id: Uuid) -> AppResult<()> {
    let recipients: Vec<String> = sqlx::query_scalar(
        "SELECT p.actor_id FROM conversation_participants p
        JOIN conversations conv ON conv.id=p.conversation_id AND NOT conv.is_group
        JOIN direct_preferences pref ON pref.owner=p.actor_id AND pref.contacts_only
        WHERE p.conversation_id=$1 AND p.actor_id<>$2
        AND EXISTS (SELECT 1 FROM conversation_participants mine WHERE mine.conversation_id=$1 AND mine.actor_id=$2)",
    )
    .bind(id)
    .bind(c.actor.id.as_str())
    .fetch_all(s.store.pool())
    .await?;
    check(s, &c.actor, &recipients).await
}

pub(super) async fn check(s: &AppState, sender: &ActorRef, others: &[String]) -> AppResult<()> {
    if others.is_empty() {
        return Ok(());
    }
    let owner = sender.id.as_str();
    let recipients:Vec<String> = sqlx::query_scalar("SELECT owner FROM direct_preferences WHERE owner=ANY($1) AND contacts_only
        AND NOT EXISTS (SELECT 1 FROM chat_requests r WHERE r.status='accepted'
            AND ((r.recipient=owner AND r.sender=$2) OR (r.recipient=$2 AND r.sender=owner)))
        AND NOT EXISTS (SELECT 1 FROM chat_invitations i WHERE i.status='accepted'
            AND ((i.answered_by=owner AND i.partner_uuid=$2) OR (i.answered_by=$2 AND i.partner_uuid=owner))) ORDER BY owner")
        .bind(others).bind(owner).fetch_all(s.store.pool()).await?;
    let mut pending = false;
    for recipient in recipients {
        if contacts::saved_sender(s, &recipient, owner).await? {
            continue;
        }
        let mut tx = s.store.pool().begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 55))")
            .bind(owner)
            .execute(&mut *tx)
            .await?;
        let existing: Option<String> =
            sqlx::query_scalar("SELECT status FROM chat_requests WHERE recipient=$1 AND sender=$2")
                .bind(&recipient)
                .bind(owner)
                .fetch_optional(&mut *tx)
                .await?;
        if existing.as_deref() == Some("accepted") {
            continue;
        }
        if existing.is_none() {
            let recent:i64=sqlx::query_scalar("SELECT count(*) FROM chat_requests WHERE sender=$1 AND created_at>now()-interval '1 hour'")
                .bind(owner).fetch_one(&mut *tx).await?;
            if recent >= super::access::FIRST_CONTACTS_PER_HOUR {
                return Err(AppError::rate_limited(
                    "The hourly chat request limit has been reached.",
                )
                .with_retry_after(3600));
            }
            sqlx::query("INSERT INTO chat_requests(id,recipient,sender) VALUES($1,$2,$3) ON CONFLICT(recipient,sender) DO NOTHING")
                .bind(Uuid::new_v4()).bind(&recipient).bind(owner).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        pending = true;
    }
    if pending {
        return Err(AppError::refused(StatusCode::CONFLICT, "chat_request_pending", "A chat request has been sent.")
            .with_hint("You can send messages after the recipient accepts. Your message has not been delivered."));
    }
    Ok(())
}

pub(super) async fn list(
    State(s): State<AppState>,
    Authenticated(c): Authenticated,
) -> AppResult<Json<Value>> {
    let items:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('id',r.id,'sender',jsonb_build_object('type',a.kind,'id',a.uuid),'created_at',r.created_at)
        FROM chat_requests r JOIN accounts a ON a.uuid=r.sender WHERE r.recipient=$1 AND r.status='pending' AND a.status='active'
        ORDER BY r.created_at DESC,r.id").bind(c.actor.id.as_str()).fetch_all(s.store.pool()).await?;
    Ok(Json(json!({"items":items})))
}
#[derive(Deserialize)]
pub(super) struct RequestPath {
    request_id: Uuid,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Answer {
    accept: bool,
}

pub(super) async fn answer(
    State(s): State<AppState>,
    Authenticated(c): Authenticated,
    ApiPath(path): ApiPath<RequestPath>,
    ApiJson(a): ApiJson<Answer>,
) -> AppResult<Json<Value>> {
    let desired = if a.accept { "accepted" } else { "declined" };
    let mut tx = s.store.pool().begin().await?;
    let peer: Option<String> = sqlx::query_scalar(
        "UPDATE chat_requests SET status=$3
        WHERE id=$1 AND recipient=$2 AND status IN ('pending',$3) RETURNING sender",
    )
    .bind(path.request_id)
    .bind(c.actor.id.as_str())
    .bind(desired)
    .fetch_optional(&mut *tx)
    .await?;
    let peer = peer.ok_or_else(|| {
        AppError::not_found("This chat request is unavailable or was already answered differently.")
    })?;
    let mut response = json!({"id":path.request_id,"status":desired});
    if a.accept {
        let kind: String =
            sqlx::query_scalar("SELECT kind::text FROM accounts WHERE uuid=$1 AND status='active'")
                .bind(&peer)
                .fetch_optional(&mut *tx)
                .await?
                .ok_or_else(|| AppError::not_found("This account is unavailable."))?;
        let mut participants = vec![
            c.actor.clone(),
            ActorRef {
                actor_type: if kind == "carbon" {
                    crate::domain::ActorType::Carbon
                } else {
                    crate::domain::ActorType::Silicon
                },
                id: crate::domain::ActorId::from_storage_key(peer),
            },
        ];
        crate::infrastructure::postgres::canonicalize_participants(&mut participants);
        let id = crate::infrastructure::postgres::canonical_or_insert_in(
            &mut tx,
            &c.actor,
            &participants,
        )
        .await?;
        response["conversation_id"] = json!(id);
    }
    tx.commit().await?;
    Ok(Json(response))
}
