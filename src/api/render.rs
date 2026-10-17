//! The public shape of every JSON response: conversation addresses instead of internal
//! UUIDs, message codes, and accounts rendered from their storage key (uuid) to
//! `{type, id: <current c:/si: id>, uuid, display_name?, pfp_url?}`.
//!
//! Only contract-owned fields are touched; user metadata, text and attachments never are.
use std::collections::{HashMap, HashSet};

use axum::{
    body::{Body, to_bytes},
    extract::{Request, State},
    http::header,
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde_json::{Map, Value};

use crate::{
    AppError, AppResult,
    application::state::AppState,
    domain::{Account, is_account_uuid},
};

/// Keys whose values belong to users, never to the contract.
const USER_CONTENT: [&str; 10] = [
    "metadata",
    "message",
    "text",
    "attachments",
    "attachment_details",
    "voice",
    "gif",
    "speech",
    "script",
    "voice_transcript",
];
/// Lookups DM may make to render one response (unknown accounts are rare).
const MAX_RENDER_LOOKUPS: usize = 20;

pub(super) async fn responses(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path().to_owned();
    let eligible = path.starts_with("/api/v1/") && path != "/api/v1/contracts";
    // Lists and single conversation reads both answer summaries.
    let summary = path == "/api/v1/conversations"
        || (request.method() == axum::http::Method::GET
            && silicon_dm_protocol::http_type("GET", &path) == "conversation");
    let response = next.run(request).await;
    if !eligible
        || !response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|s| s.starts_with("application/json"))
    {
        return response;
    }
    match translate(state, response, summary).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

async fn translate(state: AppState, response: Response, summary: bool) -> AppResult<Response> {
    let (mut parts, body) = response.into_parts();
    let bytes = to_bytes(body, usize::MAX)
        .await
        .map_err(AppError::internal)?;
    let mut value = serde_json::from_slice(&bytes).map_err(AppError::internal)?;
    drop(bytes);
    state.store.public_messages(&mut value).await?;
    state.store.public_conversation_ids(&mut value).await?;
    render_accounts(&state, &mut value).await?;
    if summary {
        conversation_summary(&mut value);
    }
    let bytes = serde_json::to_vec(&value).map_err(AppError::internal)?;
    parts.headers.remove(header::CONTENT_LENGTH);
    Ok(Response::from_parts(parts, Body::from(bytes)))
}

/// Renders every account reference in a response from the accounts cache.
///
/// # Errors
/// Storage failures.
pub(crate) async fn render_accounts(state: &AppState, value: &mut Value) -> AppResult<()> {
    let mut keys = HashSet::new();
    visit(value, &mut |slot| {
        if let Some(key) = slot.storage_key() {
            keys.insert(key.to_owned());
        }
    });
    // Routing strings written before the identity re-key name IAM-era ids; follow their
    // links so they show the account's current id too.
    let legacy: Vec<String> = keys
        .iter()
        .filter(|key| key.contains(':'))
        .cloned()
        .collect();
    let links = state.store.identity_links_for(&legacy).await?;
    let mut keys: Vec<String> = keys
        .into_iter()
        .filter(|key| is_account_uuid(key))
        .collect();
    keys.extend(links.values().cloned());
    if keys.is_empty() {
        return Ok(());
    }
    let mut accounts = state.store.accounts_by_uuid(&keys).await?;
    for missing in keys
        .iter()
        .filter(|key| !accounts.contains_key(*key))
        .take(MAX_RENDER_LOOKUPS)
        .cloned()
        .collect::<Vec<_>>()
    {
        match state.directory().account(&missing).await {
            Ok(account) => {
                accounts.insert(missing, account);
            }
            Err(error) if error.code() == "account_deleted" => {
                if let Some(stored) = state.store.stored_account(&missing).await? {
                    accounts.insert(missing, stored.account());
                }
            }
            Err(error) => {
                tracing::debug!(code = error.code(), "an account could not be rendered");
            }
        }
    }
    for (legacy_id, uuid) in &links {
        if let Some(account) = accounts.get(uuid).cloned() {
            accounts.insert(legacy_id.clone(), account);
        }
    }
    visit(value, &mut |slot| slot.render(&accounts));
    Ok(())
}

/// One place in a response that names an account.
enum Slot<'a> {
    /// `{"type": "carbon"|"silicon", "id": <storage key>}`.
    Reference(&'a mut Map<String, Value>),
    /// A bare id field (`actor_id`, `member_id`) whose sibling `<name>_uuid` is added.
    Field(&'a mut Map<String, Value>, &'static str),
    /// An address string `isi@<storage key>` or `<storage key>`.
    Address(&'a mut Value),
}

impl Slot<'_> {
    fn storage_key(&self) -> Option<&str> {
        match self {
            Self::Reference(object) => object.get("id").and_then(Value::as_str),
            Self::Field(object, field) => object.get(*field).and_then(Value::as_str),
            Self::Address(value) => value
                .as_str()
                .map(|address| address.rsplit_once('@').map_or(address, |(_, base)| base)),
        }
    }

    fn render(&mut self, accounts: &HashMap<String, Account>) {
        match self {
            Self::Reference(object) => {
                let Some(key) = object.get("id").and_then(Value::as_str).map(str::to_owned) else {
                    return;
                };
                match accounts.get(&key) {
                    Some(account) => {
                        object.insert("id".into(), Value::String(account.display_id().to_owned()));
                        object.insert("uuid".into(), Value::String(account.uuid.clone()));
                        if let Some(name) = &account.display_name {
                            object.insert("display_name".into(), Value::String(name.clone()));
                        }
                        if let Some(photo) = &account.pfp_url {
                            object.insert("pfp_url".into(), Value::String(photo.clone()));
                        }
                        if account.deleted {
                            object.insert("deleted".into(), Value::Bool(true));
                        }
                    }
                    None if !is_account_uuid(&key) => {
                        // An IAM-era identity not linked to an account: shown as stored.
                        object.insert("legacy".into(), Value::Bool(true));
                    }
                    None => {
                        object.insert("uuid".into(), Value::String(key));
                    }
                }
            }
            Self::Field(object, field) => {
                let Some(key) = object
                    .get(*field)
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                else {
                    return;
                };
                if let Some(account) = accounts.get(&key) {
                    object.insert(
                        (*field).into(),
                        Value::String(account.display_id().to_owned()),
                    );
                    object.insert(
                        format!("{}_uuid", field.trim_end_matches("_id")),
                        Value::String(account.uuid.clone()),
                    );
                }
            }
            Self::Address(value) => {
                let Some(address) = value.as_str().map(str::to_owned) else {
                    return;
                };
                let (isi, base) = match address.rsplit_once('@') {
                    Some((isi, base)) => (Some(isi), base),
                    None => (None, address.as_str()),
                };
                if let Some(account) = accounts.get(base) {
                    let id = account.display_id();
                    **value = Value::String(
                        isi.map_or_else(|| id.to_owned(), |isi| format!("{isi}@{id}")),
                    );
                }
            }
        }
    }
}

fn is_reference(object: &Map<String, Value>) -> bool {
    object.len() == 2
        && object.get("id").is_some_and(Value::is_string)
        && matches!(
            object.get("type").and_then(Value::as_str),
            Some("carbon" | "silicon")
        )
}

/// Visits every account slot outside user content.
fn visit(value: &mut Value, visitor: &mut impl FnMut(&mut Slot<'_>)) {
    match value {
        Value::Array(items) => {
            for item in items {
                visit(item, visitor);
            }
        }
        Value::Object(object) => {
            if is_reference(object) {
                visitor(&mut Slot::Reference(object));
                return;
            }
            for field in ["actor_id", "member_id"] {
                if object.get(field).is_some_and(Value::is_string) {
                    visitor(&mut Slot::Field(object, field));
                }
            }
            for field in ["sender_id", "recipient_id"] {
                if let Some(address) = object.get_mut(field).filter(|value| value.is_string()) {
                    visitor(&mut Slot::Address(address));
                }
            }
            for (key, child) in object.iter_mut() {
                if USER_CONTENT.contains(&key.as_str())
                    || matches!(
                        key.as_str(),
                        "actor_id" | "member_id" | "sender_id" | "recipient_id"
                    )
                {
                    continue;
                }
                visit(child, visitor);
            }
        }
        _ => {}
    }
}

fn conversation_summary(value: &mut Value) {
    if let Some(object) = value.as_object_mut() {
        if object.contains_key("participants") {
            if let Some(group) = object
                .get_mut("group")
                .and_then(serde_json::Value::as_object_mut)
            {
                for key in ["version", "members", "pending_invitations"] {
                    group.remove(key);
                }
            }
            if let Some(last) = object
                .get_mut("last_message")
                .and_then(serde_json::Value::as_object_mut)
            {
                last.remove("version");
            }
        }
        for key in ["data", "items"] {
            if let Some(child) = object.get_mut(key) {
                conversation_summary(child);
            }
        }
    } else if let Some(items) = value.as_array_mut() {
        for item in items {
            conversation_summary(item);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ActorType;

    #[test]
    fn accounts_render_with_current_ids_and_user_content_is_untouched() {
        let mut value = serde_json::json!({
            "participants": [{"type": "carbon", "id": "zQo"}, {"type": "silicon", "id": "K1E"},
                {"type": "carbon", "id": "c:old-timer"}, {"type": "carbon", "id": "Gn3"}],
            "last_message": {"sender": {"type": "silicon", "id": "K1E"}, "sender_id": "deliberate@K1E",
                "recipient_id": "zQo", "metadata": {"who": {"type": "carbon", "id": "zQo"}},
                "message": "zQo"},
            "draft": {"actor_id": "zQo"}
        });
        let accounts = HashMap::from([
            (
                "zQo".to_owned(),
                Account {
                    uuid: "zQo".into(),
                    kind: ActorType::Carbon,
                    id: Some("c:saket".into()),
                    display_name: Some("Saket".into()),
                    pfp_url: None,
                    custodian: None,
                    deleted: false,
                },
            ),
            (
                "K1E".to_owned(),
                Account {
                    uuid: "K1E".into(),
                    kind: ActorType::Silicon,
                    id: Some("si:cos".into()),
                    display_name: None,
                    pfp_url: None,
                    custodian: Some("zQo".into()),
                    deleted: false,
                },
            ),
            (
                "Gn3".to_owned(),
                Account {
                    uuid: "Gn3".into(),
                    kind: ActorType::Carbon,
                    id: None,
                    display_name: None,
                    pfp_url: None,
                    custodian: None,
                    deleted: true,
                },
            ),
        ]);
        visit(&mut value, &mut |slot| slot.render(&accounts));
        assert_eq!(
            value["participants"][0],
            serde_json::json!({"type": "carbon", "id": "c:saket", "uuid": "zQo", "display_name": "Saket"})
        );
        assert_eq!(value["participants"][1]["id"], "si:cos");
        assert_eq!(
            value["participants"][2],
            serde_json::json!({"type": "carbon", "id": "c:old-timer", "legacy": true})
        );
        assert_eq!(
            value["participants"][3],
            serde_json::json!({"type": "carbon", "id": "Gn3", "uuid": "Gn3", "deleted": true})
        );
        assert_eq!(value["last_message"]["sender"]["id"], "si:cos");
        assert_eq!(value["last_message"]["sender_id"], "deliberate@si:cos");
        assert_eq!(value["last_message"]["recipient_id"], "c:saket");
        assert_eq!(
            value["last_message"]["metadata"]["who"]["id"], "zQo",
            "metadata is never rewritten"
        );
        assert_eq!(value["last_message"]["message"], "zQo");
        assert_eq!(value["draft"]["actor_id"], "c:saket");
        assert_eq!(value["draft"]["actor_uuid"], "zQo");
    }
}
