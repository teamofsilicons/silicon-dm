//! Owner-scoped address-book and profile routes for DM and standalone interfaces.
use super::extract::{ApiJson, ApiPath, ApiQuery, Authenticated};
use crate::{AppError, AppResult, application::state::AppState, infrastructure::contacts};
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, Method, header},
    response::{IntoResponse, Response},
};
use bytes::Bytes;
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

#[derive(Deserialize, serde::Serialize)]
pub(super) struct Page {
    q: Option<String>,
    limit: Option<u16>,
    offset: Option<u32>,
}

pub(super) async fn list(
    State(s): State<AppState>,
    Authenticated(c): Authenticated,
    ApiQuery(page): ApiQuery<Page>,
) -> AppResult<Json<Value>> {
    let (_proof, req) = contacts::user(&s, &c, Method::GET, &["contacts"]).await?;
    Ok(Json(contacts::json(req.query(&page)).await?))
}

#[derive(Deserialize)]
pub(super) struct Lookup {
    q: String,
}
pub(super) async fn lookup(
    State(s): State<AppState>,
    Authenticated(c): Authenticated,
    ApiQuery(query): ApiQuery<Lookup>,
) -> AppResult<Json<Value>> {
    let id = query.q.trim();
    if !crate::infrastructure::postgres::valid_public_id(id) && !crate::domain::is_account_uuid(id)
    {
        return Err(AppError::invalid(
            "Enter a complete Carbon or Silicon ID, or its permanent account identifier.",
        ));
    }
    let proof = contacts::user_proof(&s, &c, &Method::GET).await?;
    let req = contacts::request(&s, &proof.token, Method::GET, &["lookup"])?;
    let profile = contacts::json(req.query(&[("q", id)])).await?;
    let uuid = profile["uuid"]
        .as_str()
        .ok_or(AppError::DependencyUnavailable {
            dependency: "silicon_contacts",
        })?;
    let saved = contacts::request(&s, &proof.token, Method::GET, &["contacts", uuid])?;
    match contacts::json(saved).await {
        Ok(contact) => Ok(Json(contact_profile(&contact))),
        Err(err) if err.status() == http::StatusCode::NOT_FOUND => Ok(Json(profile)),
        Err(err) => Err(err),
    }
}

#[derive(Deserialize)]
pub(super) struct PeerPath {
    peer: String,
}
#[derive(Deserialize)]
pub(super) struct PhotoPath {
    photo_id: Uuid,
}

pub(super) async fn get(
    State(s): State<AppState>,
    Authenticated(c): Authenticated,
    ApiPath(PeerPath { peer }): ApiPath<PeerPath>,
) -> AppResult<Json<Value>> {
    let (_proof, req) = contacts::user(&s, &c, Method::GET, &["contacts", &peer]).await?;
    Ok(Json(contacts::json(req).await?))
}
pub(super) async fn add(
    State(s): State<AppState>,
    Authenticated(c): Authenticated,
    ApiJson(body): ApiJson<Value>,
) -> AppResult<Json<Value>> {
    let (_proof, req) = contacts::user(&s, &c, Method::POST, &["contacts"]).await?;
    Ok(Json(contacts::json(req.json(&body)).await?))
}
pub(super) async fn update(
    State(s): State<AppState>,
    Authenticated(c): Authenticated,
    ApiPath(PeerPath { peer }): ApiPath<PeerPath>,
    ApiJson(body): ApiJson<Value>,
) -> AppResult<Json<Value>> {
    let (_proof, req) = contacts::user(&s, &c, Method::PUT, &["contacts", &peer]).await?;
    Ok(Json(contacts::json(req.json(&body)).await?))
}
pub(super) async fn remove(
    State(s): State<AppState>,
    Authenticated(c): Authenticated,
    ApiPath(PeerPath { peer }): ApiPath<PeerPath>,
) -> AppResult<Json<Value>> {
    let (_proof, req) = contacts::user(&s, &c, Method::DELETE, &["contacts", &peer]).await?;
    Ok(Json(contacts::json(req).await?))
}

