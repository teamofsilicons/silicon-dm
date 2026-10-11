//! Signing in to DM with Silicon Accounts as DM's public client: no app secret, just the
//! `client_id` `dm`.
//!
//! - **Carbons** use the device flow: [`PublicClient::start_device`] returns a code and a
//!   page to approve it on; [`PublicClient::wait_for_device`] polls until it is approved
//!   (honouring `interval` and `slow_down`, giving up when the code expires after 10 minutes).
//! - **Silicons** never see a page: `silicon-accounts login --app dm -q` mints a short-lived
//!   token (`slt_…`, single use, 2 minutes, bound to `dm`) that
//!   [`PublicClient::exchange_slt`] turns into DM tokens.
//! - Refresh tokens (`sar_…`) rotate on every use and presenting a used one ends the whole
//!   sign-in, so refresh one at a time and store the new pair before using it
//!   ([`PublicClient::refresh`]); [`PublicClient::revoke`] signs out.
//!
//! Errors are [`SignInError`]s with a stable `code`, a `message` that says exactly what
//! happened and a `hint` that says what to do next. Tokens never appear in them.
use serde::{Deserialize, Serialize};
use silicon_accounts_client::{AccountsClient, Secret, TokenResponse};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use url::Url;

use crate::ActorType;

/// The production Silicon Accounts.
pub const DEFAULT_ACCOUNTS_URL: &str = "https://accounts.teamofsilicons.com";
/// The longest a device sign-in waits: its codes expire after 10 minutes.
pub const DEVICE_MAX_WAIT: Duration = Duration::from_secs(600);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const USER_AGENT: &str = concat!("silicon-dm-client/", env!("CARGO_PKG_VERSION"));

/// How a Silicon gets a short-lived token for DM and hands it over.
pub const SILICON_SIGN_IN: &str = "silicon-accounts login --app dm -q | dm login --slt-stdin";
const SIGN_IN_AGAIN: &str = "Sign in again: `dm login` (a Carbon approves a code), or `silicon-accounts login --app dm -q | dm login --slt-stdin` (a Silicon).";

/// A sign-in failure: stable `code`, exact `message`, next step in `hint`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SignInError {
    code: String,
    message: String,
    hint: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<u16>,
    #[serde(skip)]
    retryable: bool,
}
impl SignInError {
    pub fn new(
        code: impl Into<String>,
        message: impl Into<String>,
        hint: impl Into<String>,
    ) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            hint: hint.into(),
            status: None,
            retryable: false,
        }
    }
    fn with_status(mut self, status: u16) -> Self {
        self.status = Some(status);
        self
    }
    fn retryable(mut self) -> Self {
        self.retryable = true;
        self
    }
    pub fn code(&self) -> &str {
        &self.code
    }
    pub fn message(&self) -> &str {
        &self.message
    }
    pub fn hint(&self) -> &str {
        &self.hint
    }
    pub fn status(&self) -> Option<u16> {
        self.status
    }
    /// A passing failure (Silicon Accounts unreachable or briefly unavailable): retrying
    /// later can work. Every other failure needs the action in the hint.
    pub fn is_retryable(&self) -> bool {
        self.retryable
    }
    /// The sign-in no longer exists at Silicon Accounts (signed out, revoked, refresh token
    /// reused, access removed): the account must sign in again.
    pub fn sign_in_ended(&self) -> bool {
        self.code == "sign_in_ended"
    }
}
impl std::fmt::Display for SignInError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} Hint: {}", self.message, self.hint)
    }
}
impl std::error::Error for SignInError {}

/// Who signed in, as Silicon Accounts tells DM.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionAccount {
    /// Permanent, case-sensitive key: store it.
    pub uuid: String,
    pub kind: ActorType,
    /// Current public id (`c:ada`, `si:scout`): show it; it can change.
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pfp_url: Option<String>,
    /// A Silicon's custodian.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custodian: Option<AccountRef>,
}
/// A reference to another account: its uuid and current id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountRef {
    pub uuid: String,
    #[serde(default)]
    pub id: String,
}

