//! Scoped IAM membership projections; these never authorize a caller's token.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;
use silicon_iam_client::models::{ApplicationAuthorization, WebhookEvent};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::{
    AppError, AppResult,
    domain::{ActorId, ActorRef, ActorType, OrganizationId},
};

use super::{PostgresStore, directory::refresh_directory_in};

type AppliedProjection = (Option<String>, Option<String>, ActorType, i64, String);

struct Projection {
    membership_id: String,
    iam_membership_id: Option<Uuid>,
    iam_organization_id: Uuid,
    organization_id: Option<String>,
    actor_type: ActorType,
    actor_id: Option<String>,
    version: i64,
    epoch: Option<i64>,
    removed: bool,
    tag_ids: Option<Vec<Uuid>>,
}

impl PostgresStore {
    /// Persists only a complete, already cross-validated live IAM snapshot.
    ///
    /// # Errors
    /// Rejects conflicting identities, revoked projections, or database failure.
    pub async fn project_iam_authorization(
        &self,
        snapshot: &ApplicationAuthorization,
        actor: &ActorRef,
    ) -> AppResult<()> {
        let projection = Projection {
            membership_id: format!("{}[{}]", actor.id, snapshot.org_id),
            iam_membership_id: Uuid::parse_str(&snapshot.membership_id).ok(),
            iam_organization_id: snapshot.organization_id,
            organization_id: Some(snapshot.org_id.clone()),
            actor_type: actor.actor_type,
            actor_id: Some(actor.id.to_string()),
            version: snapshot.membership_version,
            epoch: Some(snapshot.authorization_epoch),
            removed: false,
            tag_ids: snapshot
                .tags
                .as_ref()
                .map(|tags| tags.iter().map(|tag| tag.id).collect()),
        };
        let mut tx = self.pool().begin().await?;
        project_member(&mut tx, &projection).await?;
        // If a newer event won while introspection was in flight, its old role
        // or scope snapshot must not escape into the request's authorization.
        let active: bool = sqlx::query_scalar("SELECT status = 'active' AND iam_organization_id=$2 AND actor_id=$3 AND iam_version=$4 AND authorization_epoch=$5 AND actor_kind=$6::text::actor_kind AND organization_id=$7 FROM iam_membership_projections WHERE membership_public_id = $1")
            .bind(&projection.membership_id).bind(snapshot.organization_id).bind(actor.id.as_str())
            .bind(snapshot.membership_version).bind(snapshot.authorization_epoch).bind(actor.actor_type.as_str()).bind(&snapshot.org_id)
            .fetch_one(&mut *tx).await?;
        if active {
            // Live organization-bound introspection proves current organization
            // authority without disclosing a new organization resource version.
            sqlx::query("UPDATE organization_snapshots SET status='active',refreshed_at=clock_timestamp() WHERE organization_id=$1")
                .bind(&snapshot.org_id).execute(&mut *tx).await?;
            sqlx::query("SELECT sync_group_participants($1)")
                .bind(&snapshot.org_id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        if active {
            Ok(())
        } else {
            Err(AppError::Unauthorized)
        }
    }

    /// Resolves previously disclosed active recipients in the caller's organization.
    ///
    /// # Errors
    /// Missing, removed, or ambiguous identities fail closed; database errors propagate.
    pub async fn resolve_iam_participants(
        &self,
        organization_id: &OrganizationId,
        requested: &BTreeSet<ActorId>,
    ) -> AppResult<Vec<ActorRef>> {
        let ids: Vec<&str> = requested.iter().map(ActorId::as_str).collect();
        let rows: Vec<(ActorType, String)> = sqlx::query_as("SELECT member.actor_kind, member.actor_id FROM iam_membership_projections member JOIN organization_snapshots org USING (organization_id) WHERE member.organization_id = $1 AND member.actor_id = ANY($2) AND member.status = 'active' AND org.status = 'active'")
            .bind(organization_id.as_str()).bind(ids).fetch_all(self.pool()).await?;
        let mut resolved = BTreeMap::new();
        for (actor_type, id) in rows {
            let id: ActorId = id.parse().map_err(|_| invalid_projection())?;
            if resolved
                .insert(id.clone(), ActorRef { actor_type, id })
                .is_some()
            {
                return Err(AppError::Forbidden);
            }
        }
        if resolved.len() != requested.len() {
            return Err(AppError::validation(
                "IAM has not supplied an active membership for this participant to DM; the recipient can sign in to refresh their membership",
            ));
        }
        Ok(resolved.into_values().collect())
    }

    /// Applies only signed IAM member projections in the webhook receipt transaction.
    ///
    /// # Errors
    /// Malformed known projections fail without acknowledging the event.
    pub async fn project_iam_webhook(
        tx: &mut Transaction<'_, Postgres>,
        event: &WebhookEvent,
    ) -> AppResult<()> {
        // Unknown event schemas are acknowledged, but cannot change authority.
        if !known_member_event(&event.event_type) {
            return Ok(());
        }
        let Some(current) = event.data.get("current") else {
            return Ok(());
        };
        if let Some(members) = current.get("members") {
            let members = members.as_array().ok_or_else(invalid_projection)?;
            for member in members {
                if let Some(projection) = parse_member(event, member)? {
                    project_member(tx, &projection).await?;
                }
            }
        }
        if let Some(organization) = current.get("organization") {
            project_organization(tx, organization).await?;
        }
        Ok(())
    }
}

fn parse_member(event: &WebhookEvent, member: &Value) -> AppResult<Option<Projection>> {
    let resource = member.get("resource").ok_or_else(invalid_projection)?;
    if resource.get("type").and_then(Value::as_str) != Some("organization_membership")
        || !matches!(
            resource.get("status").and_then(Value::as_str),
            Some("active" | "removed")
        )
    {
        return Err(invalid_projection());
    }
    let removed = member.get("authorization").and_then(Value::as_str) == Some("removed")
        || resource.get("status").and_then(Value::as_str) == Some("removed");
    let epoch = member
        .pointer("/membership/authorization_epoch")
        .and_then(Value::as_i64)
        .filter(|value| *value >= 0);
    let organization_id = member
        .pointer("/organization/org_id")
        .and_then(Value::as_str)
        .map(str::parse::<OrganizationId>)
        .transpose()
        .map_err(|_| invalid_projection())?
        .map(String::from);
    let actor_id = member
        .pointer("/principal/public_id")
        .and_then(Value::as_str)
        .map(str::parse::<ActorId>)
        .transpose()
        .map_err(|_| invalid_projection())?
        .map(String::from);
    validate_public_membership(resource, actor_id.as_deref(), organization_id.as_deref())?;
    // Scope-filtered profiles without membership disclosure convey no active authority.
    if !removed && (epoch.is_none() || organization_id.is_none() || actor_id.is_none()) {
        return Ok(None);
    }
    let actor_type = resource
        .get("principal_type")
        .or_else(|| resource.get("actor_type"))
        .and_then(Value::as_str)
        .ok_or_else(invalid_projection)?
        .parse()
        .map_err(|_| invalid_projection())?;
    let iam_organization_id = event
        .organization_id
        .filter(|id| !id.is_nil())
        .ok_or_else(invalid_projection)?;
    if member
        .pointer("/organization/id")
        .and_then(Value::as_str)
        .is_some_and(|id| Uuid::parse_str(id).ok() != Some(iam_organization_id))
    {
        return Err(invalid_projection());
    }
    Ok(Some(Projection {
        membership_id: resource
            .get("membership_id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| {
                actor_id
                    .as_ref()
                    .zip(organization_id.as_ref())
                    .map(|(actor, org)| format!("{actor}[{org}]"))
            })
            .unwrap_or_else(|| resource["id"].as_str().unwrap_or_default().to_owned()),
        iam_membership_id: Some(required_uuid(resource, "id")?),
        iam_organization_id,
        organization_id,
        actor_type,
        actor_id,
        version: resource
            .get("version")
            .and_then(Value::as_i64)
            .filter(|value| *value > 0)
            .ok_or_else(invalid_projection)?,
        epoch,
        removed,
        tag_ids: member
            .pointer("/membership/tags")
            .filter(|value| !value.is_null())
            .map(|value| {
                value
                    .as_array()
                    .ok_or_else(invalid_projection)?
                    .iter()
                    .map(|tag| required_uuid(tag, "id"))
                    .collect::<AppResult<Vec<_>>>()
            })
            .transpose()?,
    }))
}

fn validate_public_membership(
    resource: &Value,
    actor_id: Option<&str>,
    organization_id: Option<&str>,
) -> AppResult<()> {
    if let Some(public_membership) = resource.get("membership_id") {
        let value = public_membership.as_str().ok_or_else(invalid_projection)?;
        let (actor, org) = value
            .strip_suffix(']')
            .and_then(|value| value.split_once('['))
            .ok_or_else(invalid_projection)?;
        actor.parse::<ActorId>().map_err(|_| invalid_projection())?;
        org.parse::<OrganizationId>()
            .map_err(|_| invalid_projection())?;
        if actor_id.is_some_and(|known| known != actor)
            || organization_id.is_some_and(|known| known != org)
        {
            return Err(invalid_projection());
        }
    }
    Ok(())
}

async fn project_member(
    tx: &mut Transaction<'_, Postgres>,
    projection: &Projection,
) -> AppResult<()> {
    // Lock serializes independently delivered events and online introspection.
    // Canonical identity bindings are immutable. Equal-version removals win over active state.
    // A narrower credential does not erase known tags at the exact same IAM
    // version/epoch. Fresh token checks independently enforce its disclosure;
    // undisclosed tags at a newer authority version invalidate the cached grant.
    let public_id = projection
        .membership_id
        .ends_with(']')
        .then_some(&projection.membership_id);
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(&projection.membership_id)
        .execute(&mut **tx)
        .await?;
    let existing: Vec<Uuid> = sqlx::query_scalar(
        "SELECT membership_id FROM iam_membership_projections WHERE membership_public_id=$1 OR iam_membership_id=$2 OR membership_id=$2 OR (iam_organization_id=$3 AND actor_id=$4 AND organization_id=$5) FOR UPDATE"
    ).bind(public_id).bind(projection.iam_membership_id).bind(projection.iam_organization_id).bind(&projection.actor_id).bind(&projection.organization_id).fetch_all(&mut **tx).await?;
    if existing.len() > 1 {
        return Err(invalid_projection());
    }
    let row_id = existing
        .first()
        .copied()
        .or(projection.iam_membership_id)
        .unwrap_or_else(Uuid::new_v4);
    let row: Option<AppliedProjection> = sqlx::query_as(
        "INSERT INTO iam_membership_projections (membership_id,membership_public_id,iam_organization_id,organization_id,actor_kind,actor_id,iam_version,authorization_epoch,status,tag_ids,iam_membership_id)
         VALUES ($1,$2,$3,$4,$5::text::actor_kind,$6,$7,$8,$9,$10,$11)
         ON CONFLICT (membership_id) DO UPDATE SET
           membership_public_id = COALESCE(EXCLUDED.membership_public_id, iam_membership_projections.membership_public_id),
           iam_membership_id = COALESCE(EXCLUDED.iam_membership_id, iam_membership_projections.iam_membership_id),
           organization_id = COALESCE(EXCLUDED.organization_id, iam_membership_projections.organization_id),
           actor_id = COALESCE(EXCLUDED.actor_id, iam_membership_projections.actor_id),
           iam_version = EXCLUDED.iam_version,
           authorization_epoch = COALESCE(EXCLUDED.authorization_epoch, iam_membership_projections.authorization_epoch),
           tag_ids = CASE
             WHEN EXCLUDED.tag_ids IS NOT NULL THEN EXCLUDED.tag_ids
             WHEN iam_membership_projections.iam_version = EXCLUDED.iam_version
               AND iam_membership_projections.authorization_epoch = EXCLUDED.authorization_epoch
             THEN iam_membership_projections.tag_ids
             ELSE NULL END,
           status = EXCLUDED.status, refreshed_at = clock_timestamp()
         WHERE (iam_membership_projections.membership_public_id IS NULL OR EXCLUDED.membership_public_id IS NULL OR iam_membership_projections.membership_public_id = EXCLUDED.membership_public_id)
           AND (iam_membership_projections.iam_membership_id IS NULL OR EXCLUDED.iam_membership_id IS NULL OR iam_membership_projections.iam_membership_id = EXCLUDED.iam_membership_id)
           AND iam_membership_projections.iam_organization_id = EXCLUDED.iam_organization_id
           AND iam_membership_projections.actor_kind = EXCLUDED.actor_kind
           AND (iam_membership_projections.organization_id IS NULL OR EXCLUDED.organization_id IS NULL OR iam_membership_projections.organization_id = EXCLUDED.organization_id)
           AND (iam_membership_projections.actor_id IS NULL OR EXCLUDED.actor_id IS NULL OR iam_membership_projections.actor_id = EXCLUDED.actor_id)
           AND (iam_membership_projections.authorization_epoch IS NULL OR EXCLUDED.authorization_epoch IS NULL OR EXCLUDED.authorization_epoch >= iam_membership_projections.authorization_epoch)
           AND (EXCLUDED.iam_version > iam_membership_projections.iam_version
             OR (EXCLUDED.iam_version = iam_membership_projections.iam_version
               AND (EXCLUDED.status = 'removed' OR (iam_membership_projections.status = 'active'
                 AND EXCLUDED.authorization_epoch >= iam_membership_projections.authorization_epoch))))
         RETURNING organization_id, actor_id, actor_kind, iam_version, status")
        .bind(row_id).bind(public_id).bind(projection.iam_organization_id)
        .bind(&projection.organization_id).bind(projection.actor_type.as_str()).bind(&projection.actor_id)
        .bind(projection.version).bind(projection.epoch).bind(if projection.removed { "removed" } else { "active" }).bind(&projection.tag_ids).bind(projection.iam_membership_id)
        .fetch_optional(&mut **tx).await?;
    let Some((Some(org), Some(actor_id), actor_type, version, status)) = row else {
        return Ok(());
    };
    let org: OrganizationId = org.parse().map_err(|_| invalid_projection())?;
    let actor = ActorRef {
        actor_type,
        id: actor_id.parse().map_err(|_| invalid_projection())?,
    };
    refresh_directory_in(tx, &org, std::slice::from_ref(&actor)).await?;
    sqlx::query("UPDATE actor_snapshots SET status = $4::text::snapshot_status, iam_version = $5, refreshed_at = clock_timestamp() WHERE organization_id = $1 AND actor_kind = $2::text::actor_kind AND actor_id = $3 AND iam_version <= $5")
        .bind(org.as_str()).bind(actor_type.as_str()).bind(actor.id.as_str())
        .bind(if status == "active" { "active" } else { "deleted" }).bind(version).execute(&mut **tx).await?;
    sqlx::query("SELECT sync_group_participants($1)")
        .bind(org.as_str())
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn project_organization(
    tx: &mut Transaction<'_, Postgres>,
    organization: &Value,
) -> AppResult<()> {
    let removed = organization.get("authorization").and_then(Value::as_str) == Some("removed");
    let org_id = organization.get("org_id").and_then(Value::as_str);
    let version = organization
        .get("version")
        .and_then(Value::as_i64)
        .filter(|value| *value > 0)
        .ok_or_else(invalid_projection)?;
    let status = if removed {
        "deleted"
    } else {
        match organization.get("status").and_then(Value::as_str) {
            Some("suspended") => "suspended",
            Some("removed" | "deleted") => "deleted",
            Some("active") | None => "active",
            _ => return Err(invalid_projection()),
        }
    };
    if let Some(org_id) = org_id {
        let org: OrganizationId = org_id.parse().map_err(|_| invalid_projection())?;
        sqlx::query("INSERT INTO organization_snapshots (organization_id,status,iam_version) VALUES ($1,$2::text::snapshot_status,$3) ON CONFLICT (organization_id) DO UPDATE SET status=EXCLUDED.status,iam_version=EXCLUDED.iam_version,refreshed_at=clock_timestamp() WHERE organization_snapshots.iam_version < EXCLUDED.iam_version")
            .bind(org.as_str()).bind(status).bind(version).execute(&mut **tx).await?;
        sqlx::query("SELECT sync_group_participants($1)")
            .bind(org.as_str())
            .execute(&mut **tx)
            .await?;
    }
    Ok(())
}

fn required_uuid(value: &Value, field: &str) -> AppResult<Uuid> {
    value
        .get(field)
        .and_then(Value::as_str)
        .and_then(|value| Uuid::parse_str(value).ok())
        .filter(|id| !id.is_nil())
        .ok_or_else(invalid_projection)
}

fn invalid_projection() -> AppError {
    AppError::validation("IAM supplied an invalid membership projection")
}

fn known_member_event(event_type: &str) -> bool {
    matches!(
        event_type,
        "organization.updated.v1"
            | "organization.ownership_transferred.v1"
            | "organization.membership.created.v1"
            | "organization.membership.reactivated.v1"
            | "organization.membership.removed.v1"
            | "organization.membership.updated.v1"
            | "organization.membership.profile_updated.v1"
            | "organization.membership.authorization_updated.v1"
            | "organization.admin.promoted.v1"
            | "organization.admin.demoted.v1"
            | "organization.silicon.created.v1"
            | "organization.silicon.updated.v1"
            | "organization.silicon.removed.v1"
            | "organization.silicon.credential_rotated.v1"
            | "organization.tag_updated.v1"
            | "organization.tag_archived.v1"
            | "organization.trust.default_updated.v1"
            | "organization.trust.rule_created.v1"
            | "organization.trust.rule_updated.v1"
            | "organization.trust.rule_archived.v1"
    )
}
