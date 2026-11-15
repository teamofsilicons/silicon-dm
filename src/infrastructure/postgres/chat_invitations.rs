//! Invitations name permanent accounts, or an email whose owner must prove it at acceptance.
use serde::Serialize;
use sqlx::{FromRow, types::Json};
use time::OffsetDateTime;
use uuid::Uuid;

use super::{
    PostgresStore,
    conversations::canonical_or_insert_in,
    directory::refresh_directory_in,
    idempotency::{IdempotencyClaim, IdempotencyResult, claim, complete, request_hash},
};
use crate::{
    AppError, AppResult,
    domain::{ActorRef, IdempotencyKey, OrganizationId},
};

/// The public invitation, visible only to its sender and recipient.
#[derive(Clone, Serialize, FromRow)]
pub(crate) struct ChatInvitation {
    pub id: Uuid,
    pub inviter: Json<ActorRef>,
    pub partner: Json<ActorRef>,
    pub recipient: Option<Json<ActorRef>>,
    pub email: Option<String>,
    pub status: String,
    pub conversation_id: Option<Uuid>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
    pub mail_status: String,
    #[serde(skip)]
    pub answered_by: Option<String>,
}

const SELECT: &str = "SELECT i.id, jsonb_build_object('type',a.kind,'id',a.uuid) AS inviter,
    jsonb_build_object('type',p.kind,'id',p.uuid) AS partner,
    CASE WHEN r.uuid IS NULL THEN NULL ELSE jsonb_build_object('type',r.kind,'id',r.uuid) END AS recipient,
    i.recipient_email AS email, i.status, i.conversation_id, i.created_at, i.expires_at, i.mail_status, i.answered_by
    FROM chat_invitations i JOIN accounts a ON a.uuid=i.inviter_uuid
    JOIN accounts p ON p.uuid=i.partner_uuid LEFT JOIN accounts r ON r.uuid=i.recipient_uuid";

impl ChatInvitation {
    pub fn is_recipient(&self, actor: &ActorRef, email: Option<&str>) -> bool {
        // Once answered, an email address changing owners cannot transfer the invitation.
        if let Some(answered) = &self.answered_by {
            return answered == actor.id.as_str();
        }
        self.recipient
            .as_ref()
            .is_some_and(|recipient| recipient.0 == *actor)
            || self
                .email
                .as_deref()
                .zip(email)
                .is_some_and(|(invited, verified)| invited == verified)
    }

    pub fn require_pending(&self) -> AppResult<()> {
        if self.status != "pending" || self.expires_at <= OffsetDateTime::now_utc() {
            return Err(AppError::refused(
                http::StatusCode::GONE,
                "invitation_unavailable",
                "This chat invitation has expired or was already answered.",
            )
            .with_hint(
                "Ask the sender for a new invitation, or open your existing conversation.",
            ));
        }
        Ok(())
    }
}

/// Resolved creation input. Only the service chooses these identities and mail state.
pub(crate) struct NewChatInvitation<'a> {
    pub inviter: &'a ActorRef,
    pub partner: &'a ActorRef,
    pub recipient: Option<&'a ActorRef>,
    pub email: Option<&'a str>,
    pub mail_status: &'a str,
    pub key: &'a IdempotencyKey,
    pub request: &'a serde_json::Value,
}

impl PostgresStore {
    pub(crate) async fn chat_invitation(&self, id: Uuid) -> AppResult<ChatInvitation> {
        sqlx::query_as(sqlx::AssertSqlSafe(format!("{SELECT} WHERE i.id=$1")))
            .bind(id)
            .fetch_optional(self.pool())
            .await?
            .ok_or_else(|| AppError::not_found("This chat invitation could not be found."))
    }

