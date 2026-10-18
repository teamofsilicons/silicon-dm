//! Authentication and validated-header extractors.

use std::{future::Future, ops::Deref, str::FromStr as _};

use axum::{
    Json,
    extract::{FromRequest, FromRequestParts, Path, Query, Request, rejection::JsonRejection},
    http::{HeaderMap, HeaderName, Method, StatusCode, request::Parts},
    response::{IntoResponse, Response},
};
use secrecy::SecretString;
use serde::{Serialize, de::DeserializeOwned};

use crate::{
    AppError, AppResult,
    application::{
        auth::AuthContext,
        ports::{AuthRequirement, PresentedCredential},
        state::AppState,
    },
    domain::IdempotencyKey,
};

static IDEMPOTENCY_KEY: HeaderName = HeaderName::from_static("idempotency-key");
static IF_MATCH: HeaderName = HeaderName::from_static("if-match");

/// A request authenticated with Silicon Accounts: the account's own access token
/// (`Authorization: Bearer`), or a User verification proof from another app
/// (`Authorization: Proof sap_…`) on the routes that accept one.
pub struct Authenticated(pub AuthContext);

pub use crate::domain::ConversationAccess;

/// Validated required idempotency header.
pub struct Idempotency(pub IdempotencyKey);

/// Validated optional idempotency header (routes that are idempotent by themselves).
pub struct OptionalIdempotency(pub Option<IdempotencyKey>);

/// A JSON envelope whose `type` the handler checks itself (a route that accepts several).
pub struct ApiEnvelope<T> {
    /// The envelope's `type`.
    pub kind: String,
    /// The envelope's `data`.
    pub data: T,
}

/// Parsed optimistic-concurrency version.
pub struct IfMatch(pub Option<i64>);

/// Path parameters whose deserialization failures use the public error shape.
pub struct ApiPath<T>(pub T);

#[derive(Clone)]
struct ResolvedPath(std::collections::HashMap<String, String>);

/// Query parameters whose deserialization failures use the public error shape.
pub struct ApiQuery<T>(pub T);

/// JSON input whose parser failures use the public error shape and status.
pub struct ApiJson<T>(pub T);

/// Safe, machine-readable rejection for malformed request input: what was wrong with the
/// request (the parser's own words about the client's input, never DM's internals) and
/// where the route's shape is described.
#[derive(Debug)]
pub struct ApiInputRejection {
    status: StatusCode,
    code: &'static str,
    message: String,
    hint: Option<&'static str>,
}

#[derive(Serialize)]
struct InputErrorEnvelope {
    error: InputErrorDetail,
}

#[derive(Serialize)]
struct InputErrorDetail {
    code: &'static str,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    hint: Option<&'static str>,
}

/// Where every route's request shape is described.
const SHAPE_HINT: &str =
    "openapi.yaml (dm docs openapi) shows this route's fields, with an example for every route.";

/// The parser's words about the input, kept short (they may quote the input itself), without
/// axum's own preface ("Failed to deserialize query string: …").
fn detail(error: &impl std::fmt::Display) -> String {
    let text = error.to_string();
    let text = match text.split_once(": ") {
        Some((preface, rest)) if preface.starts_with("Failed to ") => rest,
        _ => text.as_str(),
    }
    .trim_end_matches('.');
    let mut short: String = text.chars().take(300).collect();
    if short.len() < text.len() {
        short.push('…');
    }
    short
}