/// DM tokens for one account. Not `Debug`-printable with its secrets.
#[derive(Clone)]
pub struct Session {
    /// EdDSA JWT, `aud = dm`, 30 minutes.
    pub access_token: String,
    /// Rotating `sar_…` token: every refresh returns a new one and the old one stops working.
    pub refresh_token: String,
    pub expires_in: u64,
    /// Unix seconds when the access token expires.
    pub expires_at: u64,
    /// Unix seconds when the refresh family ends (the account must sign in again).
    pub refresh_expires_at: Option<u64>,
    pub scope: Option<String>,
    /// Always present on sign-in; Silicon Accounts includes it on refresh too.
    pub account: Option<SessionAccount>,
}
impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("expires_at", &self.expires_at)
            .field("refresh_expires_at", &self.refresh_expires_at)
            .field("account", &self.account)
            .finish_non_exhaustive()
    }
}

/// A started device sign-in: show `user_code` and `verification_uri` to the Carbon.
pub struct DeviceStart {
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: Option<String>,
    pub expires_in: u64,
    pub interval: u64,
    device_code: Secret,
}
impl std::fmt::Debug for DeviceStart {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceStart")
            .field("user_code", &self.user_code)
            .field("verification_uri", &self.verification_uri)
            .field("expires_in", &self.expires_in)
            .field("interval", &self.interval)
            .finish_non_exhaustive()
    }
}
/// Progress while waiting for a device sign-in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeviceEvent {
    /// Silicon Accounts asked to poll more slowly; the new interval in seconds.
    SlowDown { interval: u64 },
    /// One poll could not reach Silicon Accounts; polling continues until the code expires.
    Retrying { message: String },
}
/// One poll of a device sign-in.
#[derive(Debug)]
pub enum DevicePoll {
    Pending,
    SlowDown,
    Approved(Box<Session>),
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Normalizes a Silicon Accounts URL: https, or http for this machine; no query, fragment
/// or credentials; no trailing slash.
pub fn normalize_accounts_url(raw: &str) -> Result<String, SignInError> {
    let trimmed = raw.trim();
    let invalid = |why: String| {
        SignInError::new(
            "invalid_accounts_url",
            format!("The Silicon Accounts URL `{trimmed}` {why}."),
            format!(
                "Set ACCOUNTS_URL (or --accounts-url) to a URL such as {DEFAULT_ACCOUNTS_URL}, or http://localhost:<port> for a local stack."
            ),
        )
    };
    let mut url =
        Url::parse(trimmed).map_err(|e| invalid(format!("is not an absolute URL ({e})")))?;
    match url.scheme() {
        "https" => {}
        "http" if crate::is_loopback(&url) => {}
        "http" => {
            return Err(invalid(
                "uses plain http for a host that is not this machine".into(),
            ));
        }
        other => return Err(invalid(format!("uses the `{other}` scheme"))),
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(invalid("has a query string or fragment".into()));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(invalid("contains credentials".into()));
    }
    let path = url.path().trim_end_matches('/').to_owned();
    url.set_path(&path);
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

/// DM's public client at one Silicon Accounts.
#[derive(Clone)]
pub struct PublicClient {
    accounts: AccountsClient,
    http: reqwest::Client,
    base: String,
    app_id: String,
}
impl std::fmt::Debug for PublicClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PublicClient")
            .field("accounts_url", &self.base)
            .field("app_id", &self.app_id)
            .finish_non_exhaustive()
    }
}

