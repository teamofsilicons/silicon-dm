//! Groups: co-equal owners, invitations that must be accepted, members leaving, and a
//! group never left without an owner. Membership is the accepted invitation; the
//! conversation roster keeps an immutable anchor row per account that ever joined.
use std::collections::HashMap;

use serde::Serialize;
use sqlx::{FromRow, Postgres, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use super::{
    PostgresStore,
    conversation_events::{self, each, group_invitees_in, group_members_in},
    directory::refresh_directory_in,
    idempotency::{IdempotencyClaim, IdempotencyResult, claim, complete, request_hash},
    map_constraint_error,
};
use crate::{
    AppError, AppResult,
    domain::{
        ActorId, ActorRef, ActorType, Conversation, ConversationChange, GroupDetails,
        GroupSettings, IdempotencyKey, OrganizationId,
    },
};

/// An account to add to a group, and whether it joins at once (a custodian adding its
/// own Silicon) or must accept first.
#[derive(Clone, Debug, Serialize)]
pub struct Invitee {
    /// The account.
    pub actor: ActorRef,
    /// Joins without an acceptance step.
    pub accepted: bool,
}

/// A pending invitation as the invitee sees it.
#[derive(Clone, Debug, Serialize)]
pub struct Invitation {
    /// The group (rendered as its group id).
    pub conversation_id: Uuid,
    /// Group name.
    pub name: String,
    /// Group description.
    pub description: String,
    /// The invited account (the caller, or a Silicon it is custodian of).
    pub invitee: ActorRef,
    /// Who invited it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invited_by: Option<ActorRef>,
    /// When.
    #[serde(with = "time::serde::rfc3339::option")]
    pub invited_at: Option<OffsetDateTime>,
    /// The group's owners.
    pub owners: Vec<ActorRef>,
}

#[derive(FromRow)]
struct InvitationRow {
    conversation_id: Uuid,
    name: String,
    description: String,
    actor_kind: String,
    actor_id: String,
    invited_by_kind: Option<String>,
    invited_by_id: Option<String>,
    invited_at: Option<OffsetDateTime>,
}

#[derive(FromRow)]
struct MemberRow {
    actor_kind: String,
    actor_id: String,
}

#[derive(FromRow)]
struct GroupMemberRow {
    conversation_id: Uuid,
    actor_kind: String,
    actor_id: String,
}

fn reference(kind: &str, id: &str) -> ActorRef {
    ActorRef {
        actor_type: if kind == "silicon" {
            ActorType::Silicon
        } else {
            ActorType::Carbon
        },
        id: ActorId::from_storage_key(id.to_owned()),
    }
}

impl PostgresStore {
    /// Whether `actor` manages the group: an owner, or the custodian of a Silicon owner.
    ///
    /// # Errors
    /// Database failures.
    pub async fn manages_group(&self, actor: &ActorRef, id: Uuid) -> AppResult<bool> {
        Ok(sqlx::query_scalar(
            "SELECT EXISTS (
                SELECT 1 FROM group_owners o
                WHERE o.conversation_id = $1 AND (
                    (o.actor_kind = $2::text::actor_kind AND o.actor_id = $3)
                    OR (o.actor_kind = 'silicon' AND $2 = 'carbon' AND EXISTS (
                        SELECT 1 FROM accounts a WHERE a.uuid = o.actor_id AND a.custodian_uuid = $3)))
                  AND NOT EXISTS (SELECT 1 FROM accounts gone WHERE gone.uuid = o.actor_id AND gone.status = 'deleted'))",
        )
        .bind(id)
        .bind(actor.actor_type.as_str())
        .bind(actor.id.as_str())
        .fetch_one(self.pool())
        .await?)
    }

    /// Group settings, owners and members; pending invitations only for those who manage
    /// the group.
    ///
    /// # Errors
    /// Database failures.
    pub async fn group_details(
        &self,
        viewer: &ActorRef,
        id: Uuid,
    ) -> AppResult<Option<GroupDetails>> {
        Ok(self.groups_details(viewer, &[id]).await?.remove(&id))
    }

    /// [`Self::group_details`] for a page of conversations at once (five queries for the
    /// page, not up to five per row); conversations that are not groups are left out.
    ///
    /// # Errors
    /// Database failures.
    pub async fn groups_details(
        &self,
        viewer: &ActorRef,
        ids: &[Uuid],
    ) -> AppResult<HashMap<Uuid, GroupDetails>> {
        let mut details = HashMap::new();
        if ids.is_empty() {
            return Ok(details);
        }
        let groups: Vec<(Uuid, String, String, i64)> = sqlx::query_as(
            "SELECT conversation_id, name, description, version FROM groups
             WHERE conversation_id = ANY($1)",
        )
        .bind(ids)
        .fetch_all(self.pool())
        .await?;
        if groups.is_empty() {
            return Ok(details);
        }
        let ids: Vec<Uuid> = groups.iter().map(|(id, ..)| *id).collect();
        let rows = |sql: &'static str| {
            sqlx::query_as::<_, GroupMemberRow>(sql)
                .bind(&ids)
                .fetch_all(self.pool())
        };
        let owners = rows(
            "SELECT o.conversation_id, o.actor_kind::text AS actor_kind, o.actor_id FROM group_owners o
             WHERE o.conversation_id = ANY($1)
               AND NOT EXISTS (SELECT 1 FROM accounts a WHERE a.uuid = o.actor_id AND a.status = 'deleted')
             ORDER BY o.conversation_id, o.added_at, o.actor_id",
        )
        .await?;
        let members = rows(
            "SELECT DISTINCT e.conversation_id, e.actor_kind::text AS actor_kind, e.actor_id
             FROM eligible_group_members e
             WHERE e.conversation_id = ANY($1) ORDER BY 1, 2, 3",
        )
        .await?;
        let managed: Vec<Uuid> = sqlx::query_scalar(
            "SELECT DISTINCT o.conversation_id FROM group_owners o
             WHERE o.conversation_id = ANY($1) AND (
                    (o.actor_kind = $2::text::actor_kind AND o.actor_id = $3)
                    OR (o.actor_kind = 'silicon' AND $2 = 'carbon' AND EXISTS (
                        SELECT 1 FROM accounts a WHERE a.uuid = o.actor_id AND a.custodian_uuid = $3)))
               AND NOT EXISTS (SELECT 1 FROM accounts gone WHERE gone.uuid = o.actor_id AND gone.status = 'deleted')",
        )
        .bind(&ids)
        .bind(viewer.actor_type.as_str())
        .bind(viewer.id.as_str())
        .fetch_all(self.pool())
        .await?;
        let pending = if managed.is_empty() {
            Vec::new()
        } else {
            sqlx::query_as::<_, GroupMemberRow>(
                "SELECT i.conversation_id, i.actor_kind::text AS actor_kind, i.actor_id
                 FROM group_invitations i
                 WHERE i.conversation_id = ANY($1) AND i.status = 'pending'
                 ORDER BY i.conversation_id, i.invited_at, i.actor_id",
            )
            .bind(&managed)
            .fetch_all(self.pool())
            .await?
        };
        let group_by = |rows: Vec<GroupMemberRow>| {
            let mut grouped: HashMap<Uuid, Vec<ActorRef>> = HashMap::new();
            for row in rows {
                grouped
                    .entry(row.conversation_id)
                    .or_default()
                    .push(reference(&row.actor_kind, &row.actor_id));
            }
            grouped
        };
        let mut owners = group_by(owners);
        let mut members = group_by(members);
        let mut pending = group_by(pending);
        for (id, name, description, version) in groups {
            details.insert(
                id,
                GroupDetails {
                    settings: GroupSettings { name, description },
                    version,
                    owners: owners.remove(&id).unwrap_or_default(),
                    members: members.remove(&id).unwrap_or_default(),
                    pending_invitations: pending.remove(&id).unwrap_or_default(),
                },
            );
        }
        Ok(details)
    }

    /// Creates a group in the @accounts partition with the creator as first owner. Every
    /// invitee is a pending invitation except those that join at once.
    ///
    /// # Errors
    /// Invalid settings, conflicting retries, or durable invariant failures.
    pub async fn create_group(
        &self,
        creator: &ActorRef,
        mut settings: GroupSettings,
        mut invitees: Vec<Invitee>,
        key: &IdempotencyKey,
    ) -> AppResult<Conversation> {
        settings.validate()?;
        settings.validate_address()?;
        invitees.retain(|invitee| invitee.actor != *creator);
        invitees.sort_by(|a, b| {
            (a.actor.actor_type.as_str(), a.actor.id.as_str())
                .cmp(&(b.actor.actor_type.as_str(), b.actor.id.as_str()))
        });
        invitees.dedup_by(|a, b| a.actor == b.actor);
        let hash = request_hash(&(&settings, &invitees))?;
        let partition = OrganizationId::accounts();
        let mut anchors = vec![creator.clone()];
        anchors.extend(invitees.iter().map(|invitee| invitee.actor.clone()));
        let mut tx = self.pool().begin().await?;
        refresh_directory_in(&mut tx, &partition, &anchors).await?;
        let id = match claim(&mut tx, &partition, creator, "groups.create", key, &hash).await? {
            IdempotencyClaim::Replay(id) => id,
            IdempotencyClaim::Acquired => {
                let id = Uuid::now_v7();
                let digest = blake3::hash(id.as_bytes());
                sqlx::query("INSERT INTO conversations(id,organization_id,participant_set_hash,created_by_kind,created_by_id,is_group) VALUES($1,$2,$3,$4::text::actor_kind,$5,true)")
                    .bind(id).bind(partition.as_str()).bind(digest.as_bytes().as_slice())
                    .bind(creator.actor_type.as_str()).bind(creator.id.as_str())
                    .execute(&mut *tx).await.map_err(map_constraint_error)?;
                sqlx::query("INSERT INTO groups(conversation_id,organization_id,name,description) VALUES($1,$2,$3,$4)")
                    .bind(id).bind(partition.as_str()).bind(&settings.name).bind(&settings.description)
                    .execute(&mut *tx).await.map_err(map_constraint_error)?;
                sqlx::query("INSERT INTO group_owners(conversation_id,actor_kind,actor_id,added_by_id) VALUES($1,$2::text::actor_kind,$3,$3)")
                    .bind(id).bind(creator.actor_type.as_str()).bind(creator.id.as_str())
                    .execute(&mut *tx).await.map_err(map_constraint_error)?;
                let mut everyone = vec![Invitee {
                    actor: creator.clone(),
                    accepted: true,
                }];
                everyone.extend(invitees.iter().cloned());
                invite_in(&mut tx, &partition, id, creator, &everyone).await?;
                // Members of the new group (the creator, Silicons its custodian added) see it
                // appear; everyone else invited has an invitation waiting.
                let events: Vec<(ActorRef, ConversationChange)> = everyone
                    .iter()
                    .map(|invitee| {
                        let change = if invitee.accepted {
                            ConversationChange::Created
                        } else {
                            ConversationChange::Invited
                        };
                        (invitee.actor.clone(), change)
                    })
                    .collect();
                conversation_events::record_in(&mut tx, id, &events).await?;
                complete_group(&mut tx, creator, "groups.create", key, id, 201).await?;
                id
            }
        };
        sync_in(&mut tx, id).await?;
        tx.commit().await.map_err(map_constraint_error)?;
        self.get_conversation(creator, id).await
    }

    /// Changes name and description at an exact version (the caller manages the group).
    ///
    /// # Errors
    /// Stale versions, invalid settings, conflicting retries.
    pub async fn update_group(
        &self,
        actor: &ActorRef,
        id: Uuid,
        mut settings: GroupSettings,
        version: i64,
        key: &IdempotencyKey,
    ) -> AppResult<GroupDetails> {
        settings.validate()?;
        if version < 1 {
            return Err(AppError::validation(
                "If-Match must be a positive group version",
            ));
        }
        let hash = request_hash(&(id, &settings, version))?;
        let mut tx = self.pool().begin().await?;
        lock_group(&mut tx, id).await?;
        if acquired(&mut tx, actor, "groups.update", key, &hash).await? {
            let (name, description): (String, String) =
                sqlx::query_as("SELECT name, description FROM groups WHERE conversation_id=$1")
                    .bind(id)
                    .fetch_one(&mut *tx)
                    .await?;
            let changed = sqlx::query("UPDATE groups SET name=$2,description=$3,version=version+1 WHERE conversation_id=$1 AND version=$4")
                .bind(id).bind(&settings.name).bind(&settings.description).bind(version)
                .execute(&mut *tx).await?;
            if changed.rows_affected() != 1 {
                return Err(AppError::refused(
                    http::StatusCode::CONFLICT,
                    "conflict",
                    "group version changed; fetch the group and retry with its current version",
                )
                .with_hint("GET /api/v1/groups/{group} for its version, then PATCH again with If-Match set to it."));
            }
            // Members and pending invitees both see the name and the description.
            let mut readers = group_members_in(&mut tx, id).await?;
            readers.extend(group_invitees_in(&mut tx, id).await?);
            let mut events = Vec::new();
            if name != settings.name {
                events.extend(each(&readers, ConversationChange::Renamed, &[]));
            }
            if description != settings.description {
                events.extend(each(&readers, ConversationChange::DescriptionChanged, &[]));
            }
            conversation_events::record_in(&mut tx, id, &events).await?;
            complete_group(&mut tx, actor, "groups.update", key, id, 200).await?;
        }
        finish_group(&mut tx, id).await?;
        tx.commit().await.map_err(map_constraint_error)?;
        self.group_details(actor, id)
            .await?
            .ok_or(AppError::NotFound)
    }

    /// Invites accounts (the caller manages the group). Existing members stay as they are;
    /// a pending invitation of an account that may join at once is accepted.
    ///
    /// # Errors
    /// Conflicting retries or durable invariant failures.
    pub async fn invite_to_group(
        &self,
        actor: &ActorRef,
        id: Uuid,
        invitees: Vec<Invitee>,
        key: &IdempotencyKey,
    ) -> AppResult<GroupDetails> {
        let hash = request_hash(&(id, &invitees))?;
        let mut tx = self.pool().begin().await?;
        let partition = lock_group(&mut tx, id).await?;
        if acquired(&mut tx, actor, "groups.invite", key, &hash).await? {
            let anchors: Vec<ActorRef> = invitees.iter().map(|i| i.actor.clone()).collect();
            refresh_directory_in(&mut tx, &partition, &anchors).await?;
            let before = invitation_statuses_in(&mut tx, id).await?;
            invite_in(&mut tx, &partition, id, actor, &invitees).await?;
            bump(&mut tx, id).await?;
            // Only what actually changed: a new invitation, or a member who joined at once.
            let mut events: Vec<(ActorRef, ConversationChange)> = Vec::new();
            for invitee in &invitees {
                let change = match (status_of(&before, &invitee.actor), invitee.accepted) {
                    (None | Some("pending"), true) => ConversationChange::Added,
                    (None, false) => ConversationChange::Invited,
                    _ => continue,
                };
                events.push((invitee.actor.clone(), change));
            }
            if !events.is_empty() {
                let personal: Vec<ActorRef> =
                    events.iter().map(|(account, _)| account.clone()).collect();
                let members = group_members_in(&mut tx, id).await?;
                events.extend(each(
                    &members,
                    ConversationChange::MembersChanged,
                    &personal,
                ));
            }
            conversation_events::record_in(&mut tx, id, &events).await?;
            complete_group(&mut tx, actor, "groups.invite", key, id, 200).await?;
        }
        finish_group(&mut tx, id).await?;
        tx.commit().await.map_err(map_constraint_error)?;
        self.group_details(actor, id)
            .await?
            .ok_or(AppError::NotFound)
    }
}

