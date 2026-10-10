//! HTTP synchronization and activity leases after notification delivery moves to Ting.

use axum::{Json, extract::State, http::StatusCode};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq as _;
use time::OffsetDateTime;

use super::extract::{ApiJson, ApiPath, ApiQuery, Authenticated};
use crate::{
    AppError, AppResult,
    application::{auth::AuthContext, state::AppState},
    domain::{Activity, PageRequest},
    infrastructure::postgres::{PresenceLease, SyncEvent, sync_reset_required},
};

const CURSOR_SECONDS: i64 = 24 * 60 * 60;
/// Longest a sync request may wait for something new.
pub(crate) const MAX_WAIT_SECONDS: u16 = 30;
const CURSOR_VERSION: u8 = 2;

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SyncQuery {
    cursor: Option<String>,
    limit: Option<u16>,
    #[serde(default)]
    reset: bool,
    /// Seconds to wait for a new event when there is none yet (0 to 30).
    wait: Option<u16>,
}

#[derive(Serialize)]
pub(super) struct SyncPage {
    events: Vec<SyncEvent>,
    cursor: String,
    has_more: bool,
    upper_sequence: i64,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SyncCursor {
    version: u8,
    scope: String,
    position: i64,
    upper: Option<i64>,
    expires_at: i64,
}

/// `GET /api/v1/sync`: references to what changed for the account since its cursor.
/// With `wait=N` an empty answer is held for up to N seconds until something arrives.
pub(super) async fn events(
    State(state): State<AppState>,
    Authenticated(auth): Authenticated,
    ApiQuery(query): ApiQuery<SyncQuery>,
) -> AppResult<Json<SyncPage>> {
    let limit = PageRequest {
        limit: query.limit,
        cursor: None,
    }
    .validated_limit()?;
    if query.reset && query.cursor.is_some() {
        return Err(AppError::validation("reset cannot be combined with cursor"));
    }
    let wait = query.wait.unwrap_or(0);
    if wait > MAX_WAIT_SECONDS {
        return Err(AppError::validation(format!(
            "wait must be 0 to {MAX_WAIT_SECONDS} seconds"
        )));
    }
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(u64::from(wait));
    loop {
        // Subscribe before reading, so an event committed in between still wakes us.
        let wakeups = state.sync.subscribe();
        let page = page(&state, &auth, &query, limit).await?;
        if !page.events.is_empty()
            || page.has_more
            || query.reset
            || tokio::time::Instant::now() >= deadline
        {
            return Ok(Json(page));
        }
        if !state
            .sync
            .wait_for(wakeups, auth.actor.id.as_str(), deadline)
            .await
        {
            return Ok(Json(page));
        }
    }
}

async fn page(
    state: &AppState,
    auth: &AuthContext,
    query: &SyncQuery,
    limit: u16,
) -> AppResult<SyncPage> {
    let scope = cursor_scope(auth)?;
    let key = cursor_key(state);
    let now = OffsetDateTime::now_utc().unix_timestamp();
    let cursor = query
        .cursor
        .as_deref()
        .map(|value| decode_cursor(value, &key, &scope, now))
        .transpose()?;
    let (head, floor) = state.store.sync_head(&auth.actor).await?;
    let after = cursor.as_ref().map_or(floor, |cursor| cursor.position);
    let upper = cursor
        .as_ref()
        .and_then(|cursor| cursor.upper)
        .unwrap_or(head);
    if after > head || upper > head || after < floor {
        return Err(sync_reset_required());
    }
    let (events, position, has_more, upper_sequence) = if query.reset {
        (Vec::new(), head, false, head)
    } else {
        let scan = state
            .store
            .sync_events(&auth.actor, after, upper, limit)
            .await?;
        (scan.events, scan.position, scan.has_more, upper)
    };
    let next = SyncCursor {
        version: CURSOR_VERSION,
        scope,
        position,
        upper: has_more.then_some(upper_sequence),
        // Keep the original deadline while paging through a fixed snapshot.
        expires_at: cursor
            .filter(|cursor| cursor.upper.is_some())
            .map_or(now + CURSOR_SECONDS, |cursor| cursor.expires_at),
    };
    Ok(SyncPage {
        events,
        cursor: encode_cursor(&next, &key)?,
        has_more,
        upper_sequence,
    })
}

fn cursor_scope(auth: &AuthContext) -> AppResult<String> {
    let scope = serde_json::to_vec(&(
        "accounts",
        auth.actor.actor_type.as_str(),
        auth.actor.id.as_str(),
    ))
    .map_err(AppError::internal)?;
    Ok(blake3::hash(&scope).to_hex().to_string())
}

fn cursor_key(state: &AppState) -> [u8; 32] {
    state
        .settings
        .data_key
        .derive("silicon-dm authenticated HTTP sync cursor v2")
}

fn encode_cursor(cursor: &SyncCursor, key: &[u8; 32]) -> AppResult<String> {
    let body = serde_json::to_vec(cursor).map_err(AppError::internal)?;
    Ok(format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(&body),
        URL_SAFE_NO_PAD.encode(blake3::keyed_hash(key, &body).as_bytes()),
    ))
}

