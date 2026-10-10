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
    },
    /// Requested aggregate does not exist in the caller's scope.
    #[error("resource not found")]
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
    #[error("dependency {dependency} is unavailable")]
    DependencyUnavailable {
        /// Stable dependency name.
        dependency: &'static str,
    },
    /// Caller exceeded an enforced rate.
    #[error("rate limit exceeded")]
    RateLimited,
    /// Database operation failed. Details remain in server traces.
    #[error("database operation failed")]
    Database(#[source] sqlx::Error),
    /// Unexpected internal failure. Details remain in server traces.
    #[error("internal server error")]
    Internal(#[source] anyhow::Error),
}

#[derive(Debug, Serialize)]
struct ErrorEnvelope {
    error: ErrorDetail,
}

#[derive(Debug, Serialize)]
struct ErrorDetail {
    code: &'static str,
    message: String,
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
        }
    }

    /// 401 with a precise reason (expired, wrong audience, revoked sign-in…).
    #[must_use]
    pub fn unauthenticated(code: &'static str, message: impl Into<String>) -> Self {
        Self::refused(StatusCode::UNAUTHORIZED, code, message)
    }

    /// 403 with a precise reason.
    #[must_use]
    pub fn forbidden(code: &'static str, message: impl Into<String>) -> Self {
        Self::refused(StatusCode::FORBIDDEN, code, message)
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
        let body = ErrorEnvelope {
            error: ErrorDetail {
                code: self.code(),
                message: self.public_message(),
            },
        };
        (status, Json(body)).into_response()
    }
}

impl From<sqlx::Error> for AppError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error)
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
}
