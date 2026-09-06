//! Authentication and validated-header extractors.

use std::{future::Future, ops::Deref, str::FromStr as _};

use axum::{
    Json,
    extract::{FromRequest, FromRequestParts, Path, Query, Request, rejection::JsonRejection},
    http::{HeaderMap, HeaderName, StatusCode, request::Parts},
    response::{IntoResponse, Response},
};
use secrecy::SecretString;
use serde::{Serialize, de::DeserializeOwned};

use crate::{
    AppError, AppResult,
    application::{auth::AuthContext, ports::AuthenticationRequest, state::AppState},
    domain::{IdempotencyKey, OrganizationId},
};

static ORG_ID: HeaderName = HeaderName::from_static("x-org-id");
static OBO_PROOF: HeaderName = HeaderName::from_static("x-iam-obo-access-proof");
static IDEMPOTENCY_KEY: HeaderName = HeaderName::from_static("idempotency-key");
static IF_MATCH: HeaderName = HeaderName::from_static("if-match");

/// IAM-authenticated actor request.
pub struct Authenticated(pub AuthContext);

/// Validated required idempotency header.
pub struct Idempotency(pub IdempotencyKey);

/// Parsed optimistic-concurrency version.
pub struct IfMatch(pub Option<i64>);

/// Path parameters whose deserialization failures use the public error shape.
pub struct ApiPath<T>(pub T);

/// Query parameters whose deserialization failures use the public error shape.
pub struct ApiQuery<T>(pub T);

/// JSON input whose parser failures use the public error shape and status.
pub struct ApiJson<T>(pub T);

/// Safe, machine-readable rejection for malformed request input.
#[derive(Debug)]
pub struct ApiInputRejection {
    status: StatusCode,
    code: &'static str,
    message: &'static str,
}

#[derive(Serialize)]
struct InputErrorEnvelope {
    error: InputErrorDetail,
}

#[derive(Serialize)]
struct InputErrorDetail {
    code: &'static str,
    message: &'static str,
}

impl Deref for Authenticated {
    type Target = AuthContext;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl FromRequestParts<AppState> for Authenticated {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let organization_id = parse_organization_id(&parts.headers)?;
        let token = realtime_bearer(&parts.headers)?;
        let context = state
            .identity
            .authenticate(AuthenticationRequest::Bearer {
                token: &token,
                organization_id: &organization_id,
            })
            .await?;

        if context.organization_id != organization_id {
            return Err(AppError::Forbidden);
        }
        Ok(Self(context))
    }
}

impl FromRequestParts<AppState> for Idempotency {
    type Rejection = AppError;

    fn from_request_parts(
        parts: &mut Parts,
        _state: &AppState,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        let result = required_header(&parts.headers, &IDEMPOTENCY_KEY).and_then(|value| {
            IdempotencyKey::from_str(value)
                .map(Self)
                .map_err(|error| AppError::validation(error.to_string()))
        });
        std::future::ready(result)
    }
}

impl FromRequestParts<AppState> for IfMatch {
    type Rejection = AppError;

    fn from_request_parts(
        parts: &mut Parts,
        _state: &AppState,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        let result = optional_single_header(&parts.headers, &IF_MATCH).and_then(|raw| {
            let Some(raw) = raw else {
                return Ok(Self(None));
            };
            let unquoted = match raw
                .strip_prefix('"')
                .and_then(|value| value.strip_suffix('"'))
            {
                Some(value) => value,
                None => raw,
            };
            let version = unquoted.parse::<i64>().map_err(|_| {
                AppError::validation("If-Match must contain a non-negative draft version")
            })?;
            if version < 0 {
                return Err(AppError::validation(
                    "If-Match must contain a non-negative draft version",
                ));
            }
            Ok(Self(Some(version)))
        });
        std::future::ready(result)
    }
}

impl<S, T> FromRequestParts<S> for ApiPath<T>
where
    S: Send + Sync,
    T: DeserializeOwned + Send,
{
    type Rejection = ApiInputRejection;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        Path::<T>::from_request_parts(parts, state)
            .await
            .map(|Path(value)| Self(value))
            .map_err(|_| ApiInputRejection::validation("path parameters are invalid"))
    }
}

impl<S, T> FromRequestParts<S> for ApiQuery<T>
where
    S: Send + Sync,
    T: DeserializeOwned + Send,
{
    type Rejection = ApiInputRejection;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        Query::<T>::from_request_parts(parts, state)
            .await
            .map(|Query(value)| Self(value))
            .map_err(|_| ApiInputRejection::validation("query parameters are invalid"))
    }
}

impl<S, T> FromRequest<S> for ApiJson<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = ApiInputRejection;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        Json::<T>::from_request(request, state)
            .await
            .map(|Json(value)| Self(value))
            .map_err(|rejection| ApiInputRejection::from_json(&rejection))
    }
}

impl ApiInputRejection {
    fn validation(message: &'static str) -> Self {
        Self {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code: "validation_error",
            message,
        }
    }

