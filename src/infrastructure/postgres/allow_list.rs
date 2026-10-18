//! A Silicon's allow-list, managed by the Silicon and by its custodian. It no longer limits
//! anything: since DM 0.17.1 anyone signed in to DM may start a conversation with any
//! Silicon or invite it to a group. The rows and their routes stay so that older CLIs and
//! open browser tabs that read and change the list keep working (contract 4 only grows).

use serde::Serialize;
use sqlx::FromRow;
use time::OffsetDateTime;

use super::{PostgresStore, map_constraint_error};
use crate::{
    AppResult,
    domain::{ActorId, ActorRef, ActorType},
};

/// One allowed account.
#[derive(Clone, Debug, FromRow, Serialize)]
pub struct AllowListEntry {
    /// The allowed account (rendered with its current id).
    #[sqlx(skip)]
    pub account: Option<ActorRef>,
    #[serde(skip)]
    allowed_kind: String,
    #[serde(skip)]
    allowed_id: String,
    /// Who added it: the Silicon itself or its custodian (rendered).
    #[sqlx(skip)]
    pub added_by: Option<ActorRef>,
    #[serde(skip)]
    added_by_id: String,
    #[serde(skip)]
    added_by_kind: String,
    /// When.
    #[serde(with = "time::serde::rfc3339")]
    pub added_at: OffsetDateTime,
}

impl PostgresStore {
    /// The Silicon's allow-list, oldest first.
    ///
    /// # Errors
    /// Database failures.
    pub async fn allow_list(&self, silicon: &str) -> AppResult<Vec<AllowListEntry>> {
        let mut rows: Vec<AllowListEntry> = sqlx::query_as(
            "SELECT a.allowed_kind::text AS allowed_kind, a.allowed_id, a.added_by_id,
                    COALESCE(by.kind::text, 'carbon') AS added_by_kind, a.added_at
             FROM silicon_contact_allowances a
             LEFT JOIN accounts by ON by.uuid = a.added_by_id
             WHERE a.silicon_id = $1
             ORDER BY a.added_at, a.allowed_id",
        )
        .bind(silicon)
        .fetch_all(self.pool())
        .await?;
        for row in &mut rows {
            row.account = Some(reference(&row.allowed_kind, &row.allowed_id));
            row.added_by = Some(reference(&row.added_by_kind, &row.added_by_id));
        }
        Ok(rows)
    }

    /// Puts `who` on the Silicon's allow-list. Idempotent; returns whether it was new.
    ///
    /// # Errors
    /// Database failures.
    pub async fn allow_contact(
        &self,
        silicon: &str,
        who: &ActorRef,
        added_by: &str,
    ) -> AppResult<bool> {
        Ok(sqlx::query(
            "INSERT INTO silicon_contact_allowances (silicon_id, allowed_kind, allowed_id, added_by_id)
             VALUES ($1, $2::text::actor_kind, $3, $4) ON CONFLICT DO NOTHING",
        )
        .bind(silicon)
        .bind(who.actor_type.as_str())
        .bind(who.id.as_str())
        .bind(added_by)
        .execute(self.pool())
        .await
        .map_err(map_constraint_error)?
        .rows_affected()
            == 1)
    }

    /// Removes `who` from the Silicon's allow-list. Returns whether it was there.
    ///
    /// # Errors
    /// Database failures.
    pub async fn disallow_contact(&self, silicon: &str, who: &ActorRef) -> AppResult<bool> {
        Ok(sqlx::query(
            "DELETE FROM silicon_contact_allowances
             WHERE silicon_id = $1 AND allowed_kind = $2::text::actor_kind AND allowed_id = $3",
        )
        .bind(silicon)
        .bind(who.actor_type.as_str())
        .bind(who.id.as_str())
        .execute(self.pool())
        .await?
        .rows_affected()
            == 1)
    }
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
