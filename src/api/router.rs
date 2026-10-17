//! Axum router construction.

use std::time::Duration;

use axum::{
    Router,
    extract::DefaultBodyLimit,
    http::{HeaderName, Method, StatusCode, Uri, header},
    response::Response,
    routing::{get, post, put},
};
use tower::ServiceBuilder;
use tower_http::{
    catch_panic::CatchPanicLayer,
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    sensitive_headers::SetSensitiveRequestHeadersLayer,
    timeout::TimeoutLayer,
    trace::TraceLayer,
};

use super::{groups, handlers, me, receipts, speech, webhook};
use crate::{AppError, application::state::AppState};

/// Margin added to the longest sync wait before the request is cut off.
const SYNC_TIMEOUT_MARGIN: Duration = Duration::from_secs(15);
/// Rendering speech takes 2 to 6 seconds, at most three minutes.
const SPEECH_TIMEOUT: Duration = Duration::from_secs(200);
/// A transcription includes uploading the audio (up to `DM_TRANSCRIBE_MAX_BYTES`).
const TRANSCRIPTION_TIMEOUT: Duration = Duration::from_secs(610);

/// Builds the versioned DM router.
#[allow(
    clippy::too_many_lines,
    reason = "all public routes remain visible together"
)]
fn build_api_router(state: AppState) -> Router {
    let public_api = Router::new()
        .route("/groups", post(groups::create))
        .route("/groups/{group_id}", get(groups::get).patch(groups::update))
        .route(
            "/groups/{group_id}/members",
            post(groups::invite).delete(groups::remove),
        )
        .route(
            "/groups/{group_id}/owners",
            post(groups::add_owners).delete(groups::remove_owners),
        )
        .route("/groups/{group_id}/accept", post(groups::accept))
        .route("/groups/{group_id}/decline", post(groups::decline))
        .route("/groups/{group_id}/leave", post(groups::leave))
        .route("/invitations", get(groups::invitations))
        .route(
            "/allow-list",
            get(me::allow_list).post(me::allow).delete(me::disallow),
        )
        .route("/me", get(me::me))
        .route("/accounts", get(me::accounts))
        .route("/voices", get(speech::voices))
        .route(
            "/voices/{account}",
            get(speech::voice)
                .put(speech::set_voice)
                .delete(speech::reset_voice),
        )
        .route("/delivery", get(me::delivery_status))
        .route("/messages", post(handlers::send_to_recipient))
        .route(
            "/reports",
            post(crate::reporting::submit).layer(DefaultBodyLimit::max(65536)),
        )
        .route("/contracts", get(super::contracts::describe))
        .route(
            "/presence/devices/{device_id}",
            put(super::sync::renew_presence).delete(super::sync::close_presence),
        )
        .route(
            "/conversations",
            get(handlers::list_conversations).post(handlers::create_conversation),
        )
        .route(
            "/conversations/{conversation_id}",
            get(handlers::get_conversation),
        )
        .route(
            "/conversations/{conversation_id}/receipts",
            post(receipts::record_receipts),
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
        .route("/gifs/recent", get(handlers::list_recent_gifs));

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
    // Sync may hold a request open up to MAX_WAIT_SECONDS waiting for something new.
    let sync_routes = Router::new()
        .route("/api/v1/sync", get(super::sync::events))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            Duration::from_secs(u64::from(super::sync::MAX_WAIT_SECONDS)) + SYNC_TIMEOUT_MARGIN,
        ));
    // Speech renders through Deepgram (seconds, not milliseconds); transcriptions upload.
    let speech_routes = Router::new()
        .route(
            "/api/v1/conversations/{conversation_id}/messages/{message_id}/speech",
            get(speech::message_speech),
        )
        .route(
            "/api/v1/speech",
            post(speech::render_speech).layer(DefaultBodyLimit::max(64 * 1024)),
        )
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            SPEECH_TIMEOUT,
        ));
    let transcription_routes = Router::new()
        .route("/api/v1/speech/transcriptions", post(speech::transcribe))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            TRANSCRIPTION_TIMEOUT,
        ));
    let retired_routes = Router::new()
        .route("/api/v1/ws", get(super::contracts::retired))
        .route("/api/v1/ws/shared", get(super::contracts::retired));

    Router::new()
        .merge(timed_routes)
        .merge(sync_routes)
        .merge(speech_routes)
        .merge(transcription_routes)
        .merge(retired_routes)
        .method_not_allowed_fallback(wrong_method)
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            super::render::responses,
        ))
        .fallback(unknown_route)
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            super::contracts::negotiate,
        ))
        .with_state(state)
}

