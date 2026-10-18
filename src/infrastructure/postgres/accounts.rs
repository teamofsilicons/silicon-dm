//! The local cache of Silicon Accounts accounts, the custodian circle and Silicon
//! allow-lists. Accounts is authoritative; every write here comes from a verified token,
//! an app lookup or a signed webhook.

use std::collections::HashMap;

use sqlx::FromRow;
use time::OffsetDateTime;

use super::{PostgresStore, map_constraint_error};
use crate::{
    AppResult,
    application::ports::AccountLookup,
    domain::{Account, ActorType, ConversationChange},
};

/// One cached account with its bookkeeping.
#[derive(Clone, Debug, FromRow)]
pub struct StoredAccount {
    /// Permanent uuid.
    pub uuid: String,
    /// `carbon` or `silicon`.
    pub kind: String,
    /// Current public id (null when deleted).
    pub current_id: Option<String>,
    /// Display name.
    pub display_name: Option<String>,
    /// Photo URL.
    pub pfp_url: Option<String>,
    /// A Silicon's custodian uuid.
    pub custodian_uuid: Option<String>,
    /// `active` or `deleted`.
    pub status: String,
    /// Tokens issued before this instant are refused.
    pub revoked_before: Option<OffsetDateTime>,
    /// Last time Accounts confirmed this row (token, lookup or webhook).
    pub refreshed_at: OffsetDateTime,
}

impl StoredAccount {
    /// The domain view.
    #[must_use]
    pub fn account(&self) -> Account {
        Account {
            uuid: self.uuid.clone(),
            kind: if self.kind == "silicon" {
                ActorType::Silicon
            } else {
                ActorType::Carbon
            },
            id: self.current_id.clone(),
            display_name: self.display_name.clone(),
            pfp_url: self.pfp_url.clone(),
            custodian: self.custodian_uuid.clone(),
            deleted: self.status == "deleted",
        }
    }
}

/// What applying a webhook event did.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Applied {
    /// The cache changed.
    Changed,
    /// The event was older than what DM already knows.
    Stale,
}

const COLUMNS: &str = "uuid, kind::text AS kind, current_id, display_name, pfp_url, custodian_uuid, \
    status, revoked_before, refreshed_at";