impl PublicClient {
    /// `accounts_url` must be https, or http for this machine. Every request carries
    /// `X-Accounts-Telemetry: off`: DM records no telemetry and opts out of Silicon
    /// Accounts' own. The third argument is ignored and kept for compatibility.
    pub fn new(accounts_url: &str, app_id: &str, _telemetry: bool) -> Result<Self, SignInError> {
        let base = normalize_accounts_url(accounts_url)?;
        let app_id = app_id.trim().to_owned();
        if app_id.is_empty() {
            return Err(SignInError::new(
                "invalid_app_id",
                "The app id to sign in to is empty.",
                "DM signs in as the app `dm`; set DM_APP_ID only for a development app.",
            ));
        }
        let accounts = AccountsClient::builder()
            .base_url(&base)
            .user_agent(format!("dm/{}", env!("CARGO_PKG_VERSION")))
            .telemetry(false)
            .build()
            .map_err(|error| accounts_error(&error, "configure the Silicon Accounts client"))?;
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(REQUEST_TIMEOUT)
            .user_agent(USER_AGENT)
            .build()
            .map_err(|e| {
                SignInError::new(
                    "client_setup_failed",
                    format!("The HTTP client could not be created: {e}."),
                    "Check this system's TLS certificate store.",
                )
            })?;
        Ok(Self {
            accounts,
            http,
            base,
            app_id,
        })
    }
    /// The normalized Silicon Accounts URL (no trailing slash).
    pub fn accounts_url(&self) -> &str {
        &self.base
    }
    pub fn app_id(&self) -> &str {
        &self.app_id
    }

    /// Starts a Carbon's device sign-in. `label` names this machine in the Carbon's list of
    /// sign-ins (e.g. `dm CLI on build-box`).
    pub async fn start_device(&self, label: Option<&str>) -> Result<DeviceStart, SignInError> {
        let started = self
            .accounts
            .app_device_authorize(&self.app_id, None, label)
            .await
            .map_err(|error| match &error {
                silicon_accounts_client::Error::Api(api) if api.status < 500 => SignInError::new(
                    "device_flow_unavailable",
                    format!(
                        "Silicon Accounts refused to start a device sign-in for `{}`: {}",
                        self.app_id, api.message
                    ),
                    api.hint.clone().unwrap_or_else(|| {
                        "A Silicon signs in with a short-lived token instead: silicon-accounts login --app dm -q | dm login --slt-stdin.".into()
                    }),
                )
                .with_status(api.status),
                _ => accounts_error(&error, "start a device sign-in"),
            })?;
        Ok(DeviceStart {
            user_code: started.user_code,
            verification_uri: started.verification_uri,
            verification_uri_complete: started.verification_uri_complete,
            expires_in: started.expires_in,
            interval: started.interval.max(1),
            device_code: started.device_code,
        })
    }

    /// Polls a device sign-in once.
    pub async fn poll_device(&self, start: &DeviceStart) -> Result<DevicePoll, SignInError> {
        match self
            .accounts
            .app_device_poll(&self.app_id, start.device_code.expose())
            .await
        {
            Ok(silicon_accounts_client::DevicePoll::Pending) => Ok(DevicePoll::Pending),
            Ok(silicon_accounts_client::DevicePoll::SlowDown) => Ok(DevicePoll::SlowDown),
            Ok(silicon_accounts_client::DevicePoll::Denied) => Err(SignInError::new(
                "device_denied",
                format!(
                    "The sign-in with code {} was declined at Silicon Accounts.",
                    start.user_code
                ),
                "If that was a mistake, run dm login again and approve the new code.",
            )),
            Ok(silicon_accounts_client::DevicePoll::Expired) => Err(device_expired(start)),
            Ok(silicon_accounts_client::DevicePoll::Tokens(tokens)) => {
                Ok(DevicePoll::Approved(Box::new(sign_in_session(*tokens)?)))
            }
            Ok(_) => Err(SignInError::new(
                "device_poll_failed",
                "Silicon Accounts answered the device sign-in poll with a state this dm does not know.",
                "Update dm (silicon-apps keeps it current) and run dm login again.",
            )),
            Err(error) => Err(accounts_error(&error, "check the device sign-in")),
        }
    }

