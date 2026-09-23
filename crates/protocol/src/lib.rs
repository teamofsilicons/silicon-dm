//! Shared external JSON contract. HTTP methods, headers, and empty responses
//! retain their normal semantics; every JSON document has exactly type and data.
use serde::{Deserialize, Serialize};

pub const WEBSOCKET_VERSION: u16 = 5;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope<T> {
    #[serde(rename = "type")]
    pub kind: String,
    pub data: T,
}

impl<T> Envelope<T> {
    pub fn new(kind: impl Into<String>, data: T) -> Self {
        Self {
            kind: kind.into(),
            data,
        }
    }
}

/// Stable operation discriminator for REST requests.
pub fn http_type(method: &str, path: &str) -> &'static str {
    let path = path.split('?').next().unwrap_or(path);
    let path = path
        .strip_prefix("/api/v1/")
        .unwrap_or(path)
        .trim_matches('/');
    let p: Vec<_> = path.split('/').collect();
    match (method, p.as_slice()) {
        ("POST", ["messages"]) => "message.create",
        (_, ["iam"]) => "iam",
        (_, ["contracts"]) => "contracts",
        (_, ["sync"]) => "sync",
        (_, ["delivery", "registration"]) => "delivery_registration",
        ("PUT", ["presence", "devices", _]) => "renew_presence",
        ("DELETE", ["presence", "devices", _]) => "close_presence",
        (_, ["reports"]) => "report",
        (_, ["telemetry"]) => "telemetry",
        (_, ["auth", "login"]) => "login",
        (_, ["auth", "refresh"]) => "refresh",
        (_, ["auth", "logout"]) => "logout",
        (_, ["auth", "me"]) => "me",
        ("POST", ["groups"]) => "create_group",
        (_, ["groups"]) => "groups",
        ("PATCH", ["groups", _]) => "update_group",
        (_, ["groups", _]) => "group",
        ("DELETE", ["groups", _, "members"]) => "remove_group_members",
        (_, ["groups", _, "members"]) => "invite_group_members",
        ("POST", ["conversations"]) => "create_conversation",
        (_, ["conversations"]) => "conversations",
        ("POST", ["conversations", _, "messages"]) => "message.create",
        (_, ["conversations", _, "messages"]) => "messages",
        ("PATCH", ["conversations", _, "messages", _]) => "message.updated",
        ("DELETE", ["conversations", _, "messages", _]) => "message.deleted",
        (_, ["conversations", _, "messages", _]) => "message",
        (_, ["conversations", _, "messages", _, "receipts"]) => "receipt",
        (_, ["conversations", _, "bundles"]) => "create_bundle",
        (_, ["conversations", _, "bundles", _]) => "bundle",
        ("PUT", ["conversations", _, "draft"]) => "put_draft",
        ("DELETE", ["conversations", _, "draft"]) => "delete_draft",
        (_, ["conversations", _, "draft"]) => "draft",
        (_, ["presence", _]) => "presence",
        (_, ["gifs", _]) => "gifs",
        ("POST", ["testing-environments"]) => "create_testing_environment",
        (_, ["testing-environments"]) => "testing_environments",
        ("PATCH", ["testing-environments", _]) => "update_testing_environment",
        ("DELETE", ["testing-environments", _]) => "delete_testing_environment",
        (_, ["testing-environments", _]) => "testing_environment",
        (_, ["testing-environments", _, "key"]) => "testing_environment_key",
        (_, ["testing-environments", _, "rotate-key"]) => "rotate_testing_environment_key",
        (_, ["testing-environments", _, "restore"]) => "restore_testing_environment",
        (_, ["testing-environments", _, "clean"]) => "clean_testing_environment",
        (_, ["requests"]) => "request",
        (_, ["requests", _, "status"]) => "request_status",
        (_, ["requests", _]) => "request_result",
        (_, ["status"]) => "relay_status",
        (_, ["shutdown"]) => "shutdown",
        _ => "response",
    }
}

/// Successful REST response discriminator, distinct from the creation request.
pub fn http_response_type(method: &str, path: &str) -> &'static str {
    match http_type(method, path) {
        "message.create" => "message.create.successful",
        other => other,
    }
}

/// Creation delivery for every recipient, including the sender's devices.
///
/// The arguments remain accepted for callers compiled against the existing API.
/// Command acceptance uses `message.create.successful` separately.
pub fn message_creation_event(_sender: &str, _recipient: &str) -> &'static str {
    "message.created"
}

/// Wrap serialized HTTP responses without copying or parsing large message bodies.
#[cfg(feature = "http")]
pub async fn responses(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::{
        body::{Body, Bytes},
        http::header,
    };
    use futures_util::{StreamExt, stream};
    let kind = http_response_type(request.method().as_str(), request.uri().path());
    let response = next.run(request).await;
    if !response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .is_some_and(|h| h.starts_with("application/json"))
    {
        if response.status().is_client_error() || response.status().is_server_error() {
            use axum::response::IntoResponse;
            let (parts, _) = response.into_parts();
            let body = axum::Json(Envelope::new("error", serde_json::json!({"error":{
                "code":"http_error", "message":parts.status.canonical_reason().unwrap_or("request failed")
            }}))).into_response().into_body();
            let mut response = axum::response::Response::from_parts(parts, body);
            response.headers_mut().remove(header::CONTENT_LENGTH);
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                header::HeaderValue::from_static("application/json"),
            );
            return response;
        }
        return response;
    }
    let kind = if response.status().is_success() {
        kind
    } else {
        "error"
    };
    let (mut parts, body) = response.into_parts();
    parts.headers.remove(header::CONTENT_LENGTH);
    let prefix = Bytes::from(format!("{{\"type\":\"{kind}\",\"data\":"));
    let stream = stream::once(async { Ok::<_, axum::Error>(prefix) })
        .chain(body.into_data_stream())
        .chain(stream::once(async { Ok(Bytes::from_static(b"}")) }));
    axum::response::Response::from_parts(parts, Body::from_stream(stream))
}

