//! Silicon DM schema migrations and cutover operator commands.

use std::{path::PathBuf, process::ExitCode};

const USAGE: &str = "\
dm-migrate: Silicon DM database migrations and Silicon Accounts cutover commands.

USAGE:
    dm-migrate                                   Apply the embedded schema migrations.
    dm-migrate link-identities --file <mapping.csv> [--dry-run] [--offline] [--force-relink]
                                                 Re-key IAM-era identities (c:handle, si:handle) to
                                                 Silicon Accounts uuids in one transaction.
    dm-migrate draft-identity-mapping --out <mapping.csv>
                                                 Draft a mapping by looking every IAM-era id up in
                                                 Silicon Accounts. Never applies anything.
    dm-migrate --help

link-identities:
    The mapping file has the header `iam_principal_id,accounts_uuid`; the first column is the
    IAM public id DM stored (c:saket, si:cos) or an IAM principal UUID. Every uuid is checked
    with Silicon Accounts (ACCOUNTS_URL, ACCOUNTS_API_URL, DM_APP_ID, DM_APP_SECRET) unless
    --offline. Each run is recorded in dm.identity_links / dm.identity_link_runs; running it
    again with a corrected mapping re-points links (before cutover). --dry-run reports and
    rolls back. Prints the JSON report on stdout.

Environment: DM_DATABASE_URL (the migrator role), DM_ENVIRONMENT, DM_LOG_FILTER.
";

#[tokio::main]
async fn main() -> ExitCode {
    dotenvy::dotenv().ok();
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments
        .iter()
        .any(|argument| argument == "--help" || argument == "-h")
    {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let settings = match silicon_dm::config::MigrationSettings::from_env() {
        Ok(settings) => settings,
        Err(error) => {
            eprintln!("DM migration configuration is invalid: {error}");
            return ExitCode::FAILURE;
        }
    };
    if silicon_dm::logging::init(settings.environment, &settings.log_filter).is_err() {
        eprintln!("DM migration logging initialization failed");
        return ExitCode::FAILURE;
    }
    match arguments.first().map(String::as_str) {
        None => match silicon_dm::bootstrap::run_migrations(settings).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                // The whole chain: "internal server error" alone tells an operator nothing.
                let cause = causes(&error);
                tracing::error!(error = %error, cause = %cause, "DM migration failed");
                eprintln!("DM migration failed: {cause}");
                ExitCode::FAILURE
            }
        },
        Some("link-identities") => {
            let mut link = silicon_dm::operator::LinkArguments::default();
            let mut rest = arguments.iter().skip(1);
            while let Some(argument) = rest.next() {
                match argument.as_str() {
                    "--file" => match rest.next() {
                        Some(path) => link.file = PathBuf::from(path),
                        None => return usage_error("--file needs a path"),
                    },
                    "--dry-run" => link.dry_run = true,
                    "--offline" => link.offline = true,
                    "--force-relink" => link.force_relink = true,
                    other => return usage_error(&format!("unknown argument `{other}`")),
                }
            }
            if link.file.as_os_str().is_empty() {
                return usage_error("link-identities needs --file <mapping.csv>");
            }
            match silicon_dm::operator::link(&settings, &link).await {
                Ok(report) => {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&report).unwrap_or_default()
                    );
                    eprintln!(
                        "{}: {} linked, {} re-pointed, {} unchanged, {} IAM-era ids still unmapped, {} conversations superseded",
                        if report.dry_run {
                            "dry run (rolled back)"
                        } else {
                            "applied"
                        },
                        report.linked.len(),
                        report.relinked.len(),
                        report.unchanged.len(),
                        report.unmatched.len(),
                        report.superseded
                    );
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!("link-identities failed, nothing was changed: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("draft-identity-mapping") => {
            let out = match (arguments.get(1).map(String::as_str), arguments.get(2)) {
                (Some("--out"), Some(path)) => PathBuf::from(path),
                _ => return usage_error("draft-identity-mapping needs --out <mapping.csv>"),
            };
            match silicon_dm::operator::draft_mapping(&settings, &out).await {
                Ok(summary) => {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&summary).unwrap_or_default()
                    );
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!("draft-identity-mapping failed: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        Some(other) => usage_error(&format!("unknown command `{other}`")),
    }
}

fn usage_error(message: &str) -> ExitCode {
    eprintln!("dm-migrate: {message}\n\n{USAGE}");
    ExitCode::from(2)
}

/// An error and its sources, outermost first ("database migration failed: while executing
/// migration 43: error returned from database: canceling statement due to statement timeout").
/// DM's errors never carry the database URL or its password.
fn causes(error: &silicon_dm::AppError) -> String {
    let mut words = vec![error.to_string()];
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        let text = cause.to_string();
        if words.last() != Some(&text) {
            words.push(text);
        }
        source = cause.source();
    }
    if words.len() > 1 && words[0] == "internal server error" {
        words.remove(0);
    }
    words.join(": ")
}