impl PostgresStore {
    /// The cached accounts for these uuids.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn accounts_by_uuid(&self, uuids: &[String]) -> AppResult<HashMap<String, Account>> {
        if uuids.is_empty() {
            return Ok(HashMap::new());
        }
        let rows: Vec<StoredAccount> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {COLUMNS} FROM accounts WHERE uuid = ANY($1)"
        )))
        .bind(uuids)
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| (row.uuid.clone(), row.account()))
            .collect())
    }

    /// One cached account.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn stored_account(&self, uuid: &str) -> AppResult<Option<StoredAccount>> {
        Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {COLUMNS} FROM accounts WHERE uuid = $1"
        )))
        .bind(uuid)
        .fetch_optional(self.pool())
        .await?)
    }

    /// The Accounts uuids that IAM-era public ids were linked to.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn identity_links_for(
        &self,
        public_ids: &[String],
    ) -> AppResult<HashMap<String, String>> {
        if public_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT iam_public_id, accounts_uuid FROM identity_links WHERE iam_public_id = ANY($1)",
        )
        .bind(public_ids)
        .fetch_all(self.pool())
        .await?;
        Ok(rows.into_iter().collect())
    }

    /// Active cached accounts that currently claim a public id (normally zero or one).
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn accounts_with_public_id(&self, id: &str) -> AppResult<Vec<StoredAccount>> {
        Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {COLUMNS} FROM accounts WHERE current_id = $1 AND status = 'active' ORDER BY refreshed_at DESC"
        )))
        .bind(id)
        .fetch_all(self.pool())
        .await?)
    }

    /// Records a verified sign-in. The token's id only replaces the cached one when the
    /// token is newer than the last id change DM heard about (tokens live 30 minutes and
    /// keep the id they were issued with). Returns the row and whether it is new.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn record_authentication(
        &self,
        uuid: &str,
        kind: ActorType,
        id: Option<&str>,
        issued_at: Option<OffsetDateTime>,
    ) -> AppResult<(StoredAccount, bool)> {
        let row: (bool,) = sqlx::query_as(
            "INSERT INTO accounts (uuid, kind, current_id, last_authenticated_at, refreshed_at)
             VALUES ($1, $2::text::actor_kind, $3, clock_timestamp(), clock_timestamp())
             ON CONFLICT (uuid) DO UPDATE SET
                current_id = CASE
                    WHEN accounts.status = 'deleted' THEN accounts.current_id
                    WHEN EXCLUDED.current_id IS NOT NULL
                     AND (accounts.id_changed_at IS NULL OR $4::timestamptz > accounts.id_changed_at)
                    THEN EXCLUDED.current_id
                    ELSE accounts.current_id END,
                last_authenticated_at = clock_timestamp(),
                refreshed_at = GREATEST(accounts.refreshed_at, clock_timestamp())
             WHERE accounts.last_authenticated_at IS NULL
                OR accounts.last_authenticated_at < clock_timestamp() - interval '60 seconds'
                OR accounts.current_id IS DISTINCT FROM EXCLUDED.current_id
             RETURNING (xmax = 0)",
        )
        .bind(uuid)
        .bind(kind.as_str())
        .bind(id.filter(|id| valid_public_id(id)))
        .bind(issued_at)
        .fetch_optional(self.pool())
        .await
        .map_err(map_constraint_error)?
        .unwrap_or((false,));
        let stored = self
            .stored_account(uuid)
            .await?
            .ok_or_else(|| crate::AppError::internal(anyhow::anyhow!("account row vanished")))?;
        Ok((stored, row.0))
    }

    /// Records an authoritative lookup (or a webhook's account snapshot): identity,
    /// profile and custodian. Returns whether the row is new.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn record_lookup(&self, lookup: &AccountLookup) -> AppResult<bool> {
        let mut transaction = self.pool().begin().await?;
        if let Some((custodian, custodian_id)) = &lookup.custodian {
            sqlx::query(
                "INSERT INTO accounts (uuid, kind, current_id) VALUES ($1, 'carbon', $2)
                 ON CONFLICT (uuid) DO NOTHING",
            )
            .bind(custodian)
            .bind(Some(custodian_id.as_str()).filter(|id| valid_public_id(id)))
            .execute(&mut *transaction)
            .await
            .map_err(map_constraint_error)?;
        }
        let inserted: (bool,) = sqlx::query_as(
            "INSERT INTO accounts (uuid, kind, current_id, display_name, pfp_url, custodian_uuid,
                status, deleted_at, refreshed_at)
             VALUES ($1, $2::text::actor_kind, $3, $4, $5, $6,
                CASE WHEN $7 THEN 'deleted' ELSE 'active' END,
                CASE WHEN $7 THEN clock_timestamp() END, clock_timestamp())
             ON CONFLICT (uuid) DO UPDATE SET
                current_id = CASE WHEN $7 THEN NULL ELSE COALESCE(EXCLUDED.current_id, accounts.current_id) END,
                display_name = CASE WHEN $7 THEN NULL ELSE COALESCE(EXCLUDED.display_name, accounts.display_name) END,
                pfp_url = CASE WHEN $7 THEN NULL ELSE COALESCE(EXCLUDED.pfp_url, accounts.pfp_url) END,
                custodian_uuid = COALESCE(EXCLUDED.custodian_uuid, accounts.custodian_uuid),
                status = CASE WHEN $7 THEN 'deleted' ELSE accounts.status END,
                deleted_at = CASE WHEN $7 THEN COALESCE(accounts.deleted_at, clock_timestamp()) ELSE accounts.deleted_at END,
                refreshed_at = clock_timestamp()
             RETURNING (xmax = 0)",
        )
        .bind(&lookup.uuid)
        .bind(lookup.kind.as_str())
        .bind(lookup.id.as_deref().filter(|id| valid_public_id(id)))
        .bind(lookup.display_name.as_deref().map(|name| truncate(name, 200)))
        .bind(lookup.pfp_url.as_deref().filter(|url| url.len() <= 2048))
        .bind(lookup.custodian.as_ref().map(|(uuid, _)| uuid.as_str()))
        .bind(lookup.deleted)
        .fetch_one(&mut *transaction)
        .await
        .map_err(map_constraint_error)?;
        if let Some(id) = lookup.id.as_deref().filter(|_| !lookup.deleted) {
            // Accounts says this account holds the id now: nobody else does.
            sqlx::query(
                "UPDATE accounts SET current_id = NULL WHERE current_id = $1 AND uuid <> $2",
            )
            .bind(id)
            .bind(&lookup.uuid)
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await?;
        Ok(inserted.0)
    }

    /// The profile a member shared with DM (its user base entry). Only fills a row no
    /// `account.updated` event has described yet, so a read racing a newer event never
    /// moves the profile back; events keep it current from then on.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn record_member_profile(
        &self,
        uuid: &str,
        profile: &crate::application::ports::MemberProfile,
    ) -> AppResult<()> {
        sqlx::query(
            "UPDATE accounts SET display_name = $2, pfp_url = $3, refreshed_at = clock_timestamp()
             WHERE uuid = $1 AND status = 'active' AND profile_version = 0",
        )
        .bind(uuid)
        .bind(
            profile
                .display_name
                .as_deref()
                .map(|name| truncate(name, 200)),
        )
        .bind(profile.pfp_url.as_deref().filter(|url| url.len() <= 2048))
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// `account.id_changed`, applied in `occurred_at` order.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn apply_id_change(
        &self,
        uuid: &str,
        kind: Option<ActorType>,
        new_id: &str,
        occurred_at: OffsetDateTime,
    ) -> AppResult<Applied> {
        if !valid_public_id(new_id) {
            return Err(crate::AppError::refused(
                http::StatusCode::BAD_REQUEST,
                "invalid_webhook_body",
                format!("account.id_changed carries an invalid id `{new_id}`"),
            ));
        }
        let kind = kind.unwrap_or(if new_id.starts_with("si:") {
            ActorType::Silicon
        } else {
            ActorType::Carbon
        });
        let mut transaction = self.pool().begin().await?;
        let changed = sqlx::query(
            "INSERT INTO accounts (uuid, kind, current_id, id_changed_at) VALUES ($1, $2::text::actor_kind, $3, $4)
             ON CONFLICT (uuid) DO UPDATE SET current_id = EXCLUDED.current_id,
                id_changed_at = EXCLUDED.id_changed_at, refreshed_at = clock_timestamp()
             WHERE accounts.status = 'active'
               AND (accounts.id_changed_at IS NULL OR accounts.id_changed_at < EXCLUDED.id_changed_at)",
        )
        .bind(uuid)
        .bind(kind.as_str())
        .bind(new_id)
        .bind(occurred_at)
        .execute(&mut *transaction)
        .await
        .map_err(map_constraint_error)?
        .rows_affected();
        if changed == 1 {
            sqlx::query(
                "UPDATE accounts SET current_id = NULL WHERE current_id = $1 AND uuid <> $2",
            )
            .bind(new_id)
            .bind(uuid)
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await?;
        Ok(if changed == 1 {
            Applied::Changed
        } else {
            Applied::Stale
        })
    }

    /// `account.updated`, applied in profile-version order.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn apply_profile_update(
        &self,
        lookup: &AccountLookup,
        version: i64,
    ) -> AppResult<Applied> {
        let changed = sqlx::query(
            "INSERT INTO accounts (uuid, kind, current_id, display_name, pfp_url, custodian_uuid, profile_version)
             VALUES ($1, $2::text::actor_kind, $3, $4, $5, $6, $7)
             ON CONFLICT (uuid) DO UPDATE SET
                display_name = EXCLUDED.display_name,
                pfp_url = EXCLUDED.pfp_url,
                custodian_uuid = COALESCE(EXCLUDED.custodian_uuid, accounts.custodian_uuid),
                profile_version = EXCLUDED.profile_version,
                refreshed_at = clock_timestamp()
             WHERE accounts.status = 'active' AND accounts.profile_version < EXCLUDED.profile_version",
        )
        .bind(&lookup.uuid)
        .bind(lookup.kind.as_str())
        .bind(lookup.id.as_deref().filter(|id| valid_public_id(id)))
        .bind(lookup.display_name.as_deref().map(|name| truncate(name, 200)))
        .bind(lookup.pfp_url.as_deref().filter(|url| url.len() <= 2048))
        .bind(lookup.custodian.as_ref().map(|(uuid, _)| uuid.as_str()))
        .bind(version.max(0))
        .execute(self.pool())
        .await
        .map_err(map_constraint_error)?
        .rows_affected();
        Ok(if changed == 1 {
            Applied::Changed
        } else {
            Applied::Stale
        })
    }

    /// `silicon.custodian_changed`, applied in `occurred_at` order.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn apply_custodian_change(
        &self,
        silicon: &str,
        custodian: &str,
        custodian_id: &str,
        occurred_at: OffsetDateTime,
    ) -> AppResult<Applied> {
        let mut transaction = self.pool().begin().await?;
        sqlx::query(
            "INSERT INTO accounts (uuid, kind, current_id) VALUES ($1, 'carbon', $2)
             ON CONFLICT (uuid) DO NOTHING",
        )
        .bind(custodian)
        .bind(Some(custodian_id).filter(|id| valid_public_id(id)))
        .execute(&mut *transaction)
        .await
        .map_err(map_constraint_error)?;
        let changed = sqlx::query(
            "INSERT INTO accounts (uuid, kind, custodian_uuid, custodian_changed_at)
             VALUES ($1, 'silicon', $2, $3)
             ON CONFLICT (uuid) DO UPDATE SET custodian_uuid = EXCLUDED.custodian_uuid,
                custodian_changed_at = EXCLUDED.custodian_changed_at, refreshed_at = clock_timestamp()
             WHERE accounts.kind = 'silicon'
               AND (accounts.custodian_changed_at IS NULL OR accounts.custodian_changed_at < EXCLUDED.custodian_changed_at)",
        )
        .bind(silicon)
        .bind(custodian)
        .bind(occurred_at)
        .execute(&mut *transaction)
        .await
        .map_err(map_constraint_error)?
        .rows_affected();
        transaction.commit().await?;
        Ok(if changed == 1 {
            Applied::Changed
        } else {
            Applied::Stale
        })
    }

    /// Refuses tokens issued before `at` (sign-out, access removed). Never moves back.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn revoke_sessions_before(
        &self,
        uuid: &str,
        at: OffsetDateTime,
    ) -> AppResult<Applied> {
        let changed = sqlx::query(
            "UPDATE accounts SET revoked_before = $2
             WHERE uuid = $1 AND (revoked_before IS NULL OR revoked_before < $2)",
        )
        .bind(uuid)
        .bind(at)
        .execute(self.pool())
        .await?
        .rows_affected();
        Ok(if changed == 1 {
            Applied::Changed
        } else {
            Applied::Stale
        })
    }

    /// The Silicons a Carbon is custodian of (uuids).
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn silicons_of(&self, custodian: &str) -> AppResult<Vec<String>> {
        Ok(sqlx::query_scalar(
            "SELECT uuid FROM accounts WHERE custodian_uuid = $1 AND kind = 'silicon' AND status = 'active' ORDER BY uuid",
        )
        .bind(custodian)
        .fetch_all(self.pool())
        .await?)
    }

    /// Whether `other` may reach the Silicon `silicon` unasked: it is the Silicon itself,
    /// its custodian, another Silicon of the same custodian, or on its allow-list.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn may_reach_silicon(&self, silicon: &str, other: &str) -> AppResult<bool> {
        Ok(sqlx::query_scalar(
            "SELECT EXISTS (
                SELECT 1 FROM accounts s WHERE s.uuid = $1 AND (
                    s.uuid = $2
                    OR s.custodian_uuid = $2
                    OR EXISTS (SELECT 1 FROM accounts o WHERE o.uuid = $2 AND o.kind = 'silicon'
                               AND o.custodian_uuid IS NOT NULL AND o.custodian_uuid = s.custodian_uuid)
                    OR EXISTS (SELECT 1 FROM silicon_contact_allowances a
                               WHERE a.silicon_id = s.uuid AND a.allowed_id = $2)))",
        )
        .bind(silicon)
        .bind(other)
        .fetch_one(self.pool())
        .await?)
    }

    /// The first (Silicon, account) pair in which the account may not reach the Silicon
    /// unasked ([`Self::may_reach_silicon`] for every pair of `silicons` × `accounts` at once,
    /// in their order), or `None` when every account may reach every Silicon.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn first_unreachable_silicon(
        &self,
        silicons: &[&str],
        accounts: &[&str],
    ) -> AppResult<Option<(String, String)>> {
        Ok(sqlx::query_as(
            "SELECT t.id, o.id
             FROM unnest($1::text[]) WITH ORDINALITY AS t(id, position)
             LEFT JOIN accounts s ON s.uuid = t.id
             CROSS JOIN unnest($2::text[]) WITH ORDINALITY AS o(id, position)
             WHERE o.id <> t.id
               AND s.custodian_uuid IS DISTINCT FROM o.id
               AND NOT EXISTS (SELECT 1 FROM accounts x WHERE x.uuid = o.id AND x.kind = 'silicon'
                               AND x.custodian_uuid IS NOT NULL AND x.custodian_uuid = s.custodian_uuid)
               AND NOT EXISTS (SELECT 1 FROM silicon_contact_allowances a
                               WHERE a.silicon_id = t.id AND a.allowed_id = o.id)
             ORDER BY t.position, o.position
             LIMIT 1",
        )
        .bind(silicons)
        .bind(accounts)
        .fetch_optional(self.pool())
        .await?)
    }
}

