//! Accounts as DM sees them: admitting an authenticated caller, resolving the ids people
//! type to permanent uuids, and the custodian rules.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use dashmap::DashMap;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::{
    AppError, AppResult,
    application::{auth::AuthContext, state::AppState},
    domain::{Account, ActorId, ActorRef, ActorType, is_account_uuid},
    infrastructure::postgres::{StoredAccount, valid_public_id},
};

/// How long a cached id → uuid answer is trusted before DM asks Accounts again. Accounts
/// tells DM about id changes of its members by webhook; this bounds staleness for others.
const ID_CACHE_TTL: time::Duration = time::Duration::minutes(10);
/// Minimum time between two attempts to learn a Silicon's custodian.
const ENRICH_RETRY: Duration = Duration::from_secs(60);

/// Process-local throttles of the accounts service.
#[derive(Clone, Default)]
pub struct AccountRuntime {
    enriched: Arc<DashMap<String, Instant>>,
    ting_checked: Arc<DashMap<String, Instant>>,
}

/// How often one process re-checks an account's Ting enrolment.
const TING_CHECK_INTERVAL: Duration = Duration::from_secs(600);

impl AccountRuntime {
    /// Whether this process should check (and possibly start) the account's Ting
    /// enrolment now: at most once per ten minutes per account.
    #[must_use]
    pub fn ting_check_due(&self, uuid: &str) -> bool {
        let now = Instant::now();
        let mut due = false;
        self.ting_checked
            .entry(uuid.to_owned())
            .and_modify(|last| {
                if now.duration_since(*last) >= TING_CHECK_INTERVAL {
                    *last = now;
                    due = true;
                }
            })
            .or_insert_with(|| {
                due = true;
                now
            });
        due
    }
}

/// The accounts service of one request.
pub struct Directory<'a> {
    state: &'a AppState,
}

impl AppState {
    /// The accounts service.
    #[must_use]
    pub fn directory(&self) -> Directory<'_> {
        Directory { state: self }
    }
}