impl Deref for Authenticated {
    type Target = AuthContext;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// The proof scope that lets another app call a route, and whether the route is
/// sensitive enough to check the sign-in with Silicon Accounts live.
pub(crate) fn route_requirement(method: &Method, path: &str) -> AuthRequirement {
    let operation = silicon_dm_protocol::http_type(method.as_str(), path);
    let read = *method == Method::GET;
    let proof_scope = match operation {
        "conversations" | "conversation" | "group" if read => Some("dm.conversations.read"),
        // The sync feed only references messages the account can already read, so an app
        // that may read them can follow new ones (Ting is off by default). Listening reads
        // a message too; its renders are charged to the account either way.
        "messages" | "message" | "bundle" | "sync" | "speech" if read => Some("dm.messages.read"),
        "create_conversation" | "message.create" | "message.updated" | "message.deleted" => {
            Some("dm.messages.write")
        }
        "receipt" | "receipts" => Some("dm.receipts.write"),
        "draft" | "put_draft" | "delete_draft" => Some("dm.drafts.write"),
        _ => None,
    };
    let live = matches!(
        operation,
        "update_group"
            | "remove_group_members"
            | "add_group_owners"
            | "remove_group_owners"
            | "message.deleted"
            | "allow_list_add"
            | "allow_list_remove"
    );
    AuthRequirement { proof_scope, live }
}

impl FromRequestParts<AppState> for Authenticated {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let secret;
        let credential = match presented_credential(&parts.headers)? {
            (Scheme::Bearer, token) => {
                secret = token;
                PresentedCredential::Bearer(&secret)
            }
            (Scheme::Proof, token) => {
                secret = token;
                PresentedCredential::Proof(&secret)
            }
        };
        let requirement = route_requirement(&parts.method, parts.uri.path());
        let context = state.identity.authenticate(credential, requirement).await?;
        state.directory().admit(&context).await?;
        if state.ting_enabled() && state.accounts.ting_check_due(context.actor.id.as_str()) {
            crate::infrastructure::ting_enrollment::spawn_enrolment(state, &context);
        }
        // Addresses are resolved to internal keys, and access is decided, before any
        // handler runs. A conversation the caller may not see is reported as not found.
        if let Ok(Path(mut parameters)) =
            Path::<std::collections::HashMap<String, String>>::from_request_parts(parts, state)
                .await
        {
            let create = parts.method == Method::POST && parts.uri.path().ends_with("/messages");
            if let Some(value) = parameters.get("conversation_id").cloned() {
                let (id, access) =
                    super::access::resolve_conversation_path(state, &context, &value, create)
                        .await?;
                if access == ConversationAccess::Invitee {
                    return Err(super::groups::invitation_pending());
                }
                if access == ConversationAccess::Custodian
                    && parts.method != Method::GET
                    && parts.method != Method::DELETE
                {
                    if silicon_dm_protocol::http_type(parts.method.as_str(), parts.uri.path())
                        == "receipts"
                        && access == ConversationAccess::Custodian
                    {
                        return Err(super::receipts::read_only_view());
                    }
                    return Err(custodian_read_only(&parts.method));
                }
                parameters.insert("conversation_id".into(), id.to_string());
                parts.extensions.insert(access);
            }
            if let Some(value) = parameters.get("group_id").cloned() {
                let (id, access) =
                    super::access::resolve_group_path(state, &context, &value).await?;
                parameters.insert("group_id".into(), id.to_string());
                parts.extensions.insert(access);
            }
            if let (Some(conversation), Some(message)) = (
                parameters.get("conversation_id"),
                parameters.get("message_id"),
            ) {
                let conversation = conversation.parse().map_err(|_| AppError::NotFound)?;
                let id = state
                    .store
                    .resolve_message_id(conversation, message)
                    .await?;
                parameters.insert("message_id".into(), id.to_string());
            }
            if let (Some(conversation), Some(bundle)) = (
                parameters.get("conversation_id"),
                parameters.get("bundle_id"),
            ) {
                let id = state
                    .store
                    .resolve_bundle_id(
                        conversation.parse().map_err(|_| AppError::NotFound)?,
                        bundle,
                    )
                    .await?;
                parameters.insert("bundle_id".into(), id.to_string());
            }
            parts.extensions.insert(ResolvedPath(parameters));
        }
        parts.extensions.insert(context.clone());
        parts.extensions.insert(state.store.clone());
        Ok(Self(context))
    }
}

fn custodian_read_only(method: &Method) -> AppError {
    AppError::forbidden(
        "custodian_read_only",
        format!(
            "You can read this conversation because you are custodian of a Silicon in it, but you are not a participant: DM never lets a custodian {method} as its Silicon. Only tombstoning that Silicon's own messages is allowed."
        ),
    )
}

/// The conversation access the extractor decided for this request.
impl<S: Send + Sync> FromRequestParts<S> for crate::domain::ConversationAccess {
    type Rejection = AppError;