impl PostgresStore {
    /// Removes members or cancels their invitations. Refused when it would leave the group
    /// without an owner.
    ///
    /// # Errors
    /// `last_owner` (409), conflicting retries, durable failures.
    pub async fn remove_from_group(
        &self,
        actor: &ActorRef,
        id: Uuid,
        members: &[ActorRef],
        key: &IdempotencyKey,
    ) -> AppResult<GroupDetails> {
        let hash = request_hash(&(id, members))?;
        let mut tx = self.pool().begin().await?;
        lock_group(&mut tx, id).await?;
        if acquired(&mut tx, actor, "groups.uninvite", key, &hash).await? {
            let before = invitation_statuses_in(&mut tx, id).await?;
            let mut events: Vec<(ActorRef, ConversationChange)> = Vec::new();
            let mut owners_changed = false;
            for member in members {
                sqlx::query("DELETE FROM group_invitations WHERE conversation_id=$1 AND actor_kind=$2::text::actor_kind AND actor_id=$3")
                    .bind(id).bind(member.actor_type.as_str()).bind(member.id.as_str())
                    .execute(&mut *tx).await?;
                owners_changed |= sqlx::query("DELETE FROM group_owners WHERE conversation_id=$1 AND actor_kind=$2::text::actor_kind AND actor_id=$3")
                    .bind(id).bind(member.actor_type.as_str()).bind(member.id.as_str())
                    .execute(&mut *tx).await?.rows_affected() > 0;
                match status_of(&before, member) {
                    Some("accepted") => events.push((member.clone(), ConversationChange::Removed)),
                    Some("pending") => {
                        events.push((member.clone(), ConversationChange::InvitationCancelled));
                    }
                    _ => {}
                }
            }
            require_an_owner(&mut tx, id).await?;
            bump(&mut tx, id).await?;
            events.extend(
                changed_for_the_rest(&mut tx, id, !events.is_empty(), owners_changed).await?,
            );
            conversation_events::record_in(&mut tx, id, &events).await?;
            complete_group(&mut tx, actor, "groups.uninvite", key, id, 200).await?;
        }
        finish_group(&mut tx, id).await?;
        tx.commit().await.map_err(map_constraint_error)?;
        self.group_details(actor, id)
            .await?
            .ok_or(AppError::NotFound)
    }