/// A path DM has no route for: name it, and where the routes are listed.
async fn unknown_route(method: Method, uri: Uri) -> AppError {
    let path: String = uri.path().chars().take(200).collect();
    AppError::not_found(format!("DM has no route {method} {path}."))
        .with_hint("Every route is listed in openapi.yaml (dm docs openapi) and docs/reference/api.md (dm docs api); routes start with /api/v1/, and GET /api/v1/contracts says which contract versions this DM serves.")
}

/// A route DM has, called with a method it does not take (axum adds the `Allow` header).
/// The code stays `http_error`, what a 405 has always answered.
async fn wrong_method(method: Method, uri: Uri) -> AppError {
    let path: String = uri.path().chars().take(200).collect();
    AppError::refused(
        StatusCode::METHOD_NOT_ALLOWED,
        "http_error",
        format!(
            "The route {path} does not take {method}; the Allow header lists the methods it takes."
        ),
    )
    .with_hint("openapi.yaml (dm docs openapi) describes every method of every route.")
}

/// Nothing is cached by default; rendered audio sets its own policy (revalidate by `ETag`).
async fn prevent_response_caching(mut response: Response) -> Response {
    if !response.headers().contains_key(header::CACHE_CONTROL) {
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            axum::http::HeaderValue::from_static("no-store"),
        );
    }
    response
}

/// Builds the DM router with request ids, tracing, panic isolation and the public
/// `{type, data}` envelope.
pub fn build_router(state: AppState) -> Router {
    let maximum_body = state.settings.server.max_body_bytes;
    let api = build_api_router(state);
    let request_id_header = HeaderName::from_static("x-request-id");
    let middleware = ServiceBuilder::new()
        .layer(SetSensitiveRequestHeadersLayer::new([header::AUTHORIZATION]))
        .layer(SetRequestIdLayer::new(
            request_id_header.clone(),
            MakeRequestUuid,
        ))
        .layer(
            TraceLayer::new_for_http().make_span_with(|request: &axum::http::Request<_>| {
                tracing::info_span!(
                    "http.request",
                    method = %request.method(),
                    operation = silicon_dm_protocol::http_type(request.method().as_str(), request.uri().path()),
                )
            }),
        )
        .layer(PropagateRequestIdLayer::new(request_id_header))
        .layer(CatchPanicLayer::new());

    api.layer(DefaultBodyLimit::max(maximum_body))
        .layer(middleware)
        .layer(axum::middleware::from_fn(silicon_dm_protocol::responses))
        .layer(axum::middleware::map_response(prevent_response_caching))
}

#[cfg(test)]
mod tests {
    /// The routes registered in this file, as full paths with every `{parameter}` written `{}`.
    fn routes() -> Vec<String> {
        let source = include_str!("router.rs");
        let source = source.split("#[cfg(test)]").next().unwrap_or(source);
        source
            .split(".route(")
            .skip(1)
            .filter_map(|rest| rest.trim_start().strip_prefix('"'))
            .filter_map(|rest| rest.split_once('"').map(|(path, _)| path))
            .map(|path| {
                // The public routes are nested under /api/v1; the probes and the webhook are not.
                if path.starts_with("/api/v1/") || matches!(path, "/webhook/" | "/live" | "/ready")
                {
                    normalized(path)
                } else {
                    normalized(&format!("/api/v1{path}"))
                }
            })
            .collect()
    }

    fn normalized(path: &str) -> String {
        let mut out = String::new();
        let mut in_parameter = false;
        for character in path.chars() {
            match character {
                '{' => {
                    in_parameter = true;
                    out.push_str("{}");
                }
                '}' => in_parameter = false,
                _ if in_parameter => {}
                other => out.push(other),
            }
        }
        out
    }

    /// openapi.yaml lists every route DM serves (retired ones included), so a client reading
    /// the contract never meets a route it does not describe.
    #[test]
    fn openapi_lists_every_route() {
        let openapi = include_str!("../../openapi.yaml");
        let section = openapi
            .split("\npaths:\n")
            .nth(1)
            .and_then(|rest| rest.split("\ncomponents:\n").next());
        assert!(section.is_some(), "openapi.yaml has paths and components");
        let paths: Vec<String> = section
            .unwrap_or_default()
            .lines()
            .filter(|line| line.starts_with("  /") && line.ends_with(':'))
            .map(|line| normalized(line.trim().trim_end_matches(':')))
            .collect();
        let routes = routes();
        assert!(routes.len() > 30, "found the routes: {routes:?}");
        let missing: Vec<&String> = routes
            .iter()
            .filter(|route| !paths.contains(route))
            .collect();
        assert!(
            missing.is_empty(),
            "openapi.yaml does not describe {missing:?}; add each with a schema and an example"
        );
        let unknown: Vec<&String> = paths.iter().filter(|path| !routes.contains(path)).collect();
        assert!(
            unknown.is_empty(),
            "openapi.yaml describes routes the router does not have: {unknown:?}"
        );
    }
}