    fn from_json(rejection: &JsonRejection) -> Self {
        let status = rejection.status();
        if status == StatusCode::UNSUPPORTED_MEDIA_TYPE {
            return Self {
                status,
                code: "unsupported_media_type",
                message: "Content-Type must be application/json",
            };
        }
        if status == StatusCode::PAYLOAD_TOO_LARGE {
            return Self {
                status,
                code: "payload_too_large",
                message: "request body exceeds the configured limit",
            };
        }
        if matches!(rejection, JsonRejection::JsonDataError(_)) {
            return Self::validation("request JSON does not match the required schema");
        }
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_json",
            message: "request body contains invalid JSON",
        }
    }
}

impl IntoResponse for ApiInputRejection {
    fn into_response(self) -> Response {
        let body = InputErrorEnvelope {
            error: InputErrorDetail {
                code: self.code,
                message: self.message,
            },
        };
        (self.status, Json(body)).into_response()
    }
}

fn parse_organization_id(headers: &HeaderMap) -> AppResult<OrganizationId> {
    required_header(headers, &ORG_ID)?
        .parse()
        .map_err(|_| AppError::validation("X-Org-ID contains an invalid organization ID"))
}

fn parse_bearer(authorization: &str) -> AppResult<SecretString> {
    let token = authorization
        .strip_prefix("Bearer ")
        .filter(|token| {
            !token.is_empty()
                && !token.contains(',')
                && !token.chars().any(char::is_whitespace)
                && !token.chars().any(char::is_control)
        })
        .ok_or(AppError::Unauthorized)?;
    Ok(SecretString::from(token.to_owned()))
}

pub(super) fn realtime_bearer(headers: &HeaderMap) -> AppResult<SecretString> {
    if optional_single_header(headers, &OBO_PROOF)
        .map_err(|_| AppError::Unauthorized)?
        .is_some()
    {
        return Err(AppError::Unauthorized);
    }
    let authorization = optional_single_header(headers, &axum::http::header::AUTHORIZATION)
        .map_err(|_| AppError::Unauthorized)?
        .ok_or(AppError::Unauthorized)?;
    parse_bearer(authorization)
}

fn required_header<'a>(headers: &'a HeaderMap, name: &HeaderName) -> AppResult<&'a str> {
    optional_single_header(headers, name)?.ok_or_else(|| {
        AppError::validation(format!("required header {} is missing", name.as_str()))
    })
}

fn optional_single_header<'a>(
    headers: &'a HeaderMap,
    name: &HeaderName,
) -> AppResult<Option<&'a str>> {
    let values = headers.get_all(name);
    let mut iter = values.iter();
    let Some(first) = iter.next() else {
        return Ok(None);
    };
    if iter.next().is_some() {
        return Err(AppError::validation(format!(
            "header {} must be supplied at most once",
            name.as_str()
        )));
    }
    first
        .to_str()
        .map(Some)
        .map_err(|_| AppError::validation(format!("header {} is invalid", name.as_str())))
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue};

    use super::{IDEMPOTENCY_KEY, OBO_PROOF, parse_bearer, realtime_bearer, required_header};

    #[test]
    fn bearer_scheme_is_exact_and_nonempty() {
        assert!(parse_bearer("Bearer opaque").is_ok());
        assert!(parse_bearer("bearer opaque").is_err());
        assert!(parse_bearer("Bearer ").is_err());
        assert!(parse_bearer("Bearer  opaque").is_err());
        assert!(parse_bearer("Bearer opaque token").is_err());
        assert!(parse_bearer("Bearer one,two").is_err());
    }

    #[test]
    fn duplicate_security_headers_are_rejected() {
        let mut headers = HeaderMap::new();
        headers.append(&IDEMPOTENCY_KEY, HeaderValue::from_static("abcdefgh"));
        headers.append(&IDEMPOTENCY_KEY, HeaderValue::from_static("ijklmnop"));
        assert!(required_header(&headers, &IDEMPOTENCY_KEY).is_err());
    }

    #[test]
    fn realtime_auth_rejects_obo_and_duplicate_bearer_headers() {
        let mut obo_headers = HeaderMap::new();
        obo_headers.insert(&OBO_PROOF, HeaderValue::from_static("proof"));
        obo_headers.insert(
            axum::http::header::AUTHORIZATION,
            HeaderValue::from_static("Bearer token"),
        );
        assert!(realtime_bearer(&obo_headers).is_err());

        let mut duplicate_bearers = HeaderMap::new();
        duplicate_bearers.append(
            axum::http::header::AUTHORIZATION,
            HeaderValue::from_static("Bearer first"),
        );
        duplicate_bearers.append(
            axum::http::header::AUTHORIZATION,
            HeaderValue::from_static("Bearer second"),
        );
        assert!(realtime_bearer(&duplicate_bearers).is_err());
    }

    #[test]
    fn realtime_auth_requires_one_exact_bearer_credential() {
        for authorization in [
            "bearer token",
            "Bearer",
            "Bearer ",
            "Bearer  token",
            "Bearer token second",
            "Bearer first,Bearer-second",
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(
                axum::http::header::AUTHORIZATION,
                HeaderValue::from_static(authorization),
            );
            assert!(
                realtime_bearer(&headers).is_err(),
                "accepted {authorization}"
            );
        }

        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            HeaderValue::from_static("Bearer opaque-token"),
        );
        assert!(realtime_bearer(&headers).is_ok());
    }
}