    /// Waits until the device sign-in is approved, declined or expired. Polls every
    /// `interval` seconds (5 more after each `slow_down`) and gives up when the code expires
    /// (at most 10 minutes). A poll that cannot reach Silicon Accounts is retried.
    pub async fn wait_for_device(
        &self,
        start: &DeviceStart,
        mut on_event: impl FnMut(&DeviceEvent),
    ) -> Result<Session, SignInError> {
        let lifetime = Duration::from_secs(start.expires_in.max(1)).min(DEVICE_MAX_WAIT);
        let deadline = tokio::time::Instant::now() + lifetime;
        let mut interval = start.interval.max(1);
        loop {
            let next = tokio::time::Instant::now() + Duration::from_secs(interval);
            if next > deadline {
                return Err(device_expired(start));
            }
            tokio::time::sleep_until(next).await;
            match self.poll_device(start).await {
                Ok(DevicePoll::Pending) => {}
                Ok(DevicePoll::SlowDown) => {
                    interval += 5;
                    on_event(&DeviceEvent::SlowDown { interval });
                }
                Ok(DevicePoll::Approved(session)) => return Ok(*session),
                Err(error) if error.is_retryable() => on_event(&DeviceEvent::Retrying {
                    message: error.message().to_owned(),
                }),
                Err(error) => return Err(error),
            }
        }
    }

    /// Exchanges a Silicon's short-lived token as DM's public client. The token is used up
    /// even when the exchange is refused, and is never logged or echoed.
    pub async fn exchange_slt(&self, slt: &str) -> Result<Session, SignInError> {
        let slt = slt.trim();
        if !slt.starts_with("slt_") {
            return Err(SignInError::new(
                "slt_not_a_short_lived_token",
                "That is not a Silicon Accounts short-lived token (they start with slt_); nothing was sent.",
                format!("Mint one for DM and pass it straight in: {SILICON_SIGN_IN}"),
            ));
        }
        let form = [
            ("grant_type", silicon_accounts_client::SLT_GRANT_TYPE),
            ("slt", slt),
            ("client_id", self.app_id.as_str()),
        ];
        let response = self.post_form("v1/oauth/token", &form).await?;
        let status = response.status().as_u16();
        let body = response
            .bytes()
            .await
            .map_err(|e| transport(&e, "read the token response"))?;
        if (200..300).contains(&status) {
            let tokens: TokenResponse = serde_json::from_slice(&body).map_err(|_| {
                SignInError::new(
                    "invalid_token_response",
                    "Silicon Accounts answered the token exchange with a body dm cannot read.",
                    "Check ACCOUNTS_URL points at Silicon Accounts, then mint a new token.",
                )
            })?;
            return sign_in_session(tokens);
        }
        Err(classify_slt_refusal(status, &body, &self.app_id))
    }

    /// Rotates the refresh token. Store the new pair before using it: the old refresh token
    /// stops working, and presenting it again ends the whole sign-in.
    pub async fn refresh(&self, refresh_token: &str) -> Result<Session, SignInError> {
        match self
            .accounts
            .refresh_app_public_client(&self.app_id, refresh_token)
            .await
        {
            Ok(tokens) => session(tokens),
            Err(silicon_accounts_client::Error::OAuth(error)) if error.error == "invalid_grant" => {
                Err(
                    SignInError::new("sign_in_ended", error.message(), SIGN_IN_AGAIN)
                        .with_status(error.status),
                )
            }
            Err(error) => Err(accounts_error(&error, "refresh the sign-in")),
        }
    }

