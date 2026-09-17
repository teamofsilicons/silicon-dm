//! Public-address response adapter, scoped to the selected production or sandbox store.
use crate::{AppError, AppResult, application::state::AppState};
use axum::{
    body::{Body, to_bytes},
    extract::{Request, State},
    http::header,
    middleware::Next,
    response::{IntoResponse, Response},
};
pub(super) async fn responses(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let eligible = request.uri().path() == "/api/v1/messages"
        || request.uri().path().starts_with("/api/v1/groups")
        || request.uri().path().starts_with("/api/v1/conversations");
    let summary = request.uri().path() == "/api/v1/conversations";
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
    if summary {
        conversation_summary(&mut value);
    }
    let bytes = serde_json::to_vec(&value).map_err(AppError::internal)?;
    parts.headers.remove(header::CONTENT_LENGTH);
    Ok(Response::from_parts(parts, Body::from(bytes)))
}

fn conversation_summary(value: &mut serde_json::Value) {
    if let Some(object) = value.as_object_mut() {
        if object.contains_key("participants") {
            if let Some(group) = object
                .get_mut("group")
                .and_then(serde_json::Value::as_object_mut)
            {
                for key in ["is_public", "version", "invited_members"] {
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
