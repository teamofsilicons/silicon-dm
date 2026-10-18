//! Stable application errors and HTTP representations.

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;
use thiserror::Error;

/// Application result alias.
pub type AppResult<T> = Result<T, AppError>;

/// Error categories crossing application boundaries.
#[derive(Debug, Error)]
pub enum AppError {
    /// Authentication is absent or invalid.
    #[error("authentication required")]
    Unauthorized,
    /// Authenticated principal lacks authority.
    #[error("operation is not permitted")]
    Forbidden,
    /// A refusal with a precise, actionable explanation and its own stable code.
    #[error("{message}")]
    Refused {
        /// HTTP status.
        status: StatusCode,
        /// Stable machine-readable code.
        code: &'static str,
        /// What was refused and why, and what to do instead.
        message: String,
        /// A hint, structured details and a retry delay, when the refusal has them.
        extra: Option<Box<ErrorExtra>>,
    },
    /// Requested aggregate does not exist in the caller's scope. Prefer
    /// [`AppError::not_found`], whose words name what is missing.
    #[error("It does not exist, or it is not yours to see (DM does not say which).")]
    NotFound,
    /// Request is structurally or semantically invalid.
    #[error("{0}")]
    Validation(String),
    /// A requested expansion cannot be safely materialized in one response.
    #[error("{0}")]
    ResponseTooLarge(String),
    /// Mutation conflicts with current durable state.
    #[error("{0}")]
    Conflict(String),
    /// The saved synchronization position can no longer be replayed safely.
    #[error("{0}")]
    SyncResetRequired(String),
    /// A required conditional header was omitted.
    #[error("{0}")]
    PreconditionRequired(String),
    /// An upstream system could not complete a required check.
    #[error("{}", unavailable_words(dependency))]
    DependencyUnavailable {
        /// Stable dependency name.
        dependency: &'static str,
    },
    /// Caller exceeded an enforced rate. Prefer [`AppError::rate_limited`] with the limit,
    /// a hint and `Retry-After`.
    #[error(
        "Too many requests: DM is limiting how often this can be done. Wait a little, then retry."
    )]
    RateLimited,
    /// Database operation failed. Details remain in server traces.
    #[error("database operation failed")]
    Database(#[source] sqlx::Error),
    /// Unexpected internal failure. Details remain in server traces.
    #[error("internal server error")]
    Internal(#[source] anyhow::Error),
}

/// What an unavailable dependency means for the request, and what to do.
fn unavailable_words(dependency: &str) -> String {
    match dependency {
        "accounts" => "Silicon Accounts did not answer DM in time, so DM could not check the sign-in or the account. Nothing was changed: retry in a few seconds with the same Idempotency-Key.".to_owned(),
        "giphy" => "Giphy did not answer DM in time, so there are no GIFs to show right now. Try again in a moment.".to_owned(),
        "ting" => "Ting did not take DM's request right now; DM retries by itself.".to_owned(),
        "postmark_configuration" => "This DM service cannot email bug reports: its operators have not configured email (DM_POSTMARK_SERVER_TOKEN). The report was not stored; open an issue at https://github.com/teamofsilicons/silicon-dm/issues instead.".to_owned(),
        other => format!("{other} did not answer DM in time. Nothing was changed: retry in a few seconds with the same Idempotency-Key."),
    }
}

/// What a refusal can add to its code and message.
#[derive(Debug, Default)]
pub struct ErrorExtra {
    /// What to do next, in one sentence (`error.hint`).
    pub hint: Option<String>,
    /// Machine-readable facts about the refusal (`error.details`), such as the valid values.
    pub details: Option<serde_json::Value>,
    /// Seconds after which a retry may succeed (the `Retry-After` header).
    pub retry_after_seconds: Option<u64>,
}

#[derive(Debug, Serialize)]
struct ErrorEnvelope {
    error: ErrorDetail,
}

#[derive(Debug, Serialize)]
struct ErrorDetail {
    code: &'static str,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    details: Option<serde_json::Value>,
}

impl AppError {
    /// Creates a validation error without exposing internal details.
    #[must_use]
    pub fn validation(message: impl Into<String>) -> Self {
        Self::Validation(message.into())
    }

    /// Creates a conflict error.
    #[must_use]
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::Conflict(message.into())
    }

    /// A precise refusal with its own code and status.
    #[must_use]
    pub fn refused(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self::Refused {
            status,
            code,
            message: message.into(),
            extra: None,
        }
    }

    /// Adds what to do next (`error.hint`) to a refusal; other errors are unchanged.
    #[must_use]
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        if let Some(extra) = self.extra_mut() {
            extra.hint = Some(hint.into());
        }
        self
    }

    /// Adds machine-readable facts (`error.details`) to a refusal; other errors are unchanged.
    #[must_use]
    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        if let Some(extra) = self.extra_mut() {
            extra.details = Some(details);
        }
        self
    }

    /// Adds a `Retry-After` delay (seconds, at least one) to a refusal; other errors are
    /// unchanged.
    #[must_use]
    pub fn with_retry_after(mut self, seconds: u64) -> Self {
        if let Some(extra) = self.extra_mut() {
            extra.retry_after_seconds = Some(seconds.max(1));
        }
        self
    }

    /// The `Retry-After` delay of a refusal, when it has one.
    #[must_use]
    pub fn retry_after(&self) -> Option<u64> {
        match self {
            Self::Refused {
                extra: Some(extra), ..
            } => extra.retry_after_seconds,
            _ => None,
        }
    }

    /// The hint of a refusal, when it has one.
    #[must_use]
    pub fn hint(&self) -> Option<&str> {
        match self {
            Self::Refused {
                extra: Some(extra), ..
            } => extra.hint.as_deref(),
            _ => None,
        }
    }

    fn extra_mut(&mut self) -> Option<&mut ErrorExtra> {
        match self {
            Self::Refused { extra, .. } => Some(extra.get_or_insert_with(Box::default)),
            _ => None,
        }
    }

    /// 401 with a precise reason (expired, wrong audience, revoked sign-in…).
    #[must_use]
    pub fn unauthenticated(code: &'static str, message: impl Into<String>) -> Self {
        let refusal = Self::refused(StatusCode::UNAUTHORIZED, code, message);
        if code == "invalid_token" {
            // Whatever is wrong with the token, what to do is the same.
            refusal.with_hint("Sign in to DM again (dm login; the website does it by itself) and repeat the request with the new token.")
        } else {
            refusal
        }
    }

    /// 403 with a precise reason.
    #[must_use]
    pub fn forbidden(code: &'static str, message: impl Into<String>) -> Self {
        Self::refused(StatusCode::FORBIDDEN, code, message)
    }

    /// 404 `not_found` (the code every 404 without one of its own keeps) with words that
    /// name what is missing, so it can carry a hint.
    #[must_use]
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::refused(StatusCode::NOT_FOUND, "not_found", message)
    }

    /// 422 `validation_error` that, unlike [`AppError::validation`], can carry a hint and
    /// details.
    #[must_use]
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::refused(
            StatusCode::UNPROCESSABLE_ENTITY,
            "validation_error",
            message,
        )
    }

    /// 429 `rate_limited` for a limit with no code of its own. Name the limit in the
    /// message and add `with_retry_after` when the wait is known.
    #[must_use]
    pub fn rate_limited(message: impl Into<String>) -> Self {
        Self::refused(StatusCode::TOO_MANY_REQUESTS, "rate_limited", message)
    }

    /// 404 for an account id that resolves to nobody.
    #[must_use]
    pub fn account_not_found(id: &str) -> Self {
        Self::refused(
            StatusCode::NOT_FOUND,
            "account_not_found",
            format!(
                "No Silicon Accounts account has the id {id} now. Ids can change: use the account's current c:/si: id (an id released by a rename is not followed)."
            ),
        )
    }

    /// 404 for an account that was deleted.
    #[must_use]
    pub fn account_deleted(id: &str) -> Self {
        Self::refused(
            StatusCode::NOT_FOUND,
            "account_deleted",
            format!(
                "{id} belongs to a deleted Silicon Accounts account; it cannot receive messages or invitations."
            ),
        )
    }

    /// Creates an internal error while retaining its source for tracing.
    #[must_use]
    pub fn internal(error: impl Into<anyhow::Error>) -> Self {
        Self::Internal(error.into())
    }

    /// A database failure as the client sees it. A transient one (PostgreSQL broke a tie
    /// between two changes made at once, a statement ran out of time, no connection was free
    /// or the connection was lost) is a 503 `dependency_unavailable` to retry; anything else
    /// stays a 500 whose details go to the server log only.
    #[must_use]
    pub fn database(error: sqlx::Error) -> Self {
        let transient = match &error {
            sqlx::Error::PoolTimedOut | sqlx::Error::PoolClosed | sqlx::Error::Io(_) => {
                Some("connection")
            }
            sqlx::Error::Database(database) => match database.code().as_deref() {
                Some("40P01") => Some("deadlock"),
                Some("40001") => Some("serialization"),
                Some("55P03") => Some("lock_timeout"),
                Some("57014") => Some("statement_timeout"),
                Some("53300" | "57P01" | "57P02" | "57P03") => Some("unavailable"),
                _ => None,
            },
            _ => None,
        };
        let Some(cause) = transient else {
            return Self::Database(error);
        };
        tracing::warn!(cause, error = %error, "a database operation failed for a moment");
        Self::refused(
            StatusCode::SERVICE_UNAVAILABLE,
            "dependency_unavailable",
            "DM's database could not finish this request just now: it was busy, or two changes made at the same moment got in each other's way.",
        )
        .with_hint("Retry in a moment with the same Idempotency-Key: a change that did go through is answered again, never made twice.")
        .with_retry_after(1)
    }

    /// Stable machine-readable error code.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unauthorized => "unauthorized",
            Self::Forbidden => "forbidden",
            Self::Refused { code, .. } => code,
            Self::NotFound => "not_found",
            Self::Validation(_) => "validation_error",
            Self::ResponseTooLarge(_) => "response_too_large",
            Self::Conflict(_) => "conflict",
            Self::SyncResetRequired(_) => "sync_reset_required",
            Self::PreconditionRequired(_) => "precondition_required",
            Self::DependencyUnavailable { .. } => "dependency_unavailable",
            Self::RateLimited => "rate_limited",
            Self::Database(_) | Self::Internal(_) => "internal_error",
        }
    }

    /// HTTP status matching the public error category.
    #[must_use]
    pub fn status(&self) -> StatusCode {
        match self {
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Validation(_) => StatusCode::UNPROCESSABLE_ENTITY,
            Self::ResponseTooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
            Self::Conflict(_) | Self::SyncResetRequired(_) => StatusCode::CONFLICT,
            Self::PreconditionRequired(_) => StatusCode::PRECONDITION_REQUIRED,
            Self::Refused { status, .. } => *status,
            Self::DependencyUnavailable { .. } => StatusCode::SERVICE_UNAVAILABLE,
            Self::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            Self::Database(_) | Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn public_message(&self) -> String {
        match self {
            Self::Database(_) | Self::Internal(_) => "internal server error".to_owned(),
            _ => self.to_string(),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status();
        if status.is_server_error() {
            tracing::error!(error = ?self, code = self.code(), "request failed");
        }
        let retry_after = self.retry_after();
        let message = self.public_message();
        let code = self.code();
        let (hint, details) = match self {
            Self::Refused {
                extra: Some(extra), ..
            } => (extra.hint, extra.details),
            _ => (None, None),
        };
        let body = ErrorEnvelope {
            error: ErrorDetail {
                code,
                message,
                hint,
                details,
            },
        };
        let mut response = (status, Json(body)).into_response();
        if let Some(seconds) = retry_after {
            response
                .headers_mut()
                .insert(axum::http::header::RETRY_AFTER, seconds.into());
        }
        response
    }
}

impl From<sqlx::Error> for AppError {
    fn from(error: sqlx::Error) -> Self {
        Self::database(error)
    }
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    use super::AppError;

    #[test]
    fn error_statuses_are_stable() {
        assert_eq!(AppError::Unauthorized.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            AppError::validation("bad input").status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(AppError::conflict("stale").status(), StatusCode::CONFLICT);
        let refused = AppError::forbidden("contact_not_allowed", "si:x has not allowed c:y");
        assert_eq!(refused.status(), StatusCode::FORBIDDEN);
        assert_eq!(refused.code(), "contact_not_allowed");
        assert_eq!(refused.to_string(), "si:x has not allowed c:y");
        assert_eq!(
            AppError::account_not_found("c:nobody").code(),
            "account_not_found"
        );
    }

    #[test]
    fn a_database_that_fails_for_a_moment_is_a_retryable_503() {
        for error in [sqlx::Error::PoolTimedOut, sqlx::Error::PoolClosed] {
            let error = AppError::from(error);
            assert_eq!(error.status(), StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(error.code(), "dependency_unavailable");
            assert_eq!(error.retry_after(), Some(1));
            assert!(
                error
                    .hint()
                    .is_some_and(|hint| hint.contains("Idempotency-Key")),
                "{error}"
            );
        }
        let other = AppError::from(sqlx::Error::RowNotFound);
        assert_eq!(other.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(other.code(), "internal_error");
    }

    #[tokio::test]
    async fn refusals_carry_hints_details_and_retry_after() -> Result<(), Box<dyn std::error::Error>>
    {
        use axum::response::IntoResponse as _;
        let error = AppError::refused(
            StatusCode::TOO_MANY_REQUESTS,
            "speech_rate_limited",
            "Too many renders.",
        )
        .with_hint("Wait a minute.")
        .with_details(serde_json::json!({"limit": 30}))
        .with_retry_after(0);
        assert_eq!(error.retry_after(), Some(1), "never a zero delay");
        assert_eq!(error.hint(), Some("Wait a minute."));
        let response = error.into_response();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers()["retry-after"], "1");
        let body: serde_json::Value =
            serde_json::from_slice(&axum::body::to_bytes(response.into_body(), 4096).await?)?;
        assert_eq!(body["error"]["hint"], "Wait a minute.");
        assert_eq!(body["error"]["details"]["limit"], 30);
        let plain = AppError::validation("bad")
            .with_hint("ignored")
            .into_response();
        let body: serde_json::Value =
            serde_json::from_slice(&axum::body::to_bytes(plain.into_body(), 4096).await?)?;
        assert!(body["error"].get("hint").is_none(), "{body}");
        Ok(())
    }
}