    /// The invitee accepts (joins with full history) or declines (the invitation goes).
    ///
    /// # Errors
    /// Not found when there is no pending invitation for this account.
    pub async fn respond_to_invitation(
        &self,
        invitee: &ActorRef,
        id: Uuid,
        accept: bool,
    ) -> AppResult<()> {
        let mut tx = self.pool().begin().await?;
        lock_group(&mut tx, id).await?;
        let changed = if accept {
            sqlx::query("UPDATE group_invitations SET status='accepted', accepted_at=clock_timestamp() WHERE conversation_id=$1 AND actor_kind=$2::text::actor_kind AND actor_id=$3 AND status='pending'")
        } else {
            sqlx::query("DELETE FROM group_invitations WHERE conversation_id=$1 AND actor_kind=$2::text::actor_kind AND actor_id=$3 AND status='pending'")
        }
        .bind(id)
        .bind(invitee.actor_type.as_str())
        .bind(invitee.id.as_str())
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if changed != 1 {
            return Err(AppError::refused(
                http::StatusCode::NOT_FOUND,
                "invitation_not_found",
                "There is no pending invitation to this group for this account (it may have been accepted, declined or cancelled already).",
            ));
        }
        if accept {
            // Joining reads the whole history, but none of it is new to the joiner.
            super::read_states::start_reading_at_newest_in(&mut tx, id, invitee).await?;
        }
        bump(&mut tx, id).await?;
        finish_group(&mut tx, id).await?;
        let mut events = vec![(
            invitee.clone(),
            if accept {
                ConversationChange::InvitationAccepted
            } else {
                ConversationChange::InvitationDeclined
            },
        )];
        let members = group_members_in(&mut tx, id).await?;
        events.extend(each(
            &members,
            ConversationChange::MembersChanged,
            std::slice::from_ref(invitee),
        ));
        conversation_events::record_in(&mut tx, id, &events).await?;
        tx.commit().await.map_err(map_constraint_error)?;
        Ok(())
    }

