//! Authenticated reports persist before acknowledgement; sandbox delivery is simulated.
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
const RECIPIENTS: &str = "saketdev12@gmail.com,shubhastro2@gmails.com,bugs@teamofsilicons.com";

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
    if report.message.trim().is_empty()
        || report.message.len() > 60_000
        || report.client_version.len() > 100
    {
        return Err(AppError::validation(
            "report requires 1–60000 message bytes and a client version of at most 100 bytes",
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
            return Err(AppError::validation(
                "PR must be a pull-request URL in teamofsilicons/silicon-dm",
            ));
        }
    }
    let simulated = state.testing_environment.is_some();
    if !simulated && state.settings.reporting.is_none() {
        return Err(AppError::DependencyUnavailable {
            dependency: "postmark_configuration",
        });
    }
    let payload = serde_json::to_value(report).map_err(AppError::internal)?;
    let mut tx = state.store.pool().begin().await?;
    // Serialize one reporter's retries and rate budget across API replicas.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 29))")
        .bind(format!(
            "report:{}:{}",
            actor.organization_id, actor.actor.id
        ))
        .execute(&mut *tx)
        .await?;
    let old = sqlx::query("SELECT id,payload,status FROM bug_reports WHERE organization_id=$1 AND actor_id=$2 AND idempotency_key=$3")
        .bind(actor.organization_id.as_str()).bind(actor.actor.id.as_str()).bind(key.as_str()).fetch_optional(&mut *tx).await?;
    let (id, status): (Uuid, String) = if let Some(old) = old {
        if old.get::<Value, _>("payload") != payload {
            return Err(AppError::conflict(
                "report idempotency key already belongs to different content",
            ));
        }
        (old.get("id"), old.get("status"))
    } else {
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM bug_reports WHERE organization_id=$1 AND actor_id=$2 AND created_at>now()-interval '1 hour'")
            .bind(actor.organization_id.as_str()).bind(actor.actor.id.as_str()).fetch_one(&mut *tx).await?;
        if count >= 10 {
            return Err(AppError::RateLimited);
        }
        let id = Uuid::now_v7();
        let status = if simulated { "simulated" } else { "queued" };
        sqlx::query("INSERT INTO bug_reports(id,organization_id,actor_id,idempotency_key,payload,status) VALUES($1,$2,$3,$4,$5,$6)")
            .bind(id).bind(actor.organization_id.as_str()).bind(actor.actor.id.as_str()).bind(key.as_str()).bind(payload).bind(status).execute(&mut *tx).await?;
        (id, status.to_owned())
    };
    tx.commit().await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({"id":id,"submitted":true,"notification":status,"repository":REPOSITORY})),
    ))
}

/// Deliver one queued production report with bounded transport and durable retries.
///
/// # Errors
/// Returns a redacted database error; provider failures remain queued for retry.
pub async fn deliver_one(state: &AppState) -> AppResult<()> {
    // Defense in depth: even a queued row inside a sandbox cannot send real mail.
    if state.testing_environment.is_some() {
        return Ok(());
    }
    let Some(settings) = &state.settings.reporting else {
        return Ok(());
    };
    let mut tx = state.store.pool().begin().await?;
    let Some(row) = sqlx::query("SELECT id,payload,actor_id,organization_id FROM bug_reports WHERE status='queued' AND next_attempt_at<=now() ORDER BY next_attempt_at FOR UPDATE SKIP LOCKED LIMIT 1")
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
            "TextBody":format!("Report {id}\nActor: {}@{}\nClient: {}\n\n{}\n\nPR: {}\nRepository: {REPOSITORY}",row.get::<String,_>("actor_id"),row.get::<String,_>("organization_id"),payload["client_version"].as_str().unwrap_or_default(),payload["message"].as_str().unwrap_or_default(),payload["pr"].as_str().unwrap_or("No PR supplied; contributions welcome.")),
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