    /// Signs out at Silicon Accounts by revoking the refresh token (RFC 7009: an unknown or
    /// already revoked token also succeeds). Other machines' sign-ins are not affected.
    pub async fn revoke(&self, refresh_token: &str) -> Result<(), SignInError> {
        let form = [
            ("token", refresh_token.trim()),
            ("token_type_hint", "refresh_token"),
            ("client_id", self.app_id.as_str()),
        ];
        let response = self.post_form("v1/oauth/revoke", &form).await?;
        let status = response.status().as_u16();
        if (200..300).contains(&status) {
            return Ok(());
        }
        let body = response.bytes().await.unwrap_or_default();
        let (error, description) = oauth_error(&body);
        let failure = SignInError::new(
            "revoke_failed",
            format!(
                "Silicon Accounts did not revoke the sign-in (HTTP {status}{}).",
                match (error.as_deref(), description.as_deref()) {
                    (Some(code), Some(text)) => format!(", {code}: {text}"),
                    (Some(code), None) => format!(", {code}"),
                    _ => String::new(),
                }
            ),
            "The local sign-in was removed anyway; the account can end remaining sign-ins of DM at Silicon Accounts.",
        )
        .with_status(status);
        Err(if status >= 500 {
            failure.retryable()
        } else {
            failure
        })
    }

    async fn post_form(
        &self,
        path: &str,
        form: &[(&str, &str)],
    ) -> Result<reqwest::Response, SignInError> {
        self.http
            .post(format!("{}/{path}", self.base))
            .header(reqwest::header::ACCEPT, "application/json")
            .header(silicon_accounts_client::TELEMETRY_HEADER, "off")
            .form(form)
            .send()
            .await
            .map_err(|e| transport(&e, "reach Silicon Accounts"))
    }
}

fn device_expired(start: &DeviceStart) -> SignInError {
    SignInError::new(
        "device_expired",
        format!(
            "The sign-in code {} expired before it was approved (codes last {} minutes).",
            start.user_code,
            start.expires_in.clamp(60, 600) / 60
        ),
        "Run dm login again and approve the new code at Silicon Accounts within 10 minutes.",
    )
}

fn transport(error: &reqwest::Error, action: &str) -> SignInError {
    let why = if error.is_timeout() {
        "the request timed out".to_owned()
    } else if error.is_connect() {
        "the connection failed".to_owned()
    } else {
        error.to_string()
    };
    SignInError::new(
        "accounts_unreachable",
        format!("dm could not {action}: {why}."),
        "Check the network and ACCOUNTS_URL, then try again.",
    )
    .retryable()
}

fn accounts_error(error: &silicon_accounts_client::Error, action: &str) -> SignInError {
    use silicon_accounts_client::Error as E;
    match error {
        E::Http { message, hint, .. } => SignInError::new(
            "accounts_unreachable",
            format!("dm could not {action}: {message}"),
            hint.clone(),
        )
        .retryable(),
        E::Api(api) => {
            let failure = SignInError::new(
                api.code.clone(),
                api.message.clone(),
                api.hint.clone().unwrap_or_else(|| {
                    "Try again; if it keeps failing, report it with dm report.".into()
                }),
            )
            .with_status(api.status);
            if api.status >= 500 || api.status == 429 {
                failure.retryable()
            } else {
                failure
            }
        }
        E::OAuth(oauth) => SignInError::new(oauth.error.clone(), oauth.message(), SIGN_IN_AGAIN)
            .with_status(oauth.status),
        other => SignInError::new(
            "accounts_error",
            format!("dm could not {action}: {other}"),
            "Check ACCOUNTS_URL points at Silicon Accounts, then try again.",
        ),
    }
}

fn non_empty(value: String) -> Option<String> {
    Some(value).filter(|v| !v.trim().is_empty())
}

/// A token response from a sign-in: it must carry the refresh token and the account.
fn sign_in_session(tokens: TokenResponse) -> Result<Session, SignInError> {
    let session = session(tokens)?;
    if session.account.is_none() {
        return Err(SignInError::new(
            "invalid_token_response",
            "Silicon Accounts signed the account in but did not say which account it is.",
            "Check ACCOUNTS_URL points at Silicon Accounts, then sign in again.",
        ));
    }
    Ok(session)
}