    pub(crate) async fn list_chat_invitations(
        &self,
        actor: &ActorRef,
        email: Option<&str>,
    ) -> AppResult<Vec<ChatInvitation>> {
        Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "{SELECT}
            WHERE i.status='pending' AND i.expires_at>now()
            AND (i.recipient_uuid=$1 OR i.recipient_email=$2)
            ORDER BY i.created_at DESC LIMIT 100"
        )))
        .bind(actor.id.as_str())
        .bind(email)
        .fetch_all(self.pool())
        .await?)
    }

    pub(crate) async fn create_chat_invitation(
        &self,
        input: NewChatInvitation<'_>,
    ) -> AppResult<ChatInvitation> {
        let mut tx = self.pool().begin().await?;
        let partition = OrganizationId::accounts();
        refresh_directory_in(&mut tx, &partition, std::slice::from_ref(input.inviter)).await?;
        let hash = request_hash(input.request)?;
        let operation = "chat_invitations.create";
        let claimed = claim(
            &mut tx,
            &partition,
            input.inviter,
            operation,
            input.key,
            &hash,
        )
        .await?;
        let id = match claimed {
            IdempotencyClaim::Replay(id) => id,
            IdempotencyClaim::Acquired => {
                // Serialize the sender's budget across replicas and different idempotency keys.
                sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,54))")
                    .bind(input.inviter.id.as_str())
                    .execute(&mut *tx)
                    .await?;
                check_partner(
                    &mut tx,
                    input.inviter.id.as_str(),
                    input.partner.id.as_str(),
                )
                .await?;
                let duplicate: Option<Uuid> = sqlx::query_scalar("SELECT id FROM chat_invitations
                    WHERE inviter_uuid=$1 AND partner_uuid=$2 AND recipient_uuid IS NOT DISTINCT FROM $3
                    AND recipient_email IS NOT DISTINCT FROM $4 AND status='pending' AND expires_at>now()
                    ORDER BY created_at DESC LIMIT 1")
                    .bind(input.inviter.id.as_str()).bind(input.partner.id.as_str())
                    .bind(input.recipient.map(|a| a.id.as_str())).bind(input.email)
                    .fetch_optional(&mut *tx).await?;
                let id = if let Some(id) = duplicate {
                    id
                } else {
                    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM chat_invitations WHERE inviter_uuid=$1 AND created_at>now()-interval '1 hour'")
                        .bind(input.inviter.id.as_str()).fetch_one(&mut *tx).await?;
                    if count >= 20 {
                        return Err(AppError::rate_limited(
                            "This account sent 20 chat invitations in the last hour.",
                        )
                        .with_hint(
                            "Try again in an hour. Existing invitations are still available.",
                        )
                        .with_retry_after(3600));
                    }
                    let id = Uuid::new_v4();
                    sqlx::query("INSERT INTO chat_invitations(id,inviter_uuid,partner_uuid,recipient_uuid,recipient_email,mail_status) VALUES($1,$2,$3,$4,$5,$6)")
                        .bind(id).bind(input.inviter.id.as_str()).bind(input.partner.id.as_str())
                        .bind(input.recipient.map(|a| a.id.as_str())).bind(input.email).bind(input.mail_status)
                        .execute(&mut *tx).await?;
                    id
                };
                complete(
                    &mut tx,
                    &partition,
                    input.inviter,
                    operation,
                    input.key,
                    IdempotencyResult {
                        resource_type: "chat_invitation",
                        resource_id: id,
                        response_status: 201,
                    },
                )
                .await?;
                id
            }
        };
        tx.commit().await?;
        self.chat_invitation(id).await
    }

    pub(crate) async fn answer_chat_invitation(
        &self,
        invitation: &ChatInvitation,
        actor: &ActorRef,
        email: Option<&str>,
        accept: bool,
        key: &IdempotencyKey,
    ) -> AppResult<ChatInvitation> {
        let mut participants = vec![actor.clone(), invitation.partner.0.clone()];
        participants.sort_by(|a, b| {
            (a.actor_type.as_str(), a.id.as_str()).cmp(&(b.actor_type.as_str(), b.id.as_str()))
        });
        if accept && participants[0] == participants[1] {
            return Err(AppError::invalid(
                "You cannot accept an invitation to chat with yourself.",
            ));
        }
        let mut tx = self.pool().begin().await?;
        let partition = OrganizationId::accounts();
        // Follow the messaging lock order: directory, idempotency, invitation, conversation, streams.
        refresh_directory_in(&mut tx, &partition, &participants).await?;
        let operation = "chat_invitations.answer";
        let hash = request_hash(&(invitation.id, accept))?;
        let claimed = claim(&mut tx, &partition, actor, operation, key, &hash).await?;
        if matches!(claimed, IdempotencyClaim::Acquired) {
            let current: ChatInvitation = sqlx::query_as(sqlx::AssertSqlSafe(format!(
                "{SELECT} WHERE i.id=$1 FOR UPDATE OF i"
            )))
            .bind(invitation.id)
            .fetch_one(&mut *tx)
            .await?;
            if !current.is_recipient(actor, email) {
                return Err(recipient_required());
            }
            let desired = if accept { "accepted" } else { "declined" };
            if current.status != desired {
                current.require_pending()?;
                let conversation = if accept {
                    check_partner(
                        &mut tx,
                        current.inviter.id.as_str(),
                        current.partner.id.as_str(),
                    )
                    .await?;
                    Some(canonical_or_insert_in(&mut tx, actor, &participants).await?)
                } else {
                    None
                };
                sqlx::query("UPDATE chat_invitations SET status=$2,conversation_id=$3,answered_by=$4 WHERE id=$1")
                    .bind(current.id).bind(desired).bind(conversation).bind(actor.id.as_str()).execute(&mut *tx).await?;
            }
            complete(
                &mut tx,
                &partition,
                actor,
                operation,
                key,
                IdempotencyResult {
                    resource_type: "chat_invitation",
                    resource_id: current.id,
                    response_status: 200,
                },
            )
            .await?;
        }
        tx.commit().await?;
        self.chat_invitation(invitation.id).await
    }
}

pub(crate) fn recipient_required() -> AppError {
    AppError::forbidden("invitation_recipient_required", "This chat invitation belongs to a different account or email address.")
        .with_hint("Sign in to the invited Carbon or Silicon. For an email invitation, verify that address, make it your primary email in Silicon Accounts, and share email with DM when signing in.")
}

async fn check_partner(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    inviter: &str,
    partner: &str,
) -> AppResult<()> {
    let allowed: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM accounts a JOIN accounts p ON p.uuid=$2
        WHERE a.uuid=$1 AND a.status='active' AND p.status='active'
        AND (a.uuid=p.uuid OR (a.kind='carbon' AND p.kind='silicon' AND p.custodian_uuid=a.uuid)))",
    )
    .bind(inviter)
    .bind(partner)
    .fetch_one(&mut **tx)
    .await?;
    if !allowed {
        return Err(AppError::forbidden(
            "invitation_partner_forbidden",
            "You may invite someone to chat with yourself or a Silicon you currently look after.",
        )
        .with_hint("Choose yourself or one of your own Silicons."));
    }
    Ok(())
}
