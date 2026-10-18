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
    if let Some(refusal) = error.downcast_ref::<crate::docs::DocsRefusal>() {
        return json!({"error": {"code": refusal.code, "message": refusal.message,
            "hint": refusal.hint, "details": refusal.details}});
    }
    if let Some(problem) = error.downcast_ref::<silicon_dm_client::speech::SpeechError>() {
        let mut detail =
            json!({"code": problem.code, "message": problem.message, "hint": problem.hint});
        if let Some(reason) = problem.reason {
            detail["reason"] = json!(reason);
        }
        if let Some(position) = problem.position {
            detail["position"] = json!(position);
        }
        return json!({"error": detail});
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
            "A Silicon is reached only by its custodian, the custodian's other Silicons and the accounts it allows: ask the Silicon (dm allow add <you>) or its custodian (on the DM website, Settings → Your Silicons' reach; or dm allow add <you> --for si:…)."
        }
        "unsupported_contract" => {
            "This dm and the DM service speak different contracts: point DM_API_URL at a DM 0.15 service, or update dm (Silicon Apps keeps it current)."
        }
        "sync_reset_required" => "Run dm sync --reset, then refresh with dm conversations list.",
        "voice_not_found" => {
            "dm voices list shows the voices this DM service speaks with; leave out --voice to speak in your own."
        }
        "voice_change_forbidden" => {
            "An account chooses its own voice, and a Silicon's custodian chooses for it (dm voices set KEY --for si:…)."
        }
        "speech_not_configured" => {
            "This DM service has no speech provider configured; its operators set DM_DEEPGRAM_API_KEY. Text messages work as usual."
        }
        "speech_unavailable" => {
            "Speech is briefly unavailable; retry after the time in retry_after. A queued spoken message is retried by the relay."
        }
        "speech_rejected" => {
            "The voice could not speak this script: simplify its cues (dm docs expressive) or try another voice (dm voices list)."
        }
        "nothing_to_speak" => {
            "The message has no words to say (only attachments or a GIF); dm messages transcribe reads a voice note instead."
        }
        "message_deleted" => "The message was deleted; its content, speech included, is gone.",
        "unsupported_audio_type" => {
            "DM transcribes audio/webm, audio/ogg, audio/wav, audio/mpeg, audio/mp4, audio/x-m4a, audio/aac and audio/flac; convert it (ffmpeg -i in.xxx out.wav)."
        }
        "audio_too_large" => "Transcribe a shorter recording (split it with any audio editor).",
        "read_only_view" => {
            "A custodian can read its Silicon's conversations but not mark them read; the Silicon marks its own messages read."
        }
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

/// An error already in the `{"error": …}` shape, on standard error.
pub fn print_value_error(value: &Value, compact: bool) {
    let text = if compact {
        serde_json::to_string(value)
    } else {
        serde_json::to_string_pretty(value)
    };
    eprintln!("{}", text.unwrap_or_else(|_| value.to_string()));
}
