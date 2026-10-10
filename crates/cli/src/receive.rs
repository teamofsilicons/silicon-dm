//! Receiving without pushes: `dm sync` (one page of references) and `dm inbox` (a stream of
//! new messages as JSON lines).
use crate::{
    args::{InboxArgs, InboxFrom, SyncArgs},
    output,
};
use anyhow::Result;
use serde_json::Value;
use silicon_dm_client::{
    SyncRequest,
    runtime::{InboxOptions, InboxStart, LocalRuntime},
};

pub async fn sync(local: &LocalRuntime, profile: &str, args: SyncArgs) -> Result<Value> {
    let request = SyncRequest {
        cursor: args.cursor,
        limit: args.limit,
        reset: args.reset,
        wait: args.wait,
    };
    Ok(serde_json::to_value(
        local.sync_page(profile, &request).await?,
    )?)
}

/// Streams the inbox until it is done (without --watch), stopped (Ctrl-C or SIGTERM: exit 0),
/// or a problem that waiting cannot fix (an error).
pub async fn inbox(
    local: &LocalRuntime,
    profile: &str,
    args: InboxArgs,
    _compact: bool,
) -> Result<()> {
    let start = match (args.cursor, args.from) {
        (Some(cursor), _) => InboxStart::Cursor(cursor),
        (None, Some(InboxFrom::Beginning)) => InboxStart::Beginning,
        (None, Some(InboxFrom::Now)) => InboxStart::Now,
        (None, None) => InboxStart::Saved,
    };
    let options = InboxOptions {
        profile: profile.to_owned(),
        watch: args.watch,
        wait: args.wait,
        start,
        hydrate: !args.references_only,
        mark_delivered: args.mark_delivered,
        limit: args.limit,
    };
    let run = local.inbox(
        &options,
        |line| output::print_line(&line).map_err(Into::into),
        |notice| eprintln!("{notice}"),
    );
    tokio::select! {
        result = run => {
            let done = result?;
            output::print_line(&done)?;
            Ok(())
        }
        () = stopped() => Ok(()),
    }
}

/// Ctrl-C, or SIGTERM from a supervisor.
async fn stopped() {
    #[cfg(unix)]
    {
        let mut terminate =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(signal) => signal,
                Err(_) => {
                    let _ = tokio::signal::ctrl_c().await;
                    return;
                }
            };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
