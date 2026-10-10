//! Operator commands of `dm-migrate` beyond applying migrations: re-keying IAM-era
//! identities to Silicon Accounts uuids at cutover, and drafting the mapping file.

use std::{collections::BTreeMap, fmt::Write as _, path::PathBuf, time::Duration};

use secrecy::SecretString;

use crate::{
    AppError, AppResult,
    application::ports::IdentityProvider as _,
    config::{AccountsSettings, MigrationSettings, ProofIssuers},
    infrastructure::{
        accounts::AccountsAdapter,
        postgres::{
            PostgresStore,
            link_identities::{
                LinkOptions, LinkReport, VerifiedAccount, legacy_ids, link_identities,
                parse_mapping,
            },
        },
    },
};

/// `dm-migrate link-identities` arguments.
#[derive(Debug, Default)]
pub struct LinkArguments {
    /// The mapping CSV (`iam_principal_id,accounts_uuid`).
    pub file: PathBuf,
    /// Report and roll back.
    pub dry_run: bool,
    /// Do not verify uuids with Silicon Accounts (the cache is refreshed by DM later).
    pub offline: bool,
    /// Re-point links whose current uuid already signed in to DM.
    pub force_relink: bool,
}

/// Silicon Accounts settings for operator lookups, from `ACCOUNTS_URL`, `ACCOUNTS_API_URL`,
/// `DM_APP_ID` and `DM_APP_SECRET`.
///
/// # Errors
/// Names the missing variable.
pub fn accounts_from_env() -> AppResult<AccountsSettings> {
    let read = |name: &str| {
        std::env::var(name)
            .ok()
            .filter(|value| !value.trim().is_empty())
    };
    let issuer = read("ACCOUNTS_URL")
        .ok_or_else(|| AppError::validation("ACCOUNTS_URL is required to verify uuids with Silicon Accounts (or pass --offline)"))?
        .trim_end_matches('/')
        .to_owned();
    let api = read("ACCOUNTS_API_URL").unwrap_or_else(|| issuer.clone());
    Ok(AccountsSettings {
        api_url: api
            .parse()
            .map_err(|_| AppError::validation("ACCOUNTS_API_URL is not a URL"))?,
        issuer,
        app_id: read("DM_APP_ID").unwrap_or_else(|| "dm".to_owned()),
        app_secret: SecretString::from(read("DM_APP_SECRET").ok_or_else(|| {
            AppError::validation("DM_APP_SECRET is required to verify uuids with Silicon Accounts (or pass --offline)")
        })?),
        webhook_secret: SecretString::from("whsec_unused-by-operator-commands".to_owned()),
        request_timeout: Duration::from_secs(10),
        proof_issuers: ProofIssuers::default(),
    })
}

/// Runs `dm-migrate link-identities`.
///
/// # Errors
/// Unreadable mapping, failed verification, or a refused re-key (nothing changes then).
pub async fn link(
    settings: &MigrationSettings,
    arguments: &LinkArguments,
) -> AppResult<LinkReport> {
    let text = std::fs::read_to_string(&arguments.file).map_err(|error| {
        AppError::validation(format!(
            "cannot read the mapping file {}: {error}",
            arguments.file.display()
        ))
    })?;
    let rows = parse_mapping(&text)?;
    let mut verified = BTreeMap::new();
    if !arguments.offline {
        let adapter = AccountsAdapter::new(&accounts_from_env()?)?;
        for row in &rows {
            let account = adapter.lookup(&row.accounts_uuid).await.map_err(|error| {
                AppError::validation(format!(
                    "line {}: Silicon Accounts could not confirm {}: {error}",
                    row.line, row.accounts_uuid
                ))
            })?;
            if account.deleted {
                return Err(AppError::validation(format!(
                    "line {}: {} is a deleted Silicon Accounts account",
                    row.line, row.accounts_uuid
                )));
            }
            verified.insert(
                row.accounts_uuid.clone(),
                VerifiedAccount {
                    kind: account.kind.as_str().to_owned(),
                    current_id: account.id,
                    display_name: account.display_name,
                    custodian_uuid: account.custodian.map(|(uuid, _)| uuid),
                },
            );
        }
    }
    let store = PostgresStore::connect(&settings.database).await?;
    let mut connection = store.pool().acquire().await?;
    let report = link_identities(
        &mut connection,
        &rows,
        &verified,
        &LinkOptions {
            dry_run: arguments.dry_run,
            source: arguments.file.display().to_string(),
            force_relink: arguments.force_relink,
        },
    )
    .await?;
    drop(connection);
    store.pool().close().await;
    Ok(report)
}

/// Runs `dm-migrate draft-identity-mapping`: looks every IAM-era id in the data up by its
/// current id in Silicon Accounts and writes the rows it could resolve. It never applies
/// anything; ids that were renamed or never moved need explicit rows.
///
/// # Errors
/// Missing Accounts configuration, unwritable output, or database failures.
pub async fn draft_mapping(
    settings: &MigrationSettings,
    out: &PathBuf,
) -> AppResult<serde_json::Value> {
    let adapter = AccountsAdapter::new(&accounts_from_env()?)?;
    let store = PostgresStore::connect(&settings.database).await?;
    let mut connection = store.pool().acquire().await?;
    let ids = legacy_ids(&mut connection).await?;
    drop(connection);
    store.pool().close().await;
    let mut csv = String::from("iam_principal_id,accounts_uuid\n");
    let mut unresolved = BTreeMap::new();
    for (id, rows) in &ids {
        let Some(public) =
            Some(id.as_str()).filter(|id| crate::infrastructure::postgres::valid_public_id(id))
        else {
            unresolved.insert(id.clone(), format!("not an IAM public id ({rows} rows)"));
            continue;
        };
        match adapter.lookup_by_id(public).await {
            Ok(account) if !account.deleted => {
                let _ = writeln!(csv, "{public},{}", account.uuid);
            }
            Ok(_) => {
                unresolved.insert(
                    id.clone(),
                    "the account with this id was deleted".to_owned(),
                );
            }
            Err(error) => {
                unresolved.insert(id.clone(), error.to_string());
            }
        }
    }
    std::fs::write(out, csv).map_err(|error| {
        AppError::validation(format!("cannot write {}: {error}", out.display()))
    })?;
    Ok(serde_json::json!({
        "written": out.display().to_string(),
        "ids_in_data": ids.len(),
        "unresolved": unresolved,
        "note": "Review every row before applying: a handle that now belongs to someone else would be linked to the wrong account. Add explicit rows for the unresolved ids.",
    }))
}