    fn from_request_parts(
        parts: &mut Parts,
        _state: &S,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        std::future::ready(
            parts
                .extensions
                .get::<Self>()
                .copied()
                .ok_or(AppError::NotFound),
        )
    }
}

impl FromRequestParts<AppState> for Idempotency {
    type Rejection = AppError;

    fn from_request_parts(
        parts: &mut Parts,
        _state: &AppState,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        let result = optional_single_header(&parts.headers, &IDEMPOTENCY_KEY).and_then(|value| {
            let Some(value) = value else {
                return Err(AppError::invalid(
                    "This change needs an Idempotency-Key header, so that a retry cannot make it twice.",
                )
                .with_hint(IDEMPOTENCY_HINT));
            };
            IdempotencyKey::from_str(value)
                .map(Self)
                .map_err(|error| invalid_idempotency_key(&error))
        });
        std::future::ready(result)
    }
}

impl FromRequestParts<AppState> for OptionalIdempotency {
    type Rejection = AppError;

    fn from_request_parts(
        parts: &mut Parts,
        _state: &AppState,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        let result = optional_single_header(&parts.headers, &IDEMPOTENCY_KEY).and_then(|value| {
            value
                .map(|value| {
                    IdempotencyKey::from_str(value).map_err(|error| invalid_idempotency_key(&error))
                })
                .transpose()
                .map(Self)
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
            let version = unquoted
                .parse::<i64>()
                .ok()
                .filter(|version| *version >= 0)
                .ok_or_else(|| {
                    AppError::invalid(
                        "If-Match must be the version you last read: a whole number such as 3 (quoted or not).",
                    )
                    .with_hint(
                        "Read the draft or the group again (its version field), then send that version.",
                    )
                })?;
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
        if let Some(parameters) = parts.extensions.get::<ResolvedPath>() {
            return serde_json::from_value(serde_json::json!(parameters.0))
                .map(Self)
                .map_err(|error| {
                    ApiInputRejection::validation(format!(
                        "The path does not fit this route: {}.",
                        detail(&error)
                    ))
                });
        }
        Path::<T>::from_request_parts(parts, state)
            .await
            .map(|Path(value)| Self(value))
            .map_err(|rejection| {
                ApiInputRejection::validation(format!(
                    "The path does not fit this route: {}.",
                    detail(&rejection.body_text())
                ))
            })
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
            .map_err(|rejection| {
                ApiInputRejection::validation(format!(
                    "The query string does not fit this route: {}.",
                    detail(&rejection.body_text())
                ))
            })
    }
}

impl<S, T> FromRequest<S> for ApiJson<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = ApiInputRejection;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        let expected =
            silicon_dm_protocol::http_type(request.method().as_str(), request.uri().path());
        let store = request
            .extensions()
            .get::<crate::infrastructure::postgres::PostgresStore>()
            .cloned();
        let context = request.extensions().get::<AuthContext>().cloned();
        let conversation = request
            .extensions()
            .get::<ResolvedPath>()
            .and_then(|p| p.0.get("conversation_id"))
            .and_then(|v| v.parse().ok());
        let Json(mut envelope) =
            Json::<silicon_dm_protocol::Envelope<serde_json::Value>>::from_request(request, state)
                .await
                .map_err(|rejection| ApiInputRejection::from_json(&rejection))?;
        if envelope.kind != expected
            && !(expected == "message.create" && envelope.kind == "message.created")
        {
            return Err(ApiInputRejection::validation(format!(
                "This route takes the request type \"{expected}\", but the request's type is \"{}\".",
                envelope.kind.chars().take(64).collect::<String>()
            ))
            .with_hint("Send {\"type\": …, \"data\": {…}} with the type this route names; openapi.yaml (dm docs openapi) has an example for every route."));
        }
        if let (Some(store), Some(context), Some(conversation)) = (store, context, conversation) {
            let _ = &context;
            store
                .resolve_message_input(conversation, &mut envelope.data)
                .await
                .map_err(|_| {
                    ApiInputRejection::validation(
                        "A message this request refers to (its reply) is not in this conversation, or its code is not a message code such as 00a.",
                    )
                    .with_hint("dm messages list CONVERSATION shows each message with its code.")
                })?;
        }
        serde_json::from_value(envelope.data)
            .map(Self)
            .map_err(|error| invalid_data(&error))
    }
}

