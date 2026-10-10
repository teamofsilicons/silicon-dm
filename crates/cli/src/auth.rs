//! `dm login` (device flow or short-lived token), and the offline `dm accounts --json`.
use crate::{args::LoginArgs, output};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use silicon_dm_client::{
    accounts::{DeviceEvent, DeviceStart, SILICON_SIGN_IN, normalize_accounts_url},
    runtime::{DaemonCommand, LocalRuntime, ServiceCheck, SignInTarget, store},
};

/// DM's app id at Silicon Accounts; `DM_APP_ID` selects a development app.
pub fn app_id() -> String {
    std::env::var("DM_APP_ID")
        .ok()
        .map(|id| id.trim().to_owned())
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| silicon_dm_client::APP_ID.to_owned())
}

fn env_url(variable: &str, default: &str) -> String {
    std::env::var(variable)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default.to_owned())
}

/// `dm accounts --json`: how DM signs in. Offline: reads only ACCOUNTS_URL, DM_API_URL and
/// DM_APP_ID, touches no state, never fails.
pub fn accounts_json() -> Value {
    let app_id = app_id();
    let accounts_url = env_url("ACCOUNTS_URL", store::DEFAULT_ACCOUNTS_URL);
    let accounts_url = normalize_accounts_url(&accounts_url).unwrap_or(accounts_url);
    json!({
        "app_id": app_id,
        "client_id": app_id,
        "accounts_url": accounts_url,
        "api_url": env_url("DM_API_URL", store::DEFAULT_API_URL).trim_end_matches('/'),
        "version": env!("CARGO_PKG_VERSION"),
        "contract_version": silicon_dm_client::CONTRACT_VERSION,
        "device_flow": true,
        "public_client": true,
        "sign_in": {
            "carbon": "dm login",
            "silicon": SILICON_SIGN_IN,
        },
        "status": "dm login status --json",
        "install": "silicon-apps install dm",
        "website_url": "https://dm.teamofsilicons.com",
        "docs_url": "https://docs.dm.teamofsilicons.com",
        "repository_url": "https://github.com/teamofsilicons/silicon-dm",
        "package_url": "https://crates.io/crates/silicon-dm-client",
    })
}

/// `dm CLI on <host>`, from the system host name (no subprocess).
fn default_label() -> String {
    let host = std::env::var("HOSTNAME")
        .ok()
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .or_else(host_name)
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty());
    match host {
        Some(host) => format!("dm CLI on {host}"),
        None => "dm CLI".to_owned(),
    }
}
#[cfg(unix)]
fn host_name() -> Option<String> {
    let mut buffer = [0_u8; 256];
    // SAFETY: the buffer is valid for its length; gethostname NUL-terminates on success.
    let status = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) };
    if status != 0 {
        return None;
    }
    let end = buffer.iter().position(|b| *b == 0).unwrap_or(buffer.len());
    String::from_utf8(buffer[..end].to_vec()).ok()
}
#[cfg(not(unix))]
fn host_name() -> Option<String> {
    None
}

fn open_in_browser(url: &str) -> bool {
    let mut command = if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else if cfg!(windows) {
        let mut command = std::process::Command::new("cmd");
        command.args(["/C", "start", ""]);
        command
    } else {
        std::process::Command::new("xdg-open")
    };
    command
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok()
}

fn announce(start: &DeviceStart, compact: bool) -> Result<()> {
    if compact {
        output::print_line(&json!({
            "event": "device_code",
            "user_code": start.user_code,
            "verification_uri": start.verification_uri,
            "verification_uri_complete": start.verification_uri_complete,
            "expires_in": start.expires_in,
            "interval": start.interval,
        }))?;
    } else {
        let page = start
            .verification_uri_complete
            .as_deref()
            .unwrap_or(&start.verification_uri);
        eprintln!(
            "To sign in to DM, open\n  {page}\nand approve the code {} (it expires in {} minutes). Waiting for approval…",
            start.user_code,
            start.expires_in.clamp(60, 600) / 60
        );
    }
    Ok(())
}

/// The short-lived token from whichever input the command named; never echoed.
fn short_lived_token(args: &LoginArgs) -> Result<Option<String>> {
    const PROMPT: &str = "Short-lived token for dm (hidden): ";
    if let Some(token) = args.slt.as_ref().or(args.slt_option.as_ref()) {
        let token = token.trim().to_owned();
        anyhow::ensure!(!token.is_empty(), "the short-lived token is empty");
        return Ok(Some(token));
    }
    if args.slt_stdin {
        return crate::read_secret(&crate::stdin_path(), PROMPT).map(Some);
    }
    if let Some(path) = &args.token_file {
        return crate::read_secret(path, PROMPT).map(Some);
    }
    Ok(None)
}

/// `dm login`: signs the profile in and starts the relay.
pub async fn login(
    local: &LocalRuntime,
    profile: &str,
    args: LoginArgs,
    compact: bool,
) -> Result<Value> {
    let target = SignInTarget::new(profile, &args.api_url, &args.accounts_url, &app_id())?;
    // Read the token before any network call, so an empty input fails without spending anything.
    let slt = short_lived_token(&args)?;
    match LocalRuntime::check_service(&target).await? {
        ServiceCheck::Matches(_) => {}
        ServiceCheck::Unreachable(why) => eprintln!(
            "The DM service at {} could not be asked which Silicon Accounts it trusts ({why}); signing in anyway.",
            target.api_url
        ),
    }
    let client = local.public_client(&target)?;
    let (session, method) = match slt {
        Some(slt) => (client.exchange_slt(&slt).await?, "slt"),
        None => {
            let label = args.label.clone().unwrap_or_else(default_label);
            let start = client.start_device(Some(&label)).await?;
            announce(&start, compact)?;
            if args.open {
                let page = start
                    .verification_uri_complete
                    .as_deref()
                    .unwrap_or(&start.verification_uri);
                if !open_in_browser(page) {
                    eprintln!("Could not open a browser; open the page above yourself.");
                }
            }
            let session = client
                .wait_for_device(&start, |event| match event {
                    DeviceEvent::SlowDown { interval } => {
                        if compact {
                            let _ = output::print_line(&json!({"event":"slow_down","interval":interval}));
                        } else {
                            eprintln!("Silicon Accounts asked dm to check less often (every {interval} s).");
                        }
                    }
                    DeviceEvent::Retrying { message } => {
                        eprintln!("{message} Still waiting for approval…");
                    }
                })
                .await?;
            (session, "device")
        }
    };
    let launch = DaemonCommand {
        executable: std::env::current_exe()
            .context("cannot locate the dm executable to start its relay")?,
        arguments: vec!["daemon".into(), "run".into()],
    };
    let result = local
        .save_sign_in(&target, &session, method, &launch)
        .await?;
    if !compact {
        eprintln!(
            "Signed in to DM as {} ({}). NEXT: dm conversations list; dm inbox --watch to receive.",
            result["id"].as_str().unwrap_or("?"),
            result["kind"].as_str().unwrap_or("?")
        );
    }
    Ok(result)
}