    /// A member leaves. The last owner cannot leave (make someone else an owner first).
    ///
    /// # Errors
    /// `last_owner` (409), not a member (404).
    pub async fn leave_group(&self, member: &ActorRef, id: Uuid) -> AppResult<()> {
        let mut tx = self.pool().begin().await?;
        lock_group(&mut tx, id).await?;
        let left = sqlx::query("DELETE FROM group_invitations WHERE conversation_id=$1 AND actor_kind=$2::text::actor_kind AND actor_id=$3 AND status='accepted'")
            .bind(id).bind(member.actor_type.as_str()).bind(member.id.as_str())
            .execute(&mut *tx).await?.rows_affected();
        if left != 1 {
            return Err(AppError::NotFound);
        }
        let was_owner = sqlx::query("DELETE FROM group_owners WHERE conversation_id=$1 AND actor_kind=$2::text::actor_kind AND actor_id=$3")
            .bind(id).bind(member.actor_type.as_str()).bind(member.id.as_str())
            .execute(&mut *tx).await?.rows_affected() > 0;
        require_an_owner(&mut tx, id).await?;
        bump(&mut tx, id).await?;
        finish_group(&mut tx, id).await?;
        let mut events = vec![(member.clone(), ConversationChange::Left)];
        events.extend(changed_for_the_rest(&mut tx, id, true, was_owner).await?);
        conversation_events::record_in(&mut tx, id, &events).await?;
        tx.commit().await.map_err(map_constraint_error)?;
        Ok(())
    }

