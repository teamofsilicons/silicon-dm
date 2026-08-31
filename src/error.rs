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
    /// Requested aggregate does not exist in the caller's scope.
    #[error("resource not found")]
    NotFound,
    /// Request is structurally or semantically invalid.
    #[error("{0}")]
    Validation(String),
    /// Mutation conflicts with current durable state.
    #[error("{0}")]
    Conflict(String),
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
            Self::NotFound => "not_found",
            Self::Validation(_) => "validation_error",
            Self::Conflict(_) => "conflict",
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
            Self::Conflict(_) => StatusCode::CONFLICT,
            Self::PreconditionRequired(_) => StatusCode::PRECONDITION_REQUIRED,
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
    }
}
