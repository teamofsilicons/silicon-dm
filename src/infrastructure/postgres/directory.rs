//! Minimal IAM directory snapshots used for tenant-safe foreign keys.

use sqlx::{Postgres, Transaction};

use crate::{
    AppResult,
    domain::{ActorRef, OrganizationId},
};

use super::{PostgresStore, map_constraint_error};

impl PostgresStore {
    /// Refreshes minimal active IAM snapshots for already-authorized actors.
    ///
    /// IAM remains authoritative; this method must only receive actor refs from
    /// a successful IAM authorization decision.
    ///
    /// # Errors
    ///
    /// Returns a durable-constraint or database error.
    pub async fn refresh_directory(
        &self,
        organization_id: &OrganizationId,
        actors: &[ActorRef],
    ) -> AppResult<()> {
        let mut transaction = self.pool().begin().await?;
        refresh_directory_in(&mut transaction, organization_id, actors).await?;
        transaction.commit().await?;
        Ok(())
    }
}

pub(crate) async fn refresh_directory_in(
    transaction: &mut Transaction<'_, Postgres>,
    organization_id: &OrganizationId,
    actors: &[ActorRef],
) -> AppResult<()> {
    sqlx::query(
        r#"
        INSERT INTO organization_snapshots (
            organization_id,
            status,
            iam_version,
            refreshed_at
        )
        VALUES ($1, 'active', 1, clock_timestamp())
        ON CONFLICT (organization_id) DO UPDATE
        SET refreshed_at = GREATEST(organization_snapshots.refreshed_at, clock_timestamp()),
            iam_version = GREATEST(organization_snapshots.iam_version, EXCLUDED.iam_version)
        "#,
    )
    .bind(organization_id.as_str())
    .execute(&mut **transaction)
    .await
    .map_err(map_constraint_error)?;

    for actor in actors {
        sqlx::query(
            r#"
            INSERT INTO actor_snapshots (
                organization_id,
                actor_kind,
                actor_id,
                status,
                iam_version,
                refreshed_at
            )
            VALUES ($1, $2::text::actor_kind, $3, 'active', 1, clock_timestamp())
            ON CONFLICT (organization_id, actor_kind, actor_id) DO UPDATE
            SET refreshed_at = GREATEST(actor_snapshots.refreshed_at, clock_timestamp()),
                iam_version = GREATEST(actor_snapshots.iam_version, EXCLUDED.iam_version)
            "#,
        )
        .bind(organization_id.as_str())
        .bind(actor.actor_type.as_str())
        .bind(actor.id.as_str())
        .execute(&mut **transaction)
        .await
        .map_err(map_constraint_error)?;
    }
    Ok(())
}
