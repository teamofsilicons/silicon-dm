//! Enrols accounts with Ting (only when `DM_TING_URL` is set).
//!
//! Ting accepts a recipient only with a proof of the recipient's own agreement: a User
//! verification proof DM obtains with the account's current DM access token (receiving
//! app Ting, named by `DM_TING_APP_ID`; scope `subscriptions.register`). DM enrols an account
//! the first time it sees one of its requests; the proof's refresh token is kept
//! encrypted with `DM_DATA_KEY`.

use std::sync::OnceLock;

use aes_gcm::{
    Aes256Gcm, KeyInit as _,
    aead::{Aead as _, AeadCore as _, OsRng, Payload},
};
use secrecy::ExposeSecret as _;
use serde_json::{Value, json};

use crate::{
    AppError, AppResult,
    application::{auth::AuthContext, state::AppState},
};

/// Scope DM asks for when enrolling a recipient.
pub const SUBSCRIBE_SCOPE: &str = "subscriptions.register";
// This is a persisted encryption/row label, not the scope requested from Accounts.
// Preserve it so existing grants remain decryptable without a data migration.
const GRANT_PURPOSE: &str = "tings.subscribe";
const MAX_RESPONSE_BYTES: usize = 64 * 1024;

fn http() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(reqwest::Client::new)
}

/// Starts enrolling the caller with Ting in the background, when Ting is configured and
/// the caller came with its own access token. Never delays or fails the request.
pub(crate) fn spawn_enrolment(state: &AppState, context: &AuthContext) {
    if !state.ting_enabled() || context.access_token().is_none() {
        return;
    }
    let state = state.clone();
    let context = context.clone();
    tokio::spawn(async move {
        if let Err(error) = enrol(&state, &context).await {
            tracing::warn!(
                code = error.code(),
                "Ting enrolment failed; it is retried on a later request"
            );
        }
    });
}

async fn enrol(state: &AppState, context: &AuthContext) -> AppResult<()> {
    let app_id = state.settings.accounts.app_id.clone();
    let (Some(ting), Some(token)) = (state.settings.ting.base_url.clone(), context.access_token())
    else {
        return Ok(());
    };
    if !state
        .store
        .reserve_ting_enrolment(&app_id, &context.actor)
        .await?
    {
        return Ok(());
    }
    let key = format!(
        "dm-ting-enrol-{}-{}",
        context.actor.id,
        uuid::Uuid::now_v7().simple()
    );
    let proof = match state
        .identity
        .issue_user_verification(token, &state.settings.ting.app_id, &[SUBSCRIBE_SCOPE], &key)
        .await
    {
        Ok(proof) => proof,
        Err(error) => {
            state
                .store
                .finish_ting_enrolment(&app_id, &context.actor, None, Some(error.code()))
                .await?;
            return Err(error);
        }
    };
    if let Some(refresh) = &proof.refresh_token {
        let (nonce, ciphertext) = seal(
            state,
            &app_id,
            context.actor.id.as_str(),
            refresh.expose_secret(),
        )?;
        state
            .store
            .save_ting_proof_grant(
                &app_id,
                &context.actor,
                GRANT_PURPOSE,
                &proof.proof_id,
                &nonce,
                &ciphertext,
                proof.refresh_expires_at,
            )
            .await?;
    }
    let outcome = subscribe(
        state,
        ting,
        &proof.token,
        &app_id,
        context.actor.id.as_str(),
    )
    .await;
    match outcome {
        Ok(subscription) => {
            state
                .store
                .finish_ting_enrolment(&app_id, &context.actor, Some(&subscription), None)
                .await
        }
        Err(code) => {
            state
                .store
                .finish_ting_enrolment(&app_id, &context.actor, None, Some(&code))
                .await?;
            Err(AppError::DependencyUnavailable { dependency: "ting" })
        }
    }
}

/// `POST {ting}/v1/subscriptions` with a User verification proof; the subscription id or
/// a short failure code.
async fn subscribe(
    state: &AppState,
    mut endpoint: url::Url,
    proof: &secrecy::SecretString,
    app_id: &str,
    account: &str,
) -> Result<String, String> {
    endpoint.set_path("/v1/subscriptions");
    let response = http()
        .post(endpoint)
        .timeout(state.settings.ting.request_timeout)
        .bearer_auth(proof.expose_secret())
        .json(&json!({ "app_id": app_id, "for": account }))
        .send()
        .await
        .map_err(|_| "ting_unreachable".to_owned())?;
    if !response.status().is_success() {
        return Err(format!("ting_http_{}", response.status().as_u16()));
    }
    let body = response
        .bytes()
        .await
        .map_err(|_| "ting_unreachable".to_owned())?;
    if body.len() > MAX_RESPONSE_BYTES {
        return Err("ting_response_too_large".to_owned());
    }
    serde_json::from_slice::<Value>(&body)
        .ok()
        .and_then(|value| value.get("id").and_then(Value::as_str).map(str::to_owned))
        .filter(|id| !id.is_empty() && id.len() <= 255)
        .ok_or_else(|| "ting_invalid_subscription".to_owned())
}