/// Whether `id` is a well-formed public id (`c:handle` / `si:handle`).
#[must_use]
pub fn valid_public_id(id: &str) -> bool {
    let handle = id.strip_prefix("c:").or_else(|| id.strip_prefix("si:"));
    handle.is_some_and(|handle| {
        (1..=50).contains(&handle.len())
            && handle.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
            })
    })
}

fn truncate(value: &str, characters: usize) -> String {
    value.chars().take(characters).collect()
}

impl PostgresStore {
    /// Whether this webhook event was already applied.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn webhook_event_seen(&self, event_id: &str) -> AppResult<bool> {
        Ok(sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM accounts_webhook_receipts WHERE event_id = $1)",
        )
        .bind(event_id)
        .fetch_one(self.pool())
        .await?)
    }

    /// Records an applied (or deliberately ignored) webhook event.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn record_webhook_event(
        &self,
        event_id: &str,
        event_type: &str,
        account: Option<&str>,
        occurred_at: Option<OffsetDateTime>,
        outcome: &str,
    ) -> AppResult<()> {
        sqlx::query(
            "INSERT INTO accounts_webhook_receipts (event_id, event_type, account_uuid, occurred_at, outcome)
             VALUES ($1, $2, $3, $4, $5) ON CONFLICT (event_id) DO NOTHING",
        )
        .bind(event_id)
        .bind(event_type)
        .bind(account)
        .bind(occurred_at)
        .bind(outcome)
        .execute(self.pool())
        .await
        .map_err(map_constraint_error)?;
        Ok(())
    }

    /// Ends what DM holds for an account whose sign-in ended: stored Ting proofs and the
    /// enrolment that relied on them (the next sign-in enrols again).
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn end_stored_proofs(&self, uuid: &str) -> AppResult<()> {
        let mut transaction = self.pool().begin().await?;
        sqlx::query("DELETE FROM ting_proof_grants WHERE actor_id = $1")
            .bind(uuid)
            .execute(&mut *transaction)
            .await?;
        sqlx::query(
            "DELETE FROM ting_automatic_enrollments WHERE organization_id = '@accounts' AND actor_id = $1",
        )
        .bind(uuid)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(())
    }

    /// `account.deleted`: keep the messages others are part of (they render as from a
    /// deleted account, without its name or photo), delete the account's own drafts, recent GIFs, allow-lists,
    /// presence and Ting enrolment, and take it out of every group. A group it was the
    /// last owner of passes to its longest-standing member.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn delete_account(&self, uuid: &str, at: OffsetDateTime) -> AppResult<()> {
        let mut transaction = self.pool().begin().await?;
        sqlx::query(
            "INSERT INTO accounts (uuid, kind, status, deleted_at, revoked_before)
             VALUES ($1, 'carbon', 'deleted', $2, $2)
             ON CONFLICT (uuid) DO UPDATE SET status = 'deleted', current_id = NULL,
                display_name = NULL, pfp_url = NULL,
                deleted_at = COALESCE(accounts.deleted_at, EXCLUDED.deleted_at),
                revoked_before = GREATEST(COALESCE(accounts.revoked_before, EXCLUDED.revoked_before), EXCLUDED.revoked_before),
                refreshed_at = clock_timestamp()",
        )
        .bind(uuid)
        .bind(at)
        .execute(&mut *transaction)
        .await
        .map_err(map_constraint_error)?;
        for statement in [
            // Draft content may only change after a versioned draft update in the same
            // transaction (guard_draft_content_mutation).
            "UPDATE drafts SET version = version + 1 WHERE actor_id = $1",
            "DELETE FROM draft_attachments WHERE actor_id = $1",
            "DELETE FROM draft_gifs WHERE actor_id = $1",
            "DELETE FROM drafts WHERE actor_id = $1",
            "DELETE FROM recent_gifs WHERE carbon_id = $1",
            "DELETE FROM silicon_contact_allowances WHERE silicon_id = $1 OR allowed_id = $1",
            "DELETE FROM client_presence_leases WHERE actor_id = $1",
            "DELETE FROM actor_presence_state WHERE actor_id = $1",
            "DELETE FROM ting_proof_grants WHERE actor_id = $1",
            "DELETE FROM ting_automatic_enrollments WHERE actor_id = $1",
            "UPDATE ting_handoffs SET discarded_at = clock_timestamp(), discard_reason = 'account_deleted'
             WHERE target_id = $1 AND accepted_at IS NULL AND discarded_at IS NULL",
        ] {
            sqlx::query(statement)
                .bind(uuid)
                .execute(&mut *transaction)
                .await?;
        }
        let groups: Vec<uuid::Uuid> = sqlx::query_scalar(
            "SELECT conversation_id FROM group_owners WHERE actor_id = $1
             UNION SELECT conversation_id FROM group_invitations WHERE actor_id = $1",
        )
        .bind(uuid)
        .fetch_all(&mut *transaction)
        .await?;
        // Every conversation the account is in (or invited to) changes for the others: it
        // shows the account as deleted, and a group may have lost an owner. A group's
        // participant rows outlive membership, so groups count by their invitations (a group
        // the account left long ago does not change now).
        let affected: Vec<(uuid::Uuid, bool)> = sqlx::query_as(
            "SELECT conversation_id, bool_or(owner) FROM (
                SELECT p.conversation_id, false AS owner FROM conversation_participants p
                JOIN conversations c ON c.id = p.conversation_id
                WHERE p.actor_id = $1 AND NOT c.is_group
                UNION ALL SELECT conversation_id, false FROM group_invitations WHERE actor_id = $1
                UNION ALL SELECT conversation_id, true FROM group_owners WHERE actor_id = $1
             ) mine GROUP BY conversation_id ORDER BY conversation_id",
        )
        .bind(uuid)
        .fetch_all(&mut *transaction)
        .await?;
        sqlx::query("DELETE FROM group_owners WHERE actor_id = $1")
            .bind(uuid)
            .execute(&mut *transaction)
            .await?;
        sqlx::query("DELETE FROM group_invitations WHERE actor_id = $1")
            .bind(uuid)
            .execute(&mut *transaction)
            .await?;
        for group in groups {
            super::groups::ensure_owner_in(&mut transaction, group).await?;
        }
        let mut events = Vec::new();
        for (conversation, was_owner) in affected {
            for account in
                super::conversation_events::participants_in(&mut transaction, conversation).await?
            {
                events.push((
                    conversation,
                    account.clone(),
                    ConversationChange::MembersChanged,
                ));
                if was_owner {
                    events.push((conversation, account, ConversationChange::OwnersChanged));
                }
            }
            if was_owner {
                for account in
                    super::conversation_events::group_invitees_in(&mut transaction, conversation)
                        .await?
                {
                    events.push((conversation, account, ConversationChange::OwnersChanged));
                }
            }
        }
        super::conversation_events::record_many_in(&mut transaction, &events).await?;
        transaction.commit().await?;
        Ok(())
    }
}