impl Directory<'_> {
    /// Admits an authenticated caller: refuses deleted accounts and sign-ins that ended,
    /// records the sign-in, and the first time DM sees an account completes what it needs
    /// (profile, a Silicon's custodian, the Silicon ↔ custodian conversation).
    ///
    /// # Errors
    /// `session_revoked` / `account_deleted` (401), or storage failures.
    pub async fn admit(&self, context: &AuthContext) -> AppResult<StoredAccount> {
        let store = &self.state.store;
        let own_token = context.access_token().is_some();
        let (stored, inserted) = store
            .record_authentication(
                context.actor.id.as_str(),
                context.actor.actor_type,
                Some(context.public_id.as_str()).filter(|id| valid_public_id(id)),
                context.issued_at,
            )
            .await?;
        if stored.status == "deleted" {
            return Err(AppError::unauthenticated(
                "account_deleted",
                "This Silicon Accounts account was deleted; DM no longer accepts its sign-ins.",
            ));
        }
        if own_token
            && let Some(cutoff) = stored.revoked_before
            && context.issued_at.is_none_or(|issued| issued < cutoff)
        {
            let at = cutoff.format(&Rfc3339).unwrap_or_default();
            return Err(AppError::unauthenticated(
                "session_revoked",
                format!(
                    "This sign-in to DM ended at {at} (signed out, access removed or STK rotated). Sign in to DM again."
                ),
            ));
        }
        // A Silicon without a known custodian, or an account whose profile DM has not
        // read yet (it was first seen as someone's recipient), is completed once a minute
        // at most.
        let incomplete = (stored.kind == "silicon" && stored.custodian_uuid.is_none())
            || stored.display_name.is_none();
        if inserted || (incomplete && self.may_enrich(&stored.uuid)) {
            self.enrich(&context.actor, &stored).await;
        }
        Ok(stored)
    }

    fn may_enrich(&self, uuid: &str) -> bool {
        let now = Instant::now();
        let mut allowed = false;
        self.state
            .accounts
            .enriched
            .entry(uuid.to_owned())
            .and_modify(|last| {
                if now.duration_since(*last) >= ENRICH_RETRY {
                    *last = now;
                    allowed = true;
                }
            })
            .or_insert_with(|| {
                allowed = true;
                now
            });
        allowed
    }

    /// Learns the profile and custodian of a newly seen account and creates the only
    /// conversations DM creates by itself: between a Silicon and its custodian.
    async fn enrich(&self, actor: &ActorRef, stored: &StoredAccount) {
        self.state
            .accounts
            .enriched
            .insert(stored.uuid.clone(), Instant::now());
        match self.state.identity.lookup(&stored.uuid).await {
            Ok(lookup) => {
                if let Err(error) = self.state.store.record_lookup(&lookup).await {
                    tracing::warn!(code = error.code(), "recording an account lookup failed");
                }
            }
            Err(error) => {
                tracing::warn!(
                    code = error.code(),
                    "looking up a newly seen account failed; will retry"
                );
            }
        }
        // Names and photos are not in lookups: they are what a member shared with DM.
        match self.state.identity.member_profile(&stored.uuid).await {
            Ok(Some(profile)) => {
                if let Err(error) = self
                    .state
                    .store
                    .record_member_profile(&stored.uuid, &profile)
                    .await
                {
                    tracing::warn!(code = error.code(), "recording a member profile failed");
                }
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(
                    code = error.code(),
                    "reading a member profile failed; will retry"
                );
            }
        }
        if let Err(error) = self.ensure_custodian_conversations(actor).await {
            tracing::warn!(
                code = error.code(),
                "creating custodian conversations failed"
            );
        }
    }

    /// Creates (idempotently) the direct conversation between a Silicon and its custodian;
    /// for a Carbon, with every Silicon DM knows it looks after.
    ///
    /// # Errors
    /// Storage failures.
    pub async fn ensure_custodian_conversations(&self, actor: &ActorRef) -> AppResult<()> {
        let store = &self.state.store;
        let pairs: Vec<(ActorRef, ActorRef)> = match actor.actor_type {
            ActorType::Silicon => {
                let Some(stored) = store.stored_account(actor.id.as_str()).await? else {
                    return Ok(());
                };
                let Some(custodian) = stored.custodian_uuid else {
                    return Ok(());
                };
                vec![(
                    actor.clone(),
                    ActorRef {
                        actor_type: ActorType::Carbon,
                        id: ActorId::from_storage_key(custodian),
                    },
                )]
            }
            ActorType::Carbon => store
                .silicons_of(actor.id.as_str())
                .await?
                .into_iter()
                .map(|silicon| {
                    (
                        ActorRef {
                            actor_type: ActorType::Silicon,
                            id: ActorId::from_storage_key(silicon),
                        },
                        actor.clone(),
                    )
                })
                .collect(),
        };
        for (silicon, custodian) in pairs {
            store
                .ensure_direct_conversation(&silicon, &custodian)
                .await?;
        }
        Ok(())
    }

    /// Resolves what a caller typed (a `c:`/`si:` id, an ISI address `isi@si:x`, or an
    /// account uuid) to the account. Ids are case-insensitive.
    ///
    /// # Errors
    /// `account_not_found` / `account_deleted` (404) naming the id, a validation error
    /// for something that is not an account id, or Accounts being unavailable.
    pub async fn resolve(&self, selector: &str) -> AppResult<Account> {
        let selector = selector.trim();
        let base = selector.rsplit_once('@').map_or(selector, |(_, base)| base);
        if is_account_uuid(base) {
            return self.account(base).await;
        }
        let id = base.to_ascii_lowercase();
        if !valid_public_id(&id) {
            return Err(AppError::validation(format!(
                "`{selector}` is not an account id: use c:handle for a Carbon or si:handle for a Silicon (or the account's uuid)"
            )));
        }
        let cached = self.state.store.accounts_with_public_id(&id).await?;
        if let [only] = cached.as_slice()
            && only.refreshed_at > OffsetDateTime::now_utc() - ID_CACHE_TTL
        {
            return Ok(only.account());
        }
        let lookup = self.state.identity.lookup_by_id(&id).await?;
        self.state.store.record_lookup(&lookup).await?;
        if lookup.deleted {
            return Err(AppError::account_deleted(&id));
        }
        self.state
            .store
            .stored_account(&lookup.uuid)
            .await?
            .map(|stored| stored.account())
            .ok_or_else(|| AppError::account_not_found(&id))
    }

    /// Resolves several selectors, keeping their order and refusing duplicates of one
    /// account named twice.
    ///
    /// # Errors
    /// The first selector that does not resolve.
    pub async fn resolve_all(&self, selectors: &[ActorId]) -> AppResult<Vec<Account>> {
        let mut accounts = Vec::with_capacity(selectors.len());
        for selector in selectors {
            let account = self.resolve(selector.as_str()).await?;
            if !accounts
                .iter()
                .any(|known: &Account| known.uuid == account.uuid)
            {
                accounts.push(account);
            }
        }
        Ok(accounts)
    }

    /// The account with this uuid: from the cache, else from Accounts.
    ///
    /// # Errors
    /// `account_not_found` / `account_deleted`, or Accounts being unavailable.
    pub async fn account(&self, uuid: &str) -> AppResult<Account> {
        if let Some(stored) = self.state.store.stored_account(uuid).await? {
            let account = stored.account();
            if account.deleted {
                return Err(AppError::account_deleted(uuid));
            }
            return Ok(account);
        }
        let lookup = self.state.identity.lookup(uuid).await?;
        self.state.store.record_lookup(&lookup).await?;
        if lookup.deleted {
            return Err(AppError::account_deleted(uuid));
        }
        self.state
            .store
            .stored_account(uuid)
            .await?
            .map(|stored| stored.account())
            .ok_or_else(|| AppError::account_not_found(uuid))
    }

    /// Refuses `who` starting something new with the Silicon `silicon` unless `who` is in
    /// its circle (itself, its custodian, the custodian's other Silicons) or on its
    /// allow-list. Carbons can be reached by anyone signed in to DM.
    ///
    /// # Errors
    /// `contact_not_allowed` (403) explaining who can allow it.
    pub async fn require_may_reach(&self, who: &ActorRef, target: &Account) -> AppResult<()> {
        if !target.is_silicon() || target.uuid == who.id.as_str() {
            return Ok(());
        }
        if self
            .state
            .store
            .may_reach_silicon(&target.uuid, who.id.as_str())
            .await?
        {
            return Ok(());
        }
        let custodian = match &target.custodian {
            Some(custodian) => self
                .state
                .store
                .stored_account(custodian)
                .await?
                .and_then(|stored| stored.current_id)
                .map_or_else(
                    || "its custodian".to_owned(),
                    |id| format!("its custodian {id}"),
                ),
            None => "its custodian".to_owned(),
        };
        Err(AppError::forbidden(
            "contact_not_allowed",
            format!(
                "{} only accepts new conversations and invitations from its custodian, the custodian's other Silicons and accounts it has allowed. Ask {} (`dm allow add <your id>`) or {custodian} (DM website, Silicons) to allow you first.",
                target.display_id(),
                target.display_id(),
            ),
        ))
    }
}
