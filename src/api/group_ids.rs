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
    match translate(state, response).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}
async fn translate(state: AppState, response: Response) -> AppResult<Response> {
    let (mut parts, body) = response.into_parts();
    let bytes = to_bytes(body, usize::MAX)
        .await
        .map_err(AppError::internal)?;
    let mut value = serde_json::from_slice(&bytes).map_err(AppError::internal)?;
    drop(bytes);
    state.store.public_messages(&mut value).await?;
    state.store.public_conversation_ids(&mut value).await?;
    let bytes = serde_json::to_vec(&value).map_err(AppError::internal)?;
    parts.headers.remove(header::CONTENT_LENGTH);
    Ok(Response::from_parts(parts, Body::from(bytes)))
}
