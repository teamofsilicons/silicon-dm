//! Authenticated reports persist before acknowledgement. Outside production, without
//! Postmark configured, reports are stored and marked simulated (no email is sent).
use axum::{Json, extract::State, http::StatusCode};
use secrecy::ExposeSecret as _;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::Row as _;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    AppError, AppResult,
    api::extract::{ApiJson, Authenticated, Idempotency},
    application::state::AppState,
};

const REPOSITORY: &str = "https://github.com/teamofsilicons/silicon-dm";
const RECIPIENTS: &str = "saketdev12@gmail.com,shubhastro2@gmail.com,bugs@teamofsilicons.com";
/// Longest report message, in bytes.
const MAX_REPORT_BYTES: usize = 60_000;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Report {
    message: String,
    pr: Option<String>,
    client_version: String,
}

pub(crate) async fn submit(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    Idempotency(key): Idempotency,
    ApiJson(report): ApiJson<Report>,
) -> AppResult<(StatusCode, Json<Value>)> {
    if report.message.trim().is_empty() {
        return Err(AppError::invalid(
            "A report needs a message that says what happened; this one is empty.",
        )
        .with_hint("Say what you did, what you expected and what happened instead."));
    }
    if report.message.len() > MAX_REPORT_BYTES {
        return Err(AppError::invalid(format!(
            "A report's message is at most {MAX_REPORT_BYTES} bytes; this one has {}.",
            report.message.len()
        ))
        .with_hint("Shorten it: keep what happened and the error, and leave out long logs."));
    }
    if report.client_version.len() > 100 {
        return Err(AppError::invalid(
            "A report's client_version is at most 100 bytes, such as \"dm 0.17.0\".",
        ));
    }
    if let Some(pr) = &report.pr {
        let valid = url::Url::parse(pr).is_ok_and(|url| {
            url.scheme() == "https"
                && url.host_str() == Some("github.com")
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none()
                && url
                    .path()
                    .strip_prefix("/teamofsilicons/silicon-dm/pull/")
                    .is_some_and(|id| {
                        !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit())
                    })
        });
        if !valid {
            return Err(AppError::invalid(
                "pr must be the link of a pull request in teamofsilicons/silicon-dm, such as https://github.com/teamofsilicons/silicon-dm/pull/17.",
            )
            .with_hint("Leave pr out when there is no pull request yet."));
        }
    }
    let simulated = state.settings.reporting.is_none()
        && state.settings.environment != crate::config::RuntimeEnvironment::Production;
    if !simulated && state.settings.reporting.is_none() {
        return Err(AppError::DependencyUnavailable {
            dependency: "postmark_configuration",
        });
    }
    let payload = serde_json::to_value(report).map_err(AppError::internal)?;
    let mut tx = state.store.pool().begin().await?;
    // Serialize one reporter's retries and rate budget across API replicas.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 29))")
        .bind(format!("report:{}", actor.actor.id))
        .execute(&mut *tx)
        .await?;
    let old = sqlx::query("SELECT id,payload,status FROM bug_reports WHERE organization_id='@accounts' AND actor_id=$1 AND idempotency_key=$2")
        .bind(actor.actor.id.as_str()).bind(key.as_str()).fetch_optional(&mut *tx).await?;
    let (id, status): (Uuid, String) = if let Some(old) = old {
        if old.get::<Value, _>("payload") != payload {
            return Err(AppError::conflict(
                "report idempotency key already belongs to different content",
            ));
        }
        (old.get("id"), old.get("status"))
    } else {
        // The report that has to leave the hour-long window before another fits: the
        // limit-th newest. Present means the account is at its limit; its age says when
        // the next report fits.
        let limit = state.settings.reports_per_hour;
        let wait: Option<f64> = sqlx::query_scalar(
            "SELECT EXTRACT(EPOCH FROM created_at + interval '1 hour' - now())::float8 FROM bug_reports
             WHERE actor_id=$1 AND created_at>now()-interval '1 hour'
             ORDER BY created_at DESC OFFSET $2 LIMIT 1",
        )
        .bind(actor.actor.id.as_str())
        .bind(i64::from(limit) - 1)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(wait) = wait {
            return Err(report_limit(limit, wait));
        }
        let id = Uuid::now_v7();
        let status = if simulated { "simulated" } else { "queued" };
        sqlx::query("INSERT INTO bug_reports(id,organization_id,actor_id,idempotency_key,payload,status) VALUES($1,'@accounts',$2,$3,$4,$5)")
            .bind(id).bind(actor.actor.id.as_str()).bind(key.as_str()).bind(payload).bind(status).execute(&mut *tx).await?;
        (id, status.to_owned())
    };
    tx.commit().await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({"id":id,"submitted":true,"notification":status,"repository":REPOSITORY})),
    ))
}