fn session(tokens: TokenResponse) -> Result<Session, SignInError> {
    let refresh_token = tokens
        .refresh_token
        .map(Secret::into_inner)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| {
            SignInError::new(
                "invalid_token_response",
                "Silicon Accounts returned no refresh token, so this sign-in could not be kept.",
                "DM's sign-in setup must allow its command-line tool (device flow and public client); report it with dm report.",
            )
        })?;
    let account = tokens.account.map(|account| SessionAccount {
        uuid: account.uuid,
        kind: match account.kind {
            silicon_accounts_client::AccountKind::Carbon => ActorType::Carbon,
            silicon_accounts_client::AccountKind::Silicon => ActorType::Silicon,
        },
        id: account.id,
        display_name: non_empty(account.display_name),
        pfp_url: non_empty(account.pfp_url),
        custodian: account.custodian.map(|custodian| AccountRef {
            uuid: custodian.uuid,
            id: custodian.id,
        }),
    });
    Ok(Session {
        access_token: tokens.access_token.into_inner(),
        refresh_token,
        expires_in: tokens.expires_in,
        expires_at: now().saturating_add(tokens.expires_in),
        refresh_expires_at: tokens
            .refresh_token_expires_at
            .and_then(|at| u64::try_from(at.unix_timestamp()).ok()),
        scope: tokens.scope,
        account,
    })
}

/// `{"error": "...", "error_description": "..."}` from an OAuth endpoint.
fn oauth_error(body: &[u8]) -> (Option<String>, Option<String>) {
    let value: serde_json::Value = serde_json::from_slice(body).unwrap_or_default();
    let text = |key: &str| {
        value
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    };
    (
        text("error").or_else(|| {
            value
                .pointer("/error/code")
                .and_then(|v| v.as_str())
                .map(str::to_owned)
        }),
        text("error_description").or_else(|| {
            value
                .pointer("/error/message")
                .and_then(|v| v.as_str())
                .map(str::to_owned)
        }),
    )
}