    /// Promotes accepted members to owners, or demotes owners (never the last one).
    ///
    /// # Errors
    /// `not_a_member` (409) for promoting a non-member, `last_owner` (409).
    pub async fn set_group_owners(
        &self,
        actor: &ActorRef,
        id: Uuid,
        members: &[ActorRef],
        promote: bool,
        key: &IdempotencyKey,
    ) -> AppResult<GroupDetails> {
        let operation = if promote {
            "groups.owners.add"
        } else {
            "groups.owners.remove"
        };
        let hash = request_hash(&(id, members))?;
        let mut tx = self.pool().begin().await?;
        lock_group(&mut tx, id).await?;
        if acquired(&mut tx, actor, operation, key, &hash).await? {
            let mut changed = false;
            for member in members {
                if promote {
                    let is_member: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM eligible_group_members WHERE conversation_id=$1 AND actor_kind=$2::text::actor_kind AND actor_id=$3)")
                        .bind(id).bind(member.actor_type.as_str()).bind(member.id.as_str())
                        .fetch_one(&mut *tx).await?;
                    if !is_member {
                        return Err(AppError::refused(
                            http::StatusCode::CONFLICT,
                            "not_a_member",
                            "Only members who accepted their invitation can become owners.",
                        ));
                    }
                    changed |= sqlx::query("INSERT INTO group_owners(conversation_id,actor_kind,actor_id,added_by_id) VALUES($1,$2::text::actor_kind,$3,$4) ON CONFLICT DO NOTHING")
                        .bind(id).bind(member.actor_type.as_str()).bind(member.id.as_str()).bind(actor.id.as_str())
                        .execute(&mut *tx).await?.rows_affected() > 0;
                } else {
                    changed |= sqlx::query("DELETE FROM group_owners WHERE conversation_id=$1 AND actor_kind=$2::text::actor_kind AND actor_id=$3")
                        .bind(id).bind(member.actor_type.as_str()).bind(member.id.as_str())
                        .execute(&mut *tx).await?.rows_affected() > 0;
                }
            }
            require_an_owner(&mut tx, id).await?;
            bump(&mut tx, id).await?;
            let events = changed_for_the_rest(&mut tx, id, false, changed).await?;
            conversation_events::record_in(&mut tx, id, &events).await?;
            complete_group(&mut tx, actor, operation, key, id, 200).await?;
        }
        finish_group(&mut tx, id).await?;
        tx.commit().await.map_err(map_constraint_error)?;
        self.group_details(actor, id)
            .await?
            .ok_or(AppError::NotFound)
    }

    /// Pending invitations of these accounts (the caller, or a Silicon it looks after).
    ///
    /// # Errors
    /// Database failures.
    pub async fn invitations_for(&self, invitee: &ActorRef) -> AppResult<Vec<Invitation>> {
        let rows: Vec<InvitationRow> = sqlx::query_as(
            "SELECT i.conversation_id, g.name, g.description, i.actor_kind::text AS actor_kind, i.actor_id,
                    i.invited_by_kind::text AS invited_by_kind, i.invited_by_id, i.invited_at
             FROM group_invitations i JOIN groups g USING (conversation_id)
             WHERE i.actor_kind = $1::text::actor_kind AND i.actor_id = $2 AND i.status = 'pending'
             ORDER BY i.invited_at, i.conversation_id",
        )
        .bind(invitee.actor_type.as_str())
        .bind(invitee.id.as_str())
        .fetch_all(self.pool())
        .await?;
        let mut invitations = Vec::with_capacity(rows.len());
        for row in rows {
            let owners: Vec<MemberRow> = sqlx::query_as(
                "SELECT actor_kind::text AS actor_kind, actor_id FROM group_owners WHERE conversation_id = $1 ORDER BY added_at, actor_id",
            )
            .bind(row.conversation_id)
            .fetch_all(self.pool())
            .await?;
            invitations.push(Invitation {
                conversation_id: row.conversation_id,
                name: row.name,
                description: row.description,
                invitee: reference(&row.actor_kind, &row.actor_id),
                invited_by: row
                    .invited_by_kind
                    .zip(row.invited_by_id)
                    .map(|(kind, id)| reference(&kind, &id)),
                invited_at: row.invited_at,
                owners: owners
                    .iter()
                    .map(|owner| reference(&owner.actor_kind, &owner.actor_id))
                    .collect(),
            });
        }
        Ok(invitations)
    }
}

