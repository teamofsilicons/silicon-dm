//! Minimal, untracked invitation mail with a durable Postmark retry queue.
use crate::{AppError, AppResult, application::state::AppState};
use secrecy::ExposeSecret as _;
use serde_json::{Value, json};
use sqlx::Row as _;
use std::time::Duration;
use uuid::Uuid;

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn message(to: &str, inviter: &str, partner: &str, link: &str, expires: &str, id: Uuid) -> Value {
    let sentence = format!("{inviter} has invited you to talk with {partner} on Silicon DM.");
    let next = "Sign in or create an account with this email address. When asked, share your email with DM to match this invitation. Your conversation will open automatically.";
    let end = format!(
        "This invitation expires on {expires}. If you do not want to chat, you can ignore this email."
    );
    json!({
        "From": "Silicon DM <dm@teamofsilicons.com>", "To": to,
        "Subject": format!("{inviter} invited you to chat"),
        "TextBody": format!("{sentence}\n\nOpen chat: {link}\n\n{next}\n\n{end}"),
        "HtmlBody": format!("<!doctype html><html><body><main style=\"max-width:520px;margin:32px auto;padding:0 20px;font:16px/1.5 Arial,sans-serif;color:#242424\"><p>{}</p><p><a href=\"{}\">Open chat</a></p><p>{next}</p><p style=\"font-size:13px;color:#595959\">{}</p></main></body></html>", escape(&sentence), escape(link), escape(&end)),
        "TrackOpens": false, "TrackLinks": "None", "MessageStream": "outbound",
        "Metadata": {"invitation_id": id.to_string()}
    })
}

/// Delivers one queued invitation; transient provider failures stay queued.
///
/// # Errors
/// Storage errors and HTTP client setup failures, with no email content in logs.
pub async fn deliver_one(state: &AppState) -> AppResult<()> {
    let Some(settings) = &state.settings.reporting else {
        return Ok(());
    };
    let mut tx = state.store.pool().begin().await?;
    let Some(row) = sqlx::query("SELECT i.id,i.recipient_email,a.current_id AS inviter,p.current_id AS partner,
        to_char(i.expires_at AT TIME ZONE 'UTC','YYYY-MM-DD HH24:MI') || ' UTC' AS expiry
        FROM chat_invitations i JOIN accounts a ON a.uuid=i.inviter_uuid JOIN accounts p ON p.uuid=i.partner_uuid
        WHERE i.mail_status='queued' AND i.status='pending' AND i.expires_at>now() AND i.next_attempt_at<=now()
        AND a.status='active' AND p.status='active' AND (a.uuid=p.uuid OR (a.kind='carbon' AND p.custodian_uuid=a.uuid))
        ORDER BY i.next_attempt_at FOR UPDATE OF i SKIP LOCKED LIMIT 1")
        .fetch_optional(&mut *tx).await? else { return Ok(()) };
    let id: Uuid = row.get("id");
    let mail = message(
        &row.get::<String, _>("recipient_email"),
        &row.get::<String, _>("inviter"),
        &row.get::<String, _>("partner"),
        &format!("{}invite/{id}", state.settings.web_url),
        &row.get::<String, _>("expiry"),
        id,
    );
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(AppError::internal)?;
    let response = client
        .post(settings.endpoint.clone())
        .header("X-Postmark-Server-Token", settings.token.expose_secret())
        .header("Accept", "application/json")
        .json(&mail)
        .send()
        .await;
    let delivered = match response {
        Ok(response) if response.status().is_success() => response
            .json::<Value>()
            .await
            .is_ok_and(|body| body["ErrorCode"] == 0 && body["MessageID"].is_string()),
        _ => false,
    };
    if delivered {
        sqlx::query("UPDATE chat_invitations SET mail_status='sent',sent_at=now(),mail_attempts=mail_attempts+1 WHERE id=$1")
            .bind(id).execute(&mut *tx).await?;
    } else {
        sqlx::query("UPDATE chat_invitations SET mail_attempts=mail_attempts+1,next_attempt_at=now()+make_interval(secs=>least(3600,60*power(2,least(mail_attempts,6)))::double precision) WHERE id=$1")
            .bind(id).execute(&mut *tx).await?;
        tracing::warn!(invitation_id=%id, "Chat invitation email remains queued for retry");
    }
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::message;
    #[test]
    fn mail_is_minimal_untracked_and_escaped() {
        let mail = message(
            "ada@example.test",
            "c:sender<&",
            "si:scout",
            "https://dm.example/invite/id",
            "2030-01-01",
            uuid::Uuid::nil(),
        );
        assert_eq!(mail["TrackOpens"], false);
        assert_eq!(mail["TrackLinks"], "None");
        let html = mail["HtmlBody"].as_str().unwrap_or_default();
        assert!(html.contains("c:sender&lt;&amp;"));
        assert!(!html.contains("<img"));
        assert!(
            mail["TextBody"]
                .as_str()
                .unwrap_or_default()
                .contains("talk with si:scout")
        );
    }
}