/// Silicon Accounts answers every refused short-lived token with `invalid_grant`; its
/// description says why. The codes let a Silicon branch without parsing prose.
fn classify_slt_refusal(status: u16, body: &[u8], app_id: &str) -> SignInError {
    let (error, description) = oauth_error(body);
    let description = description.unwrap_or_default();
    let hint = format!(
        "Mint a fresh one for DM and pass it straight in: {SILICON_SIGN_IN} (each token works once, for 2 minutes, for the app it names)."
    );
    let lower = description.to_lowercase();
    let (code, message) = match error.as_deref() {
        Some("invalid_client" | "unauthorized_client") => {
            return SignInError::new(
                "public_client_disabled",
                format!(
                    "Silicon Accounts does not let DM's command-line tool exchange short-lived tokens for `{app_id}` without a secret{}.",
                    if description.is_empty() { String::new() } else { format!(" ({description})") }
                ),
                "DM's sign-in setup needs public_client turned on; a Carbon can use dm login (device flow) meanwhile.",
            )
            .with_status(status);
        }
        Some("invalid_grant") if lower.contains("already used") => {
            ("slt_already_used", description)
        }
        Some("invalid_grant") if lower.contains("was issued for the app") => {
            ("slt_wrong_app", description)
        }
        Some("invalid_grant") if lower.contains("expired") => ("slt_expired", description),
        Some("invalid_grant") if lower.contains("not known") => ("slt_unknown", description),
        Some("invalid_grant") if lower.contains("must be a short-lived token") => {
            ("slt_not_a_short_lived_token", description)
        }
        Some("invalid_grant")
            if lower.contains("rotated")
                || lower.contains("removed")
                || lower.contains("ended") =>
        {
            ("slt_sign_in_ended", description)
        }
        Some(_) if !description.is_empty() => ("slt_refused", description),
        Some(other) => (
            "slt_refused",
            format!("Silicon Accounts refused the short-lived token ({other})."),
        ),
        None if status >= 500 => {
            return SignInError::new(
                "accounts_unavailable",
                format!("Silicon Accounts could not exchange the short-lived token right now (HTTP {status})."),
                format!("The token may be used up; mint a fresh one in a moment: {SILICON_SIGN_IN}"),
            )
            .with_status(status)
            .retryable();
        }
        None => (
            "slt_refused",
            format!("Silicon Accounts refused the short-lived token (HTTP {status})."),
        ),
    };
    SignInError::new(code, message, hint).with_status(status)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refusal(description: &str) -> SignInError {
        let body = serde_json::json!({"error":"invalid_grant","error_description":description});
        classify_slt_refusal(400, body.to_string().as_bytes(), "dm")
    }

    #[test]
    fn short_lived_token_refusals_have_stable_codes_and_a_fresh_token_hint() {
        for (description, code) in [
            (
                "The short-lived token was already used; each one works once. Get a new one.",
                "slt_already_used",
            ),
            (
                "The short-lived token expired at 2026-10-10T10:00:00.000Z (they last 120 seconds); get a new one with `silicon-accounts login --app dm`.",
                "slt_expired",
            ),
            (
                "The short-lived token was issued for the app 'remind', not for 'dm'; get one for 'dm' with `silicon-accounts login --app dm`.",
                "slt_wrong_app",
            ),
            (
                "The short-lived token is not known: it is mistyped or was never issued.",
                "slt_unknown",
            ),
            (
                "slt must be a short-lived token (it starts with slt_), but this is a refresh token.",
                "slt_not_a_short_lived_token",
            ),
            (
                "The short-lived token was issued at … by a sign-in of si:scout that ended when its custodian rotated its STK at …",
                "slt_sign_in_ended",
            ),
            ("Something new.", "slt_refused"),
        ] {
            let error = refusal(description);
            assert_eq!(error.code(), code, "{description}");
            assert_eq!(error.message(), description);
            assert!(
                error
                    .hint()
                    .contains("silicon-accounts login --app dm -q | dm login --slt-stdin")
            );
            assert!(!error.is_retryable());
        }
        let disabled = classify_slt_refusal(
            401,
            br#"{"error":"invalid_client","error_description":"public client not allowed"}"#,
            "dm",
        );
        assert_eq!(disabled.code(), "public_client_disabled");
        let unavailable = classify_slt_refusal(503, b"upstream", "dm");
        assert_eq!(unavailable.code(), "accounts_unavailable");
        assert!(unavailable.is_retryable());
    }

    #[test]
    fn accounts_urls_are_https_or_this_machine() {
        assert_eq!(
            normalize_accounts_url("https://accounts.teamofsilicons.com/")
                .ok()
                .as_deref(),
            Some("https://accounts.teamofsilicons.com")
        );
        assert_eq!(
            normalize_accounts_url(" http://localhost:9590 ")
                .ok()
                .as_deref(),
            Some("http://localhost:9590")
        );
        for bad in [
            "http://accounts.teamofsilicons.com",
            "ftp://localhost",
            "https://a:b@accounts.teamofsilicons.com",
            "https://accounts.teamofsilicons.com/?x",
            "accounts.teamofsilicons.com",
        ] {
            assert_eq!(
                normalize_accounts_url(bad).map_err(|e| e.code().to_owned()),
                Err("invalid_accounts_url".to_owned()),
                "{bad}"
            );
        }
    }

    #[tokio::test]
    async fn input_that_is_not_a_short_lived_token_is_never_sent() {
        let client = PublicClient::new("http://127.0.0.1:9", "dm", false).expect("client");
        let error = client
            .exchange_slt("sar_refresh-token-pasted-by-mistake")
            .await
            .expect_err("refused locally");
        assert_eq!(error.code(), "slt_not_a_short_lived_token");
        assert!(!error.message().contains("sar_"));
    }
}