/// Each account's invitation status in a group (`pending` or `accepted`).
async fn invitation_statuses_in(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
) -> AppResult<Vec<(String, String, String)>> {
    Ok(sqlx::query_as(
        "SELECT actor_kind::text, actor_id, status::text FROM group_invitations WHERE conversation_id=$1",
    )
    .bind(id)
    .fetch_all(&mut **tx)
    .await?)
}

fn status_of<'a>(statuses: &'a [(String, String, String)], account: &ActorRef) -> Option<&'a str> {
    statuses
        .iter()
        .find(|(kind, id, _)| kind == account.actor_type.as_str() && id == account.id.as_str())
        .map(|(_, _, status)| status.as_str())
}

/// The events of the group's remaining accounts after a change: `members_changed` for the
/// members, `owners_changed` for members and pending invitees (both see the owners).
async fn changed_for_the_rest(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    members_changed: bool,
    owners_changed: bool,
) -> AppResult<Vec<(ActorRef, ConversationChange)>> {
    let mut events = Vec::new();
    if !members_changed && !owners_changed {
        return Ok(events);
    }
    let members = group_members_in(tx, id).await?;
    if members_changed {
        events.extend(each(&members, ConversationChange::MembersChanged, &[]));
    }
    if owners_changed {
        events.extend(each(&members, ConversationChange::OwnersChanged, &[]));
        let invitees = group_invitees_in(tx, id).await?;
        events.extend(each(&invitees, ConversationChange::OwnersChanged, &[]));
    }
    Ok(events)
}