/// AES-256-GCM with a key derived from `DM_DATA_KEY`, bound to the app and account.
fn seal(
    state: &AppState,
    app_id: &str,
    account: &str,
    secret: &str,
) -> AppResult<(Vec<u8>, Vec<u8>)> {
    let key = state
        .settings
        .data_key
        .derive("silicon-dm ting proof refresh tokens v1");
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(AppError::internal)?;
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let aad = format!("{app_id}\u{1f}{account}\u{1f}{GRANT_PURPOSE}");
    let ciphertext = cipher
        .encrypt(
            &nonce,
            Payload {
                msg: secret.as_bytes(),
                aad: aad.as_bytes(),
            },
        )
        .map_err(|_| {
            AppError::internal(anyhow::anyhow!("proof refresh token encryption failed"))
        })?;
    Ok((nonce.to_vec(), ciphertext))
}

fn open(
    state: &AppState,
    app_id: &str,
    account: &str,
    nonce: &[u8],
    ciphertext: &[u8],
) -> AppResult<String> {
    let key = state
        .settings
        .data_key
        .derive("silicon-dm ting proof refresh tokens v1");
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(AppError::internal)?;
    let nonce: [u8; 12] = nonce.try_into().map_err(|_| {
        AppError::internal(anyhow::anyhow!("stored proof nonce has the wrong length"))
    })?;
    let aad = format!("{app_id}\u{1f}{account}\u{1f}{GRANT_PURPOSE}");
    let plaintext = cipher
        .decrypt(
            &aes_gcm::Nonce::from(nonce),
            Payload {
                msg: ciphertext,
                aad: aad.as_bytes(),
            },
        )
        .map_err(|_| {
            AppError::internal(anyhow::anyhow!(
                "stored proof refresh token cannot be decrypted (was DM_DATA_KEY rotated?)"
            ))
        })?;
    String::from_utf8(plaintext).map_err(AppError::internal)
}

/// Ting no longer knows a recipient: enrol it again with the stored proof refresh token
/// (no live session needed). Without a usable stored proof the enrolment is forgotten and
/// the recipient's next request enrols it.
///
/// A proof refresh token works once (presenting a used one revokes the proof), so one
/// re-enrolment per account runs at a time across every DM process (a session advisory
/// lock); a second caller returns at once.
pub(crate) async fn reenrol(state: &AppState, target: &crate::domain::ActorRef) -> AppResult<()> {
    if state.settings.ting.base_url.is_none() {
        return Ok(());
    }
    let lock = format!(
        "dm.ting.reenrol\u{1f}{}\u{1f}{}",
        state.settings.accounts.app_id, target.id
    );
    let mut connection = state.store.pool().acquire().await?;
    let locked: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock(hashtextextended($1, 0))")
        .bind(&lock)
        .fetch_one(&mut *connection)
        .await?;
    if !locked {
        return Ok(());
    }
    let result = reenrol_locked(state, target).await;
    if sqlx::query("SELECT pg_advisory_unlock(hashtextextended($1, 0))")
        .bind(&lock)
        .execute(&mut *connection)
        .await
        .is_err()
    {
        // Closing the session releases the lock for certain.
        let _ = connection.close().await;
    }
    result
}

async fn reenrol_locked(state: &AppState, target: &crate::domain::ActorRef) -> AppResult<()> {
    let app_id = state.settings.accounts.app_id.clone();
    let Some(ting) = state.settings.ting.base_url.clone() else {
        return Ok(());
    };
    let grant = state
        .store
        .ting_proof_grant(&app_id, target, GRANT_PURPOSE)
        .await?;
    let Some((nonce, ciphertext)) = grant else {
        return state.store.forget_ting_enrolment(&app_id, target).await;
    };
    let refreshed = match open(state, &app_id, target.id.as_str(), &nonce, &ciphertext) {
        Ok(refresh) => {
            state
                .identity
                .refresh_proof(&secrecy::SecretString::from(refresh))
                .await
        }
        Err(error) => Err(error),
    };
    let proof = match refreshed {
        Ok(proof) => proof,
        Err(error) => {
            tracing::info!(
                code = error.code(),
                "stored Ting proof unusable; the recipient re-enrols on its next request"
            );
            return state.store.forget_ting_enrolment(&app_id, target).await;
        }
    };
    if let Some(refresh) = &proof.refresh_token {
        let (nonce, ciphertext) =
            seal(state, &app_id, target.id.as_str(), refresh.expose_secret())?;
        state
            .store
            .save_ting_proof_grant(
                &app_id,
                target,
                GRANT_PURPOSE,
                &proof.proof_id,
                &nonce,
                &ciphertext,
                proof.refresh_expires_at,
            )
            .await?;
    }
    let accepted = subscribe(state, ting, &proof.token, &app_id, target.id.as_str())
        .await
        .is_ok();
    if !accepted {
        state.store.forget_ting_enrolment(&app_id, target).await?;
    }
    Ok(())
}