fn invalid_data(error: &serde_json::Error) -> ApiInputRejection {
    ApiInputRejection::validation(format!(
        "The request's data does not fit this route: {}.",
        detail(error)
    ))
    .with_hint(SHAPE_HINT)
}

impl<S, T> FromRequest<S> for ApiEnvelope<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = ApiInputRejection;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        let Json(envelope) =
            Json::<silicon_dm_protocol::Envelope<serde_json::Value>>::from_request(request, state)
                .await
                .map_err(|rejection| ApiInputRejection::from_json(&rejection))?;
        let data = serde_json::from_value(envelope.data).map_err(|error| invalid_data(&error))?;
        Ok(Self {
            kind: envelope.kind,
            data,
        })
    }
}

impl ApiInputRejection {
    fn validation(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code: "validation_error",
            message: message.into(),
            hint: None,
        }
    }

    fn with_hint(mut self, hint: &'static str) -> Self {
        self.hint = Some(hint);
        self
    }

    fn from_json(rejection: &JsonRejection) -> Self {
        let status = rejection.status();
        if status == StatusCode::UNSUPPORTED_MEDIA_TYPE {
            return Self {
                status,
                code: "unsupported_media_type",
                message: "The body must be JSON, sent with Content-Type: application/json.".into(),
                hint: Some("Add the header Content-Type: application/json."),
            };
        }
        if status == StatusCode::PAYLOAD_TOO_LARGE {
            return Self {
                status,
                code: "payload_too_large",
                message: "The body is larger than this DM service accepts (DM_MAX_HTTP_BODY_BYTES, 128 MiB by default).".into(),
                hint: Some("Send less: attachments are links to files, never the files themselves."),
            };
        }
        if let JsonRejection::JsonDataError(error) = rejection {
            return Self::validation(format!(
                "The body must be one JSON envelope, {{\"type\": …, \"data\": …}}, with nothing else: {}.",
                detail(&error.body_text())
            ))
            .with_hint(SHAPE_HINT);
        }
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_json",
            message: format!(
                "The body is not valid JSON: {}.",
                detail(&rejection.body_text())
            ),
            hint: Some("Send one JSON envelope, {\"type\": …, \"data\": {…}}."),
        }
    }
}

impl IntoResponse for ApiInputRejection {
    fn into_response(self) -> Response {
        let body = InputErrorEnvelope {
            error: InputErrorDetail {
                code: self.code,
                message: self.message,
                hint: self.hint,
            },
        };
        (self.status, Json(body)).into_response()
    }
}

/// Which credential scheme the request used.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Scheme {
    /// `Authorization: Bearer <access token>`.
    Bearer,
    /// `Authorization: Proof <sap_…>`.
    Proof,
}

fn parse_credential(authorization: &str) -> AppResult<(Scheme, SecretString)> {
    let (scheme, token) = if let Some(token) = authorization.strip_prefix("Bearer ") {
        (Scheme::Bearer, token)
    } else if let Some(token) = authorization.strip_prefix("Proof ") {
        (Scheme::Proof, token)
    } else {
        return Err(missing_credential());
    };
    if token.is_empty()
        || token.contains(',')
        || token.chars().any(char::is_whitespace)
        || token.chars().any(char::is_control)
    {
        return Err(missing_credential());
    }
    Ok((scheme, SecretString::from(token.to_owned())))
}

fn missing_credential() -> AppError {
    AppError::unauthenticated(
        "unauthorized",
        "Send `Authorization: Bearer <Silicon Accounts access token for dm>` (sign in with `dm login`, or through the DM website). Apps acting for an account send `Authorization: Proof <sap_… User verification proof>`.",
    )
}

pub(super) fn presented_credential(headers: &HeaderMap) -> AppResult<(Scheme, SecretString)> {
    let authorization = optional_single_header(headers, &axum::http::header::AUTHORIZATION)
        .map_err(|_| missing_credential())?
        .ok_or_else(missing_credential)?;
    parse_credential(authorization)
}

