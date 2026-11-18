//! Silicon Contacts owns the address book. Every operation carries a proof for its caller and scope.
use bytes::Bytes;
use http::{Method, StatusCode, header};
use reqwest::{Client, RequestBuilder};
use secrecy::{ExposeSecret as _, SecretString};
use serde_json::{Value, json};
use std::{sync::LazyLock, time::Duration};
use uuid::Uuid;

use crate::{
    AppError, AppResult,
    application::{auth::AuthContext, state::AppState},
};

const APP: &str = "silicon-contacts";
pub(crate) const PHOTO_LIMIT: usize = 5 * 1024 * 1024 + 65536;
static HTTP: LazyLock<Result<Client, reqwest::Error>> = LazyLock::new(|| {
    Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .build()
});

fn unavailable() -> AppError {
    AppError::DependencyUnavailable {
        dependency: "silicon_contacts",
    }
}

fn request(
    state: &AppState,
    proof: &SecretString,
    method: Method,
    path: &[&str],
) -> AppResult<RequestBuilder> {
    let mut url = state.settings.contacts_url.clone();
    url.path_segments_mut()
        .map_err(|()| unavailable())?
        .clear()
        .push("v1")
        .extend(path.iter().copied());
    Ok(HTTP
        .as_ref()
        .map_err(|_| unavailable())?
        .request(method, url)
        .header(
            header::AUTHORIZATION,
            format!("Proof {}", proof.expose_secret()),
        ))
}

pub(crate) async fn user(
    state: &AppState,
    context: &AuthContext,
    method: Method,
    path: &[&str],
) -> AppResult<RequestBuilder> {
    let token = context.access_token().ok_or_else(|| {
        AppError::forbidden(
            "dm_sign_in_required",
            "Use your own DM access token to open Silicon Contacts.",
        )
    })?;
    let scope = if method == Method::GET {
        "contacts.read"
    } else {
        "contacts.write"
    };
    let proof = state
        .identity
        .issue_user_verification(token, APP, &[scope], &Uuid::new_v4().to_string())
        .await?;
    request(state, &proof.token, method, path)
}

pub(crate) async fn bytes(request: RequestBuilder) -> AppResult<Bytes> {
    let mut response = request.send().await.map_err(|_| unavailable())?;
    let status = response.status();
    let mut result = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| unavailable())? {
        if result.len() + chunk.len() > PHOTO_LIMIT {
            return Err(unavailable());
        }
        result.extend_from_slice(&chunk);
    }
    if !status.is_success() {
        let detail: Value = serde_json::from_slice(&result).unwrap_or_default();
        let message = detail["error"]["message"]
            .as_str()
            .unwrap_or("Silicon Contacts could not complete the request.");
        return Err(match status {
            StatusCode::NOT_FOUND => AppError::not_found(message),
            StatusCode::FORBIDDEN | StatusCode::UNAUTHORIZED => AppError::forbidden(
                "contacts_access_denied",
                "Silicon Contacts refused access to this address book.",
            ),
            StatusCode::TOO_MANY_REQUESTS => AppError::rate_limited(message).with_retry_after(10),
            s if s.is_client_error() => AppError::refused(s, "invalid_contact", message),
            _ => unavailable(),
        });
    }
    Ok(result.into())
}

pub(crate) async fn json(request: RequestBuilder) -> AppResult<Value> {
    let body = bytes(request).await?;
    if body.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_slice(&body).map_err(|_| unavailable())
}

pub(crate) async fn saved_sender(
    state: &AppState,
    recipient: &str,
    sender: &str,
) -> AppResult<bool> {
    let proof = state
        .identity
        .issue_app_verification(
            APP,
            &[
                "dm.contact.check",
                &format!("recipient:{recipient}"),
                &format!("sender:{sender}"),
            ],
            &Uuid::new_v4().to_string(),
        )
        .await?;
    let req = request(
        state,
        &proof.token,
        Method::POST,
        &["integrations", "dm", "contact-check"],
    )?;
    json(req.json(&json!({ "recipient": recipient, "sender": sender }))).await?["saved"]
        .as_bool()
        .ok_or_else(unavailable)
}
