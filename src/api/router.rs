//! Axum router construction.

use axum::{
    Router,
    extract::DefaultBodyLimit,
    http::{HeaderName, StatusCode, header},
    routing::{get, post},
};
use tower::ServiceBuilder;
use tower_http::{
    catch_panic::CatchPanicLayer,
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    sensitive_headers::SetSensitiveRequestHeadersLayer,
    timeout::TimeoutLayer,
    trace::TraceLayer,
};

use super::handlers;
use crate::{AppError, application::state::AppState};

/// Builds the versioned DM router.
pub fn build_router(state: AppState) -> Router {
    let public_api = Router::new()
        .route(
            "/conversations",
            get(handlers::list_conversations).post(handlers::create_conversation),
        )
        .route(
            "/conversations/{conversation_id}/messages",
            get(handlers::list_messages).post(handlers::send_message),
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
        .route(
            "/attachments/temporary-url",
            post(handlers::create_attachment_temporary_url),
        )
        .route("/gifs/trending", get(handlers::list_trending_gifs))
        .route("/gifs/search", get(handlers::search_gifs))
        .route("/gifs/recent", get(handlers::list_recent_gifs));

    let timed_routes = Router::new()
        .route("/live", get(handlers::liveness))
        .route("/ready", get(handlers::readiness))
        .nest("/api/v1", public_api)
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            state.settings.server.request_timeout,
        ));
    let realtime_route = Router::new().route("/api/v1/ws", get(handlers::open_realtime_connection));

    let request_id_header = HeaderName::from_static("x-request-id");
    let obo_header = HeaderName::from_static("x-iam-obo-access-proof");
    let middleware = ServiceBuilder::new()
        .layer(SetSensitiveRequestHeadersLayer::new([
            header::AUTHORIZATION,
            obo_header,
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
        .merge(timed_routes)
        .merge(realtime_route)
        .fallback(|| async { AppError::NotFound })
        .layer(DefaultBodyLimit::max(state.settings.server.max_body_bytes))
        .layer(middleware)
        .with_state(state)
}
