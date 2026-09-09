//! Axum router construction.

use axum::{
    Router,
    extract::{DefaultBodyLimit, Request},
    http::{HeaderName, StatusCode, header},
    response::{IntoResponse as _, Response},
    routing::{any, get, post},
};
use tower::{ServiceBuilder, ServiceExt as _};
use tower_http::{
    catch_panic::CatchPanicLayer,
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    sensitive_headers::SetSensitiveRequestHeadersLayer,
    timeout::TimeoutLayer,
    trace::TraceLayer,
};

use super::{auth_handlers, handlers, webhook};
use crate::testing::handlers as testing;
use crate::{AppError, application::state::AppState};

/// Builds the versioned DM router.
fn build_plane_router(state: AppState) -> Router {
    let public_api = Router::new()
        .route("/iam", get(auth_handlers::iam))
        .route(
            "/conversations",
            get(handlers::list_conversations).post(handlers::create_conversation),
        )
        .route(
            "/conversations/{conversation_id}/messages",
            get(handlers::list_messages).post(handlers::send_message),
        )
        .route(
            "/conversations/{conversation_id}/messages/{message_id}",
            get(handlers::get_message)
                .patch(handlers::edit_message)
                .delete(handlers::delete_message),
        )
        .route(
            "/conversations/{conversation_id}/messages/{message_id}/receipts",
            post(handlers::record_message_receipt),
        )
        .route(
            "/conversations/{conversation_id}/bundles",
            post(handlers::create_message_bundle),
        )
        .route(
            "/conversations/{conversation_id}/bundles/{bundle_id}",
            get(handlers::get_message_bundle),
        )
        .route(
            "/conversations/{conversation_id}/draft",
            get(handlers::get_draft)
                .put(handlers::put_draft)
                .delete(handlers::delete_draft),
        )
        .route("/presence/{actor_id}", get(handlers::get_presence))
        .route("/gifs/trending", get(handlers::list_trending_gifs))
        .route("/gifs/search", get(handlers::search_gifs))
        .route("/gifs/recent", get(handlers::list_recent_gifs))
        .route(
            "/auth/login",
            post(auth_handlers::login).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/auth/refresh",
            post(auth_handlers::refresh).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/auth/logout",
            post(auth_handlers::logout).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route("/auth/me", get(auth_handlers::me))
        .route(
            "/testing-environments",
            get(testing::list).post(testing::create),
        )
        .route(
            "/testing-environments/{environment_id}",
            get(testing::get)
                .patch(testing::update)
                .delete(testing::delete),
        )
        .route(
            "/testing-environments/{environment_id}/key",
            get(testing::key),
        )
        .route(
            "/testing-environments/{environment_id}/rotate-key",
            post(testing::rotate),
        )
        .route(
            "/testing-environments/{environment_id}/restore",
            post(testing::restore),
        )
        .route(
            "/testing-environments/{environment_id}/clean",
            post(testing::clean),
        );

    let timed_routes = Router::new()
        .route(
            "/webhook/",
            post(webhook::receive).layer(DefaultBodyLimit::max(1024 * 1024)),
        )
        .route("/live", get(handlers::liveness))
        .route("/ready", get(handlers::readiness))
        .nest("/api/v1", public_api)
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            state.settings.server.request_timeout,
        ));
    let realtime_route = Router::new().route("/api/v1/ws", get(handlers::open_realtime_connection));

    Router::new()
        .merge(timed_routes)
        .merge(realtime_route)
        .fallback(|| async { AppError::NotFound })
        .with_state(state)
}

async fn prevent_response_caching(mut response: Response) -> Response {
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    response
}

/// Builds guarded routing with fail-closed selection of testing data planes.
pub fn build_router(state: AppState) -> Router {
    let maximum_body = state.settings.server.max_body_bytes;
    let production = build_plane_router(state.clone());
    let request_id_header = HeaderName::from_static("x-request-id");
    let test_header = HeaderName::from_static("x-testing-environment-key");
    let middleware = ServiceBuilder::new()
        .layer(SetSensitiveRequestHeadersLayer::new([
            header::AUTHORIZATION,
            test_header,
        ]))
        .layer(SetRequestIdLayer::new(
            request_id_header.clone(),
            MakeRequestUuid,
        ))
        .layer(
            TraceLayer::new_for_http().make_span_with(|request: &axum::http::Request<_>| {
                tracing::info_span!(
                    "http.request",
                    method = %request.method(),
                    path = %request.uri().path(),
                )
            }),
        )
        .layer(PropagateRequestIdLayer::new(request_id_header))
        .layer(CatchPanicLayer::new());

    Router::new()
        .fallback(any(move |request: Request| {
            let state = state.clone();
            let production = production.clone();
            async move { dispatch(state, production, request).await }
        }))
        .layer(DefaultBodyLimit::max(maximum_body))
        .layer(middleware)
        .layer(axum::middleware::map_response(prevent_response_caching))
}

async fn dispatch(state: AppState, production: Router, request: Request) -> Response {
    let path = request.uri().path();
    // Lifecycle authority always comes from production IAM; only clean explicitly
    // accepts a testing root key. Webhooks authenticate their own environment binding.
    let control = path.starts_with("/api/v1/testing-environments")
        || matches!(path, "/live" | "/ready" | "/webhook/");
    let mut values = request
        .headers()
        .get_all("x-testing-environment-key")
        .iter();
    let value = values.next();
    if values.next().is_some() {
        return AppError::validation("provide exactly one testing environment key").into_response();
    }
    let selected_key = match value {
        Some(value) => match value.to_str() {
            Ok(value) => Some(value.to_owned()),
            Err(_) => return AppError::Unauthorized.into_response(),
        },
        None => None,
    };
    let expected_generation = if control {
        None
    } else {
        match testing_generation(&request) {
            Ok(value) => value,
            Err(error) => return error.into_response(),
        }
    };
    if !control && selected_key.is_none() && expected_generation.is_some() {
        return AppError::validation(
            "testing environment generation requires a testing environment key",
        )
        .into_response();
    }
    if control || selected_key.is_none() {
        return match production.oneshot(request).await {
            Ok(response) => response,
            Err(never) => match never {},
        };
    }
    let Some(registry) = state.testing.clone() else {
        return AppError::validation("testing environments are not configured").into_response();
    };
    let selected = match registry
        .state_for_key(&state, selected_key.as_deref().unwrap_or_default())
        .await
    {
        Ok(state) => state,
        Err(error) => return error.into_response(),
    };
    let (Some(id), Some(generation)) = (selected.testing_environment, selected.testing_generation)
    else {
        return AppError::Unauthorized.into_response();
    };
    if expected_generation.is_some_and(|expected| expected != generation) {
        return AppError::conflict(
            "testing environment generation changed; reconnect before retrying",
        )
        .into_response();
    }
    let fence = match registry.request_fence(id, generation).await {
        Ok(fence) => fence,
        Err(error) => return error.into_response(),
    };
    let result = build_plane_router(selected).oneshot(request).await;
    drop(fence);
    match result {
        Ok(response) => response,
        Err(never) => match never {},
    }
}

fn testing_generation(request: &Request) -> crate::AppResult<Option<i64>> {
    let mut values = request
        .headers()
        .get_all("x-testing-environment-generation")
        .iter();
    let value = values.next();
    if values.next().is_some() {
        return Err(AppError::validation(
            "provide exactly one testing environment generation",
        ));
    }
    let Some(value) = value else { return Ok(None) };
    let generation = value
        .to_str()
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| {
            AppError::validation("testing environment generation must be a positive integer")
        })?;
    Ok(Some(generation))
}