pub(super) async fn upload(
    State(s): State<AppState>,
    Authenticated(c): Authenticated,
    headers: HeaderMap,
    body: Bytes,
) -> AppResult<Json<Value>> {
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .filter(|v| v.starts_with("multipart/form-data;"))
        .ok_or_else(|| AppError::invalid("Upload a PNG, JPEG or WebP as multipart form data."))?;
    let (_proof, req) = contacts::user(&s, &c, Method::POST, &["photos"]).await?;
    let req = req.header(header::CONTENT_TYPE, content_type).body(body);
    let mut result = contacts::json(req).await?;
    let id = result["id"]
        .as_str()
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or(AppError::DependencyUnavailable {
            dependency: "silicon_contacts",
        })?;
    result["url"] = Value::String(format!("/api/v1/contact-photos/{id}"));
    Ok(Json(result))
}
pub(super) async fn photo(
    State(s): State<AppState>,
    Authenticated(c): Authenticated,
    ApiPath(PhotoPath { photo_id: id }): ApiPath<PhotoPath>,
) -> AppResult<Response> {
    let (_proof, req) = contacts::user(&s, &c, Method::GET, &["photos", &id.to_string()]).await?;
    Ok((
        [
            (header::CONTENT_TYPE, "image/png"),
            (header::CACHE_CONTROL, "private, no-store"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        contacts::bytes(req).await?,
    )
        .into_response())
}

#[derive(Deserialize)]
pub(super) struct Search {
    #[serde(default)]
    q: String,
    limit: Option<usize>,
}

fn contact_profile(contact: &Value) -> Value {
    let mut profile = contact["account"].clone();
    profile["display_name"] = contact["name"].clone();
    profile["trust"] = contact["trust"].clone();
    profile["photo_id"] = contact["photo_id"].clone();
    if let Some(id) = contact["photo_id"]
        .as_str()
        .and_then(|s| Uuid::parse_str(s).ok())
    {
        profile["pfp_url"] = Value::String(format!("/api/v1/contact-photos/{id}"));
    }
    profile
}

fn fuzzy(haystack: &str, needle: &str) -> bool {
    let mut chars = haystack.chars();
    needle
        .chars()
        .filter(|c| !c.is_whitespace())
        .all(|ch| chars.any(|c| c == ch))
}

pub(super) async fn people(
    State(s): State<AppState>,
    Authenticated(c): Authenticated,
    ApiQuery(query): ApiQuery<Search>,
) -> AppResult<Json<Value>> {
    use std::collections::HashMap;
    let owner = c.actor.id.as_str();
    let query_text = query.q.trim().to_lowercase();
    if query_text.len() > 254 {
        return Err(AppError::invalid(
            "Search by an account name or ID of up to 254 bytes.",
        ));
    }
    let known:Vec<Value>=sqlx::query_scalar("WITH peers AS (
        SELECT DISTINCT them.actor_id FROM effective_conversation_participants mine
        JOIN effective_conversation_participants them ON them.conversation_id=mine.conversation_id
        WHERE mine.actor_id=$1 UNION SELECT uuid FROM accounts WHERE custodian_uuid=$1)
        SELECT jsonb_build_object('uuid',a.uuid,'id',a.current_id,'kind',a.kind,'display_name',a.display_name,'pfp_url',a.pfp_url,'trust',0)
        FROM peers JOIN accounts a ON a.uuid=peers.actor_id WHERE a.uuid<>$1 AND a.status='active' AND a.current_id IS NOT NULL")
        .bind(owner).fetch_all(s.store.pool()).await?;
    let mut by_id: HashMap<String, Value> = known
        .into_iter()
        .filter_map(|p| Some((p["uuid"].as_str()?.to_owned(), p)))
        .collect();
    let (_proof, req) = contacts::user(&s, &c, Method::GET, &["contacts"]).await?;
    let mut offset = 0;
    loop {
        let page = contacts::json(
            req.try_clone()
                .ok_or(AppError::DependencyUnavailable {
                    dependency: "silicon_contacts",
                })?
                .query(&[("offset", offset), ("limit", 200)]),
        )
        .await?;
        let items = page["items"]
            .as_array()
            .ok_or(AppError::DependencyUnavailable {
                dependency: "silicon_contacts",
            })?;
        for item in items {
            let Some(uuid) = item["uuid"].as_str() else {
                continue;
            };
            if item["blocked"] == true || item["account"]["status"] == "deleted" {
                by_id.remove(uuid);
                continue;
            }
            by_id.insert(uuid.to_owned(), contact_profile(item));
        }
        offset += items.len();
        if u64::try_from(offset).unwrap_or(u64::MAX) >= page["total"].as_u64().unwrap_or_default()
            || items.is_empty()
        {
            break;
        }
    }
    let mut matches: Vec<Value> = by_id
        .into_values()
        .filter(|p| {
            [p["id"].as_str(), p["display_name"].as_str()]
                .into_iter()
                .flatten()
                .any(|v| fuzzy(&v.to_lowercase(), &query_text))
        })
        .collect();
    matches.sort_by(|a, b| {
        (b["id"] == query_text)
            .cmp(&(a["id"] == query_text))
            .then_with(|| b["trust"].as_u64().cmp(&a["trust"].as_u64()))
            .then_with(|| a["display_name"].as_str().cmp(&b["display_name"].as_str()))
            .then_with(|| a["id"].as_str().cmp(&b["id"].as_str()))
    });
    matches.truncate(query.limit.unwrap_or(20).clamp(1, 100));
    Ok(Json(serde_json::json!({"items":matches})))
}