/// Locks the group and returns its storage partition. The directory's snapshot rows come
/// first, in the order a send takes them (the conversation's partition, then @accounts),
/// then the conversation and the group: a send holds the directory while its message waits
/// for the conversation row, so a group change that locked the row first and then refreshed
/// the directory (every change records its actor) deadlocked against it.
async fn lock_group(tx: &mut Transaction<'_, Postgres>, id: Uuid) -> AppResult<OrganizationId> {
    let partition: Option<String> = sqlx::query_scalar(
        "SELECT c.organization_id FROM conversations c JOIN groups g ON g.conversation_id=c.id WHERE c.id=$1",
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?;
    let partition: OrganizationId = partition
        .ok_or(AppError::NotFound)?
        .parse()
        .map_err(|error| super::rows::data_error("group partition", error))?;
    refresh_directory_in(tx, &partition, &[]).await?;
    refresh_directory_in(tx, &OrganizationId::accounts(), &[]).await?;
    // NO KEY UPDATE: group changes still queue behind each other and behind sends (whose
    // UPDATE of the conversation takes the same lock), but not behind the foreign-key checks
    // (KEY SHARE) of writers that already hold an account's stream: a speech announcement or
    // a receipt inserting a Ting handoff, which references the conversation. FOR UPDATE
    // conflicted with those and deadlocked against the change's own conversation events.
    sqlx::query(
        "SELECT 1 FROM conversations c JOIN groups g ON g.conversation_id=c.id WHERE c.id=$1 FOR NO KEY UPDATE OF c,g",
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(AppError::NotFound)?;
    Ok(partition)
}

async fn acquired(
    tx: &mut Transaction<'_, Postgres>,
    actor: &ActorRef,
    operation: &str,
    key: &IdempotencyKey,
    hash: &[u8; 32],
) -> AppResult<bool> {
    let accounts = OrganizationId::accounts();
    refresh_directory_in(tx, &accounts, std::slice::from_ref(actor)).await?;
    Ok(matches!(
        claim(tx, &accounts, actor, operation, key, hash).await?,
        IdempotencyClaim::Acquired
    ))
}

async fn invite_in(
    tx: &mut Transaction<'_, Postgres>,
    partition: &OrganizationId,
    id: Uuid,
    inviter: &ActorRef,
    invitees: &[Invitee],
) -> AppResult<()> {
    refresh_directory_in(tx, partition, std::slice::from_ref(inviter)).await?;
    for invitee in invitees {
        let status = if invitee.accepted {
            "accepted"
        } else {
            "pending"
        };
        let member_before: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM group_invitations WHERE conversation_id=$1 AND actor_kind=$2::text::actor_kind AND actor_id=$3 AND status='accepted')",
        )
        .bind(id)
        .bind(invitee.actor.actor_type.as_str())
        .bind(invitee.actor.id.as_str())
        .fetch_one(&mut **tx)
        .await?;
        sqlx::query(
            "INSERT INTO group_invitations(conversation_id,organization_id,actor_kind,actor_id,status,invited_by_kind,invited_by_id,invited_at,accepted_at)
             VALUES($1,$2,$3::text::actor_kind,$4,$5,$6::text::actor_kind,$7,clock_timestamp(),CASE WHEN $5='accepted' THEN clock_timestamp() END)
             ON CONFLICT (conversation_id,actor_kind,actor_id) DO UPDATE SET
                status = CASE WHEN group_invitations.status = 'accepted' OR EXCLUDED.status = 'accepted' THEN 'accepted' ELSE 'pending' END,
                accepted_at = COALESCE(group_invitations.accepted_at, EXCLUDED.accepted_at)",
        )
        .bind(id)
        .bind(partition.as_str())
        .bind(invitee.actor.actor_type.as_str())
        .bind(invitee.actor.id.as_str())
        .bind(status)
        .bind(inviter.actor_type.as_str())
        .bind(inviter.id.as_str())
        .execute(&mut **tx)
        .await
        .map_err(map_constraint_error)?;
        if invitee.accepted && !member_before {
            // Joined at once (the creator, or a Silicon its custodian added): what was said
            // before is history, not unread.
            super::read_states::start_reading_at_newest_in(tx, id, &invitee.actor).await?;
        }
    }
    Ok(())
}