/// How to send an idempotency key (every change takes one).
const IDEMPOTENCY_HINT: &str = "Send Idempotency-Key: one new value per change, 8 to 255 visible ASCII characters (a UUID works), and the same value on every retry of that change.";

fn invalid_idempotency_key(error: &crate::domain::IdempotencyKeyError) -> AppError {
    AppError::invalid(format!(
        "The Idempotency-Key header is not usable: {error}."
    ))
    .with_hint(IDEMPOTENCY_HINT)
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
    use axum::http::{HeaderMap, HeaderValue, Method};

    use super::{
        IDEMPOTENCY_KEY, Scheme, optional_single_header, parse_credential, presented_credential,
        route_requirement,
    };

    #[test]
    fn credentials_are_one_exact_bearer_or_proof() {
        assert!(matches!(
            parse_credential("Bearer eyJ.a.b"),
            Ok((Scheme::Bearer, _))
        ));
        assert!(matches!(
            parse_credential("Proof sap_abc"),
            Ok((Scheme::Proof, _))
        ));
        for invalid in [
            "bearer token",
            "Bearer",
            "Bearer ",
            "Bearer  token",
            "Bearer token second",
            "Bearer one,two",
            "Basic ZG06c2VjcmV0",
            "Proof ",
        ] {
            assert!(parse_credential(invalid).is_err(), "accepted {invalid}");
        }
    }

    #[test]
    fn duplicate_security_headers_are_rejected() {
        let mut headers = HeaderMap::new();
        headers.append(&IDEMPOTENCY_KEY, HeaderValue::from_static("abcdefgh"));
        headers.append(&IDEMPOTENCY_KEY, HeaderValue::from_static("ijklmnop"));
        assert!(optional_single_header(&headers, &IDEMPOTENCY_KEY).is_err());

        let mut duplicate_bearers = HeaderMap::new();
        duplicate_bearers.append(
            axum::http::header::AUTHORIZATION,
            HeaderValue::from_static("Bearer first"),
        );
        duplicate_bearers.append(
            axum::http::header::AUTHORIZATION,
            HeaderValue::from_static("Bearer second"),
        );
        assert!(presented_credential(&duplicate_bearers).is_err());
        assert!(presented_credential(&HeaderMap::new()).is_err());
    }

    #[test]
    fn proofs_are_accepted_only_on_the_documented_routes() {
        let scope = |method: Method, path: &str| route_requirement(&method, path).proof_scope;
        assert_eq!(
            scope(Method::GET, "/api/v1/conversations"),
            Some("dm.conversations.read")
        );
        assert_eq!(
            scope(Method::GET, "/api/v1/conversations/c:a::si:b/messages"),
            Some("dm.messages.read")
        );
        assert_eq!(
            scope(Method::POST, "/api/v1/conversations/c:a::si:b/messages"),
            Some("dm.messages.write")
        );
        assert_eq!(
            scope(Method::POST, "/api/v1/messages"),
            Some("dm.messages.write")
        );
        assert_eq!(
            scope(
                Method::POST,
                "/api/v1/conversations/x/messages/00a/receipts"
            ),
            Some("dm.receipts.write")
        );
        assert_eq!(
            scope(Method::PUT, "/api/v1/conversations/x/draft"),
            Some("dm.drafts.write")
        );
        assert_eq!(scope(Method::GET, "/api/v1/sync"), Some("dm.messages.read"));
        for (method, path) in [
            (Method::POST, "/api/v1/groups"),
            (Method::PATCH, "/api/v1/groups/g:team"),
            (Method::GET, "/api/v1/me"),
            (Method::POST, "/api/v1/allow-list"),
            (Method::PUT, "/api/v1/presence/devices/laptop"),
            (Method::GET, "/api/v1/presence/c:ada"),
            (Method::GET, "/api/v1/gifs/recent"),
        ] {
            assert_eq!(scope(method.clone(), path), None, "{method} {path}");
        }
        assert!(route_requirement(&Method::DELETE, "/api/v1/conversations/x/messages/00a").live);
        assert!(route_requirement(&Method::PATCH, "/api/v1/groups/g:team").live);
        assert!(!route_requirement(&Method::GET, "/api/v1/conversations").live);
    }
}
