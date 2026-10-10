//! Standard output is JSON (pretty, or one line with --json); errors are JSON on standard
//! error: `{"error":{"code","message","hint"?,…}}`.
use serde_json::{Value, json};
use silicon_dm_client::{accounts::SignInError, runtime::store::SessionError};
use std::io::Write;

pub fn print_json(value: &Value, compact: bool) {
    let result = (|| -> std::io::Result<()> {
        let mut output = std::io::BufWriter::new(std::io::stdout().lock());
        if compact {
            serde_json::to_writer(&mut output, value)?;
        } else {
            serde_json::to_writer_pretty(&mut output, value)?;
        }
        output.write_all(b"\n")?;
        output.flush()
    })();
    if let Err(error) = result {
        eprintln!("dm could not write its JSON result: {error}");
        std::process::exit(1);
    }
}

/// One JSON line, flushed at once (streams such as dm inbox and dm login --json).
pub fn print_line(value: &Value) -> std::io::Result<()> {
    let mut output = std::io::stdout().lock();
    serde_json::to_writer(&mut output, value)?;
    output.write_all(b"\n")?;
    output.flush()
}

/// The JSON error object for any failure, keeping the service's code, message and hint.
pub fn error_json(error: &anyhow::Error) -> Value {
    if let Some(silicon_dm_client::Error::Api {
        status,
        code,
        message,
        hint,
        body,
        request_id,
        retry_after,
    }) = error.downcast_ref::<silicon_dm_client::Error>()
    {
        let mut detail = json!({"status": status, "code": code, "message": message,
            "body": body, "request_id": request_id, "retry_after": retry_after});
        if let Some(hint) = hint {
            detail["hint"] = json!(hint);
        } else if let Some(hint) = default_hint(code) {
            detail["hint"] = json!(hint);
        }
        return json!({"error": detail});
    }
    if let Some(reason) = error.downcast_ref::<SessionError>() {
        return json!({"error": {"code": reason.code, "message": reason.message, "hint": reason.hint}});
    }
    if let Some(reason) = error.downcast_ref::<SignInError>() {
        let mut detail =
            json!({"code": reason.code(), "message": reason.message(), "hint": reason.hint()});
        if let Some(status) = reason.status() {
            detail["status"] = json!(status);
        }
        return json!({"error": detail});
    }
    if let Some(client) = error.downcast_ref::<silicon_dm_client::Error>() {
        return json!({"error": {"code": client.code(), "message": format!("{error:#}")}});
    }
    json!({"error": {"code": "command_failed", "message": format!("{error:#}")}})
}

/// What to do next for service errors that come without a hint.
fn default_hint(code: &str) -> Option<&'static str> {
    Some(match code {
        "token_expired" | "invalid_token" | "unauthorized" | "session_revoked" => {
            "Sign in again: dm login (Carbons) or silicon-accounts login --app dm -q | dm login --slt-stdin (Silicons)."
        }
        "contact_not_allowed" => {
            "A Silicon is reached only by its custodian, the custodian's other Silicons and the accounts it allows: ask the Silicon (dm allow add <you>) or its custodian (dm allow add <you> --for si:…)."
        }
        "unsupported_contract" => {
            "This dm and the DM service speak different contracts: point DM_API_URL at a DM 0.15 service, or update dm (Silicon Apps keeps it current)."
        }
        "sync_reset_required" => "Run dm sync --reset, then refresh with dm conversations list.",
        _ => return None,
    })
}

pub fn print_error(error: &anyhow::Error, compact: bool) {
    let value = error_json(error);
    let text = if compact {
        serde_json::to_string(&value)
    } else {
        serde_json::to_string_pretty(&value)
    };
    eprintln!("{}", text.unwrap_or_else(|_| format!("{error:#}")));
}