async fn complete_group(
    tx: &mut Transaction<'_, Postgres>,
    actor: &ActorRef,
    op: &str,
    key: &IdempotencyKey,
    id: Uuid,
    status: u16,
) -> AppResult<()> {
    complete(
        tx,
        &OrganizationId::accounts(),
        actor,
        op,
        key,
        IdempotencyResult {
            resource_type: "conversation",
            resource_id: id,
            response_status: status,
        },
    )
    .await
}

async fn bump(tx: &mut Transaction<'_, Postgres>, id: Uuid) -> AppResult<()> {
    sqlx::query("UPDATE groups SET version=version+1 WHERE conversation_id=$1")
        .bind(id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn sync_in(tx: &mut Transaction<'_, Postgres>, id: Uuid) -> AppResult<()> {
    sqlx::query("SELECT sync_group_conversation($1)")
        .bind(id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn finish_group(tx: &mut Transaction<'_, Postgres>, id: Uuid) -> AppResult<()> {
    sync_in(tx, id).await?;
    sqlx::query("UPDATE conversations SET updated_at=clock_timestamp() WHERE id=$1")
        .bind(id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn require_an_owner(tx: &mut Transaction<'_, Postgres>, id: Uuid) -> AppResult<()> {
    let owners: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM group_owners o WHERE o.conversation_id=$1
         AND NOT EXISTS (SELECT 1 FROM accounts a WHERE a.uuid = o.actor_id AND a.status = 'deleted')",
    )
    .bind(id)
    .fetch_one(&mut **tx)
    .await?;
    if owners == 0 {
        return Err(AppError::refused(
            http::StatusCode::CONFLICT,
            "last_owner",
            "A group always keeps at least one owner. Make another member an owner first (POST /api/v1/groups/{group}/owners).",
        ));
    }
    Ok(())
}

/// After an account was deleted: if the group lost its last owner, its longest-standing
/// member becomes owner (nothing happens to a group with no members left).
pub(crate) async fn ensure_owner_in(tx: &mut Transaction<'_, Postgres>, id: Uuid) -> AppResult<()> {
    let has_owner: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM group_owners o WHERE o.conversation_id = $1
            AND NOT EXISTS (SELECT 1 FROM accounts a WHERE a.uuid = o.actor_id AND a.status = 'deleted'))",
    )
    .bind(id)
    .fetch_one(&mut **tx)
    .await?;
    if has_owner {
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO group_owners (conversation_id, actor_kind, actor_id)
         SELECT e.conversation_id, e.actor_kind, e.actor_id
         FROM eligible_group_members e
         LEFT JOIN group_invitations i USING (conversation_id, actor_kind, actor_id)
         LEFT JOIN conversation_participants p USING (conversation_id, actor_kind, actor_id)
         WHERE e.conversation_id = $1
         ORDER BY COALESCE(i.accepted_at, i.invited_at, p.added_at) NULLS LAST, e.actor_id
         LIMIT 1
         ON CONFLICT DO NOTHING",
    )
    .bind(id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}