/// 429 for an account that sent its hour's reports, saying when the next one fits.
fn report_limit(limit: u32, wait_seconds: f64) -> AppError {
    // Whole seconds, at least one: the window is an hour, so this always fits.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to 1..=3600 first"
    )]
    let seconds = wait_seconds.ceil().clamp(1.0, 3600.0) as u64;
    let when = if seconds < 90 {
        format!("{seconds} seconds")
    } else {
        format!("{} minutes", seconds.div_ceil(60))
    };
    AppError::rate_limited(format!(
        "This account sent {limit} bug reports in the last hour, the most DM accepts from one account. The reports already sent are kept."
    ))
    .with_hint(format!(
        "Send this one in {when} (Retry-After), or put everything into one report then."
    ))
    .with_retry_after(seconds)
    .with_details(json!({"limit": limit, "window_seconds": 3600, "retry_after_seconds": seconds}))
}

/// Deliver one queued production report with bounded transport and durable retries.
///
/// # Errors
/// Returns a redacted database error; provider failures remain queued for retry.
pub async fn deliver_one(state: &AppState) -> AppResult<()> {
    let Some(settings) = &state.settings.reporting else {
        return Ok(());
    };
    let mut tx = state.store.pool().begin().await?;
    let Some(row) = sqlx::query("SELECT r.id,r.payload,r.actor_id,COALESCE(a.current_id, r.actor_id) AS reporter FROM bug_reports r LEFT JOIN accounts a ON a.uuid = r.actor_id WHERE r.status='queued' AND r.next_attempt_at<=now() ORDER BY r.next_attempt_at FOR UPDATE OF r SKIP LOCKED LIMIT 1")
        .fetch_optional(&mut *tx).await? else { return Ok(()); };
    let id: Uuid = row.get("id");
    let payload: Value = row.get("payload");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(AppError::internal)?;
    let response = client.post(settings.endpoint.clone())
        .header("X-Postmark-Server-Token", settings.token.expose_secret())
        .header("Accept", "application/json")
        .json(&json!({"From":"dm@teamofsilicons.com","To":RECIPIENTS,
            "Subject":format!("Silicon DM bug report {id}"),
            "TextBody":format!("Report {id}\nReporter: {} (account {})\nClient: {}\n\n{}\n\nPR: {}\nRepository: {REPOSITORY}",row.get::<String,_>("reporter"),row.get::<String,_>("actor_id"),payload["client_version"].as_str().unwrap_or_default(),payload["message"].as_str().unwrap_or_default(),payload["pr"].as_str().unwrap_or("No PR supplied; contributions welcome.")),
            "TrackOpens":false,"TrackLinks":"None","MessageStream":"outbound","Metadata":{"report_id":id.to_string()}}))
        .send().await;
    let delivered = match response {
        Ok(response) if response.status().is_success() => response
            .json::<Value>()
            .await
            .is_ok_and(|value| value["ErrorCode"] == 0 && value["MessageID"].is_string()),
        _ => false,
    };
    if delivered {
        sqlx::query(
            "UPDATE bug_reports SET status='sent',sent_at=now(),attempts=attempts+1 WHERE id=$1",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
    } else {
        sqlx::query("UPDATE bug_reports SET attempts=attempts+1,next_attempt_at=now()+make_interval(secs=>least(3600,60*power(2,least(attempts,6)))::double precision) WHERE id=$1").bind(id).execute(&mut *tx).await?;
        tracing::warn!(failure="report_notification", report_id=%id, "Postmark notification remains queued for retry");
    }
    tx.commit().await?;
    Ok(())
}

pub(crate) async fn run(state: AppState, cancellation: CancellationToken) {
    let mut timer = tokio::time::interval(Duration::from_secs(5));
    loop {
        tokio::select! {
            () = cancellation.cancelled() => return,
            _ = timer.tick() => {
                if let Some(Err(error)) = cancellation.run_until_cancelled(deliver_one(&state)).await {
                    tracing::warn!(failure="report_queue",code=error.code(),"Report worker will retry");
                }
            }
        }
    }
}