fn decode_cursor(value: &str, key: &[u8; 32], scope: &str, now: i64) -> AppResult<SyncCursor> {
    if value.len() > 2048 {
        return Err(AppError::validation("invalid synchronization cursor"));
    }
    let invalid = || AppError::validation("invalid synchronization cursor");
    let (body, signature) = value.split_once('.').ok_or_else(invalid)?;
    let body = URL_SAFE_NO_PAD.decode(body).map_err(|_| invalid())?;
    let signature = URL_SAFE_NO_PAD.decode(signature).map_err(|_| invalid())?;
    if !bool::from(blake3::keyed_hash(key, &body).as_bytes().ct_eq(&signature)) {
        // Cursors from DM 0.14 were signed with a key derived from the retired IAM
        // secret: they fail here once, and the client resets.
        return Err(sync_reset_required());
    }
    let cursor: SyncCursor = serde_json::from_slice(&body).map_err(|_| invalid())?;
    if cursor.version != CURSOR_VERSION
        || cursor.position < 0
        || cursor.upper.is_some_and(|upper| upper < cursor.position)
    {
        return Err(invalid());
    }
    if cursor.scope != scope || cursor.expires_at <= now {
        return Err(sync_reset_required());
    }
    Ok(cursor)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PresenceInput {
    #[serde(default)]
    activity: Option<Activity>,
}

#[derive(Deserialize)]
pub(super) struct DevicePath {
    device_id: String,
}

pub(super) async fn renew_presence(
    State(state): State<AppState>,
    Authenticated(auth): Authenticated,
    ApiPath(device): ApiPath<DevicePath>,
    ApiJson(input): ApiJson<PresenceInput>,
) -> AppResult<Json<PresenceLease>> {
    state
        .store
        .renew_http_presence(
            &auth,
            &device.device_id,
            input.activity,
            state.settings.realtime.heartbeat_timeout,
            state.settings.realtime.activity_ttl,
        )
        .await
        .map(Json)
}

pub(super) async fn close_presence(
    State(state): State<AppState>,
    Authenticated(auth): Authenticated,
    ApiPath(device): ApiPath<DevicePath>,
) -> AppResult<StatusCode> {
    state
        .store
        .close_http_presence(&auth, &device.device_id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::{SyncCursor, decode_cursor, encode_cursor};

    #[test]
    fn cursor_is_signed_scoped_expiring_and_bound_to_its_scan() -> crate::AppResult<()> {
        let key = [7; 32];
        let cursor = SyncCursor {
            version: 2,
            scope: "actor-org-environment-generation".into(),
            position: 2,
            upper: Some(5),
            expires_at: 100,
        };
        let encoded = encode_cursor(&cursor, &key)?;
        let decoded = decode_cursor(&encoded, &key, &cursor.scope, 99)?;
        assert_eq!(decoded.position, 2);
        assert_eq!(decoded.upper, Some(5));
        assert_eq!(
            decode_cursor(&encoded, &key, "other", 99)
                .err()
                .map(|e| e.code()),
            Some("sync_reset_required")
        );
        assert_eq!(
            decode_cursor(&encoded, &key, &cursor.scope, 100)
                .err()
                .map(|e| e.code()),
            Some("sync_reset_required")
        );
        assert_eq!(
            decode_cursor(&encoded, &[8; 32], &cursor.scope, 99)
                .err()
                .map(|e| e.code()),
            Some("sync_reset_required"),
            "a cursor signed with another key (the IAM-era one) asks for a reset"
        );
        let tampered = format!("{encoded}x");
        assert!(decode_cursor(&tampered, &key, &cursor.scope, 99).is_err());
        Ok(())
    }
}
