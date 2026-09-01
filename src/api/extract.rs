//! Authentication and validated-header extractors.

use std::{future::Future, ops::Deref, str::FromStr as _};

use axum::{
    Json,
    extract::{
        FromRequest, FromRequestParts, MatchedPath, Path, Query, Request, rejection::JsonRejection,
    },
    http::{HeaderMap, HeaderName, Method, StatusCode, request::Parts},
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
static APP_ID: HeaderName = HeaderName::from_static("x-app-id");
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
        let bearer = optional_single_header(&parts.headers, &axum::http::header::AUTHORIZATION)?;
        let proof = optional_single_header(&parts.headers, &OBO_PROOF)?;

        let context = match (bearer, proof) {
            (Some(_), Some(_)) => {
                return Err(AppError::validation(
                    "provide either bearer authentication or OBO Access, not both",
                ));
            }
            (Some(authorization), None) => {
                let token = parse_bearer(authorization)?;
                state
                    .identity
                    .authenticate(AuthenticationRequest::Bearer {
                        token: &token,
                        organization_id: &organization_id,
                    })
                    .await?
            }
            (None, Some(proof)) => {
                let app_id = required_header(&parts.headers, &APP_ID)?;
                let proof = SecretString::from(proof.to_owned());
                let matched_path = parts
                    .extensions
                    .get::<MatchedPath>()
                    .map_or(parts.uri.path(), MatchedPath::as_str);
                let action =
                    action_for(&parts.method, matched_path).ok_or(AppError::Unauthorized)?;
                state
                    .identity
                    .authenticate(AuthenticationRequest::Obo {
                        proof: &proof,
                        app_id,
                        organization_id: &organization_id,
                        action,
                        resource: Some(parts.uri.path()),
                    })
                    .await?
            }
            (None, None) => return Err(AppError::Unauthorized),
        };

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

fn action_for(method: &Method, matched_path: &str) -> Option<&'static str> {
    let action = match (method, matched_path) {
        (&Method::GET, "/api/v1/conversations") => "dm.conversations.list",
        (&Method::GET, "/api/v1/conversations/{conversation_id}/messages") => "dm.messages.list",
        (&Method::POST, "/api/v1/conversations/{conversation_id}/messages") => "dm.messages.create",
        (
            &Method::POST,
            "/api/v1/conversations/{conversation_id}/messages/{message_id}/receipts",
        ) => "dm.receipts.create",
        (&Method::POST, "/api/v1/conversations/{conversation_id}/bundles") => "dm.bundles.create",
        (&Method::GET, "/api/v1/conversations/{conversation_id}/bundles/{bundle_id}") => {
            "dm.bundles.read"
        }
        (&Method::GET, "/api/v1/conversations/{conversation_id}/draft") => "dm.drafts.read",
        (&Method::PUT, "/api/v1/conversations/{conversation_id}/draft") => "dm.drafts.write",
        (&Method::DELETE, "/api/v1/conversations/{conversation_id}/draft") => "dm.drafts.delete",
        (&Method::GET, "/api/v1/gifs/trending") => "dm.gifs.trending",
        (&Method::GET, "/api/v1/gifs/search") => "dm.gifs.search",
        (&Method::GET, "/api/v1/gifs/recent") => "dm.gifs.recent",
        _ => return None,
    };
    Some(action)
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue};

    use axum::http::Method;

    use super::{
        IDEMPOTENCY_KEY, OBO_PROOF, action_for, parse_bearer, realtime_bearer, required_header,
    };

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
    fn unknown_obo_routes_have_no_fallback_authority() {
        assert_eq!(action_for(&Method::DELETE, "/api/v1/conversations"), None);
        assert_eq!(action_for(&Method::GET, "/not-a-route"), None);
    }

    #[test]
    fn bearer_only_routes_have_no_obo_authority() {
        assert_eq!(action_for(&Method::POST, "/api/v1/conversations"), None);
        assert_eq!(
            action_for(&Method::GET, "/api/v1/presence/{actor_id}"),
            None
        );
        assert_eq!(
            action_for(&Method::POST, "/api/v1/attachments/temporary-url"),
            None
        );
    }

    #[test]
    fn known_obo_route_has_one_stable_action() {
        assert_eq!(
            action_for(
                &Method::POST,
                "/api/v1/conversations/{conversation_id}/messages"
            ),
            Some("dm.messages.create")
        );
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
