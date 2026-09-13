//! Group policy, invitation mutation, and token-scoped access checks.
use super::{
    PostgresStore,
    directory::refresh_directory_in,
    idempotency::{IdempotencyClaim, IdempotencyResult, claim, complete, request_hash},
    map_constraint_error,
};
use crate::{
    AppError, AppResult,
    application::auth::AuthContext,
    domain::{ActorRef, Conversation, GroupDetails, GroupSettings, IdempotencyKey, OrganizationId},
};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

/// Only IAM-disclosed organization owners and administrators manage groups.
pub(crate) fn require_group_admin(auth: &AuthContext) -> AppResult<()> {
    if matches!(
        auth.org_role.as_deref(),
        Some("owner" | "admin" | "org_owner" | "org_admin")
    ) {
        Ok(())
    } else {
        Err(AppError::Forbidden)
    }
}
impl PostgresStore {
    /// Checks group policy against this exact credential, never broader cached tag disclosure.
    /// Direct conversations retain their existing participant checks.
    /// # Errors
    /// Rejects inaccessible or cross-organization conversations.
    pub async fn check_group_access(&self, auth: &AuthContext, id: Uuid) -> AppResult<()> {
        let tags: Vec<Uuid> = auth.tag_ids.iter().flatten().copied().collect();
        let allowed:bool=sqlx::query_scalar("SELECT NOT c.is_group OR EXISTS (SELECT 1 FROM groups g WHERE g.conversation_id=c.id AND (EXISTS(SELECT 1 FROM group_invitations i WHERE i.conversation_id=g.conversation_id AND i.actor_kind=$3::text::actor_kind AND i.actor_id=$4) OR (g.is_public AND $3='carbon') OR (NOT g.is_public AND g.tag_ids && $5))) FROM conversations c WHERE c.id=$1 AND c.organization_id=$2")
            .bind(id).bind(auth.organization_id.as_str()).bind(auth.actor.actor_type.as_str()).bind(auth.actor.id.as_str()).bind(tags).fetch_optional(self.pool()).await?.unwrap_or(false);
        if allowed {
            Ok(())
        } else {
            Err(AppError::NotFound)
        }
    }
    /// Reads metadata without history; callers must independently authorize access or administration.
    /// # Errors
    /// Propagates storage and metadata decoding failures.
    pub async fn group_details(
        &self,
        org: &OrganizationId,
        id: Uuid,
    ) -> AppResult<Option<GroupDetails>> {
        let row:Option<sqlx::types::Json<GroupDetails>>=sqlx::query_scalar("SELECT jsonb_build_object('name',g.name,'description',g.description,'is_public',g.is_public,'tag_ids',g.tag_ids,'version',g.version,'invited_members',COALESCE((SELECT jsonb_agg(jsonb_build_object('type',i.actor_kind::text,'id',i.actor_id) ORDER BY i.actor_kind,i.actor_id) FROM group_invitations i WHERE i.conversation_id=g.conversation_id),'[]'::jsonb)) FROM groups g WHERE g.conversation_id=$1 AND g.organization_id=$2")
            .bind(id).bind(org.as_str()).fetch_optional(self.pool()).await?;
        Ok(row.map(|row| row.0))
    }
    /// Creates a named group with a stable conversation UUID and idempotent invitations.
    /// # Errors
    /// Rejects unauthorized creators, invalid policies/actors, and conflicting retries.
    pub async fn create_group(
        &self,
        auth: &AuthContext,
        mut settings: GroupSettings,
        members: Vec<ActorRef>,
        key: &IdempotencyKey,
    ) -> AppResult<Conversation> {
        require_group_admin(auth)?;
        settings.validate()?;
        let mut members = members;
        members.push(auth.actor.clone());
        members.sort_by(|a, b| {
            (a.actor_type.as_str(), a.id.as_str()).cmp(&(b.actor_type.as_str(), b.id.as_str()))
        });
        members.dedup();
        let hash = request_hash(&(&settings, &members))?;
        let mut tx = self.pool().begin().await?;
        refresh_directory_in(&mut tx, &auth.organization_id, &members).await?;
        let id = match claim(
            &mut tx,
            &auth.organization_id,
            &auth.actor,
            "groups.create",
            key,
            &hash,
        )
        .await?
        {
            IdempotencyClaim::Replay(id) => id,
            IdempotencyClaim::Acquired => {
                let id = Uuid::now_v7();
                let digest = blake3::hash(id.as_bytes());
                sqlx::query("INSERT INTO conversations(id,organization_id,participant_set_hash,created_by_kind,created_by_id,is_group) VALUES($1,$2,$3,$4::text::actor_kind,$5,true)").bind(id).bind(auth.organization_id.as_str()).bind(digest.as_bytes().as_slice()).bind(auth.actor.actor_type.as_str()).bind(auth.actor.id.as_str()).execute(&mut *tx).await?;
                sqlx::query("INSERT INTO groups(conversation_id,organization_id,name,description,is_public,tag_ids) VALUES($1,$2,$3,$4,$5,$6)").bind(id).bind(auth.organization_id.as_str()).bind(&settings.name).bind(&settings.description).bind(settings.is_public).bind(&settings.tag_ids).execute(&mut *tx).await?;
                invite_in(&mut tx, &auth.organization_id, id, &members).await?;
                complete_group(&mut tx, auth, "groups.create", key, id).await?;
                id
            }
        };
        sqlx::query("SELECT sync_group_participants($1)")
            .bind(auth.organization_id.as_str())
            .execute(&mut *tx)
            .await?;
        tx.commit().await.map_err(map_constraint_error)?;
        self.get_conversation(&auth.organization_id, &auth.actor, id)
            .await
    }
    /// Updates name, description and access policy using an exact version.
    /// # Errors
    /// Rejects unauthorized callers, stale versions and conflicting retry bodies.
    pub async fn update_group(
        &self,
        auth: &AuthContext,
        id: Uuid,
        mut settings: GroupSettings,
        version: i64,
        key: &IdempotencyKey,
    ) -> AppResult<GroupDetails> {
        require_group_admin(auth)?;
        settings.validate()?;
        if version < 1 {
            return Err(AppError::validation(
                "If-Match must be a positive group version",
            ));
        }
        let hash = request_hash(&(id, &settings, version))?;
        let mut tx = self.pool().begin().await?;
        lock_group(&mut tx, &auth.organization_id, id).await?;
        if matches!(
            claim(
                &mut tx,
                &auth.organization_id,
                &auth.actor,
                "groups.update",
                key,
                &hash
            )
            .await?,
            IdempotencyClaim::Acquired
        ) {
            let changed=sqlx::query("UPDATE groups SET name=$3,description=$4,is_public=$5,tag_ids=$6,version=version+1 WHERE conversation_id=$1 AND organization_id=$2 AND version=$7").bind(id).bind(auth.organization_id.as_str()).bind(&settings.name).bind(&settings.description).bind(settings.is_public).bind(&settings.tag_ids).bind(version).execute(&mut *tx).await?;
            if changed.rows_affected() != 1 {
                return Err(AppError::conflict(
                    "group version changed; fetch the group and retry with its current version",
                ));
            }
            complete_group(&mut tx, auth, "groups.update", key, id).await?;
        }
        finish_group(&mut tx, &auth.organization_id, id).await?;
        tx.commit().await.map_err(map_constraint_error)?;
        self.group_details(&auth.organization_id, id)
            .await?
            .ok_or(AppError::NotFound)
    }
    /// Adds explicit invitations or removes one invitation; tag/public grants remain independent.
    /// # Errors
    /// Rejects unauthorized administrators, cross-organization groups and conflicting retries.
    pub async fn change_group_members(
        &self,
        auth: &AuthContext,
        id: Uuid,
        members: Vec<ActorRef>,
        remove: bool,
        key: &IdempotencyKey,
    ) -> AppResult<GroupDetails> {
        require_group_admin(auth)?;
        let mut members = members;
        members.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
        members.dedup();
        let operation = if remove {
            "groups.uninvite"
        } else {
            "groups.invite"
        };
        let hash = request_hash(&(id, &members))?;
        let mut tx = self.pool().begin().await?;
        lock_group(&mut tx, &auth.organization_id, id).await?;
        if matches!(
            claim(
                &mut tx,
                &auth.organization_id,
                &auth.actor,
                operation,
                key,
                &hash
            )
            .await?,
            IdempotencyClaim::Acquired
        ) {
            if remove {
                for actor in &members {
                    sqlx::query("DELETE FROM group_invitations WHERE conversation_id=$1 AND organization_id=$2 AND actor_kind=$3::text::actor_kind AND actor_id=$4").bind(id).bind(auth.organization_id.as_str()).bind(actor.actor_type.as_str()).bind(actor.id.as_str()).execute(&mut *tx).await?;
                }
            } else {
                refresh_directory_in(&mut tx, &auth.organization_id, &members).await?;
                invite_in(&mut tx, &auth.organization_id, id, &members).await?;
            }
            sqlx::query("UPDATE groups SET version=version+1 WHERE conversation_id=$1")
                .bind(id)
                .execute(&mut *tx)
                .await?;
            complete_group(&mut tx, auth, operation, key, id).await?;
        }
        finish_group(&mut tx, &auth.organization_id, id).await?;
        tx.commit().await.map_err(map_constraint_error)?;
        self.group_details(&auth.organization_id, id)
            .await?
            .ok_or(AppError::NotFound)
    }
}
async fn lock_group(
    tx: &mut Transaction<'_, Postgres>,
    org: &OrganizationId,
    id: Uuid,
) -> AppResult<()> {
    let found:Option<Uuid>=sqlx::query_scalar("SELECT c.id FROM conversations c JOIN groups g ON g.conversation_id=c.id WHERE c.id=$1 AND c.organization_id=$2 FOR UPDATE OF c,g").bind(id).bind(org.as_str()).fetch_optional(&mut **tx).await?;
    found.ok_or(AppError::NotFound).map(|_| ())
}
async fn invite_in(
    tx: &mut Transaction<'_, Postgres>,
    org: &OrganizationId,
    id: Uuid,
    members: &[ActorRef],
) -> AppResult<()> {
    for actor in members {
        sqlx::query("INSERT INTO group_invitations(conversation_id,organization_id,actor_kind,actor_id) VALUES($1,$2,$3::text::actor_kind,$4) ON CONFLICT DO NOTHING").bind(id).bind(org.as_str()).bind(actor.actor_type.as_str()).bind(actor.id.as_str()).execute(&mut **tx).await?;
    }
    Ok(())
}
async fn complete_group(
    tx: &mut Transaction<'_, Postgres>,
    auth: &AuthContext,
    op: &str,
    key: &IdempotencyKey,
    id: Uuid,
) -> AppResult<()> {
    complete(
        tx,
        &auth.organization_id,
        &auth.actor,
        op,
        key,
        IdempotencyResult {
            resource_type: "conversation",
            resource_id: id,
            response_status: 200,
        },
    )
    .await
}
async fn finish_group(
    tx: &mut Transaction<'_, Postgres>,
    org: &OrganizationId,
    id: Uuid,
) -> AppResult<()> {
    sqlx::query("SELECT sync_group_participants($1)")
        .bind(org.as_str())
        .execute(&mut **tx)
        .await?;
    sqlx::query("UPDATE conversations SET updated_at=clock_timestamp() WHERE id=$1")
        .bind(id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}