/// Canonical public group address: g:<organization>:<lowercase-name-slug>.
pub fn valid_group_id(value: &str) -> bool {
    let mut parts = value.split(':');
    if parts.next() != Some("g") {
        return false;
    }
    let (Some(org), Some(slug)) = (parts.next(), parts.next()) else {
        return false;
    };
    parts.next().is_none()
        && org
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && org.len() <= 255
        && org
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        && !slug.is_empty()
        && slug.len() <= 140
        && !slug.starts_with('-')
        && !slug.ends_with('-')
        && !slug.contains("--")
        && slug
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}

#[cfg(test)]
mod group_id_tests {
    #[test]
    fn canonical_group_addresses_are_bounded_and_path_safe() {
        for id in [
            "g:tos:product-design",
            "g:other-org:product-design",
            "g:tos:research-2",
            "g:tos:123",
        ] {
            assert!(super::valid_group_id(id), "{id}");
        }
        for id in [
            "g:tos-product-design",
            "g:tos:Product-Design",
            "g:tos:bad--slug",
            "g:tos:-bad",
            "g:tos:bad-",
            "g:tos:",
            "g::name",
            "g:tos:name:extra",
            "g:tos:a/b",
            "g:tos:foo?bar",
            "g:tos:foo%2fbar",
            "g:_:name",
        ] {
            assert!(!super::valid_group_id(id), "{id}");
        }
    }
}

/// Conversation-local base-36 ID, padded to at least three characters.
/// Internal message sequences start at one; public identifiers start at 000.
pub fn message_code(sequence: i64) -> Option<String> {
    let mut value = u64::try_from(sequence.checked_sub(1)?).ok()?;
    let mut digits = Vec::new();
    loop {
        digits.push(b"0123456789abcdefghijklmnopqrstuvwxyz"[(value % 36) as usize]);
        value /= 36;
        if value == 0 {
            break;
        }
    }
    while digits.len() < 3 {
        digits.push(b'0');
    }
    digits.reverse();
    String::from_utf8(digits).ok()
}

/// Decode only canonical IDs; reject aliases such as 0000, uppercase or overflow.
pub fn message_sequence(code: &str) -> Option<i64> {
    if !(3..=13).contains(&code.len()) {
        return None;
    }
    let mut value = 0_i64;
    for byte in code.bytes() {
        let digit = match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'z' => byte - b'a' + 10,
            _ => return None,
        };
        value = value.checked_mul(36)?.checked_add(i64::from(digit))?;
    }
    let sequence = value.checked_add(1)?;
    (message_code(sequence)?.as_str() == code).then_some(sequence)
}

#[cfg(test)]
mod message_code_tests {
    #[test]
    fn boundaries_and_canonical_round_trips() {
        for (n, code) in [
            (1, "000"),
            (10, "009"),
            (11, "00a"),
            (36, "00z"),
            (37, "010"),
            (46656, "zzz"),
            (46657, "1000"),
            (1679616, "zzzz"),
            (1679617, "10000"),
        ] {
            assert_eq!(super::message_code(n).as_deref(), Some(code));
            assert_eq!(super::message_sequence(code), Some(n));
        }
        assert_eq!(
            super::message_sequence(&super::message_code(i64::MAX).unwrap()),
            Some(i64::MAX)
        );
        for code in ["", "00", "0000", "ABC", "a/b", "zzzzzzzzzzzzz"] {
            assert_eq!(super::message_sequence(code), None);
        }
        assert_eq!(super::message_code(0), None);
    }
}

/// Conversation-local bundle ID starting at 001, growing past zzz without truncation.
pub fn bundle_code(sequence: i64) -> Option<String> {
    if sequence < 1 {
        return None;
    }
    message_code(sequence.checked_add(1)?)
}
/// Decode canonical positive bundle codes.
pub fn bundle_sequence(code: &str) -> Option<i64> {
    let sequence = message_sequence(code)?.checked_sub(1)?;
    (sequence > 0).then_some(sequence)
}

#[cfg(test)]
mod creation_tests {
    #[test]
    fn delivery_notification_is_distinct_from_command_and_direct_success() {
        for recipient in ["c:alice", "c:bob"] {
            assert_eq!(
                super::message_creation_event("c:alice", recipient),
                "message.created"
            );
        }
        assert_eq!(
            super::http_type("POST", "/api/v1/messages"),
            "message.create"
        );
        assert_eq!(
            super::http_response_type("POST", "/api/v1/messages"),
            "message.create.successful"
        );
    }
}
