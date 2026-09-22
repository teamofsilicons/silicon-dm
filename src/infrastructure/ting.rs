//! Ting v1's proof-bound WebSocket publisher. The database owns retries.
//!
//! This adapter deliberately requires fresh authority for each attempt. A live
//! socket does not provide application authority or solve background issuance.

use std::{sync::Arc, time::Duration};

use futures::{SinkExt as _, StreamExt as _};
use secrecy::{ExposeSecret as _, SecretString};
use serde_json::{Value, json};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::{
    net::TcpStream,
    sync::{mpsc, oneshot},
    time::{Instant, sleep, timeout, timeout_at},
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async_with_config,
    tungstenite::{Message, protocol::WebSocketConfig},
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::config::TingSettings;

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;
const MAX_BODY_BYTES: usize = 256 * 1024;
const MAX_FRAME_BYTES: usize = 1024 * 1024;

/// One attempt's single-use IAM proof and, when applicable, receiver test context.
/// Never derive Debug or persist this value in the handoff queue.
pub struct TingSendAuthority {
    /// A freshly minted proof over the exact prepared body.
    pub proof_token: SecretString,
    /// Paired audience credentials verified against the intended environment.
    pub testing: Option<TingTestingHeaders>,
}

/// Ting's audience test credentials, obtained from verified IAM testing context.
pub struct TingTestingHeaders {
    /// Ting's test app secret, not the DM test secret.
    pub app_secret: SecretString,
    /// Matching IAM environment key, not a DM-local root key.
    pub environment_key: SecretString,
}

/// Evidence of durable Ting acceptance, independent of DM delivered/read receipts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TingAcceptance {
    /// Permanent Ting notification ID.
    pub id: String,
    /// Original Ting acceptance time, unchanged on idempotent retry.
    pub created_at: OffsetDateTime,
    /// Whether Ting preferences suppress automatic delivery.
    pub silent: bool,
}

/// Redacted, retryable handoff outcome. No failure is a DM message failure.
#[derive(Clone, Debug, thiserror::Error)]
pub enum TingFailure {
    /// A connection or send was interrupted; acceptance may be uncertain.
    #[error("Ting transport is unavailable")]
    Transport,
    /// The deadline elapsed; retry only with fresh proof and unchanged body/key.
    #[error("Ting acceptance is uncertain after timeout")]
    Timeout,
    /// A response failed correlation, shape, or acceptance validation.
    #[error("Ting returned an invalid protocol response")]
    Protocol,
    /// Ting rejected this attempt; the code contains no upstream diagnostic text.
    #[error("Ting rejected the send ({0})")]
    Rejected(String),
    /// The prepared request itself is not a bounded keyed object.
    #[error("Ting prepared request is invalid")]
    InvalidRequest,
    /// The event originator must supply a current consented DM session.
    #[error("Ting delivery awaits the originator signing in to DM")]
    OriginatorAuthenticationRequired,
    /// IAM or the credential store could not safely authorize this attempt.
    #[error("Ting delivery authority is temporarily unavailable")]
    AuthorityUnavailable,
}

impl TingFailure {
    /// Stable diagnostic suitable for a durable handoff record.
    #[must_use]
    pub fn code(&self) -> &str {
        match self {
            Self::Transport => "ting_transport_unavailable",
            Self::Timeout => "ting_acceptance_uncertain",
            Self::Protocol => "ting_protocol_error",
            Self::Rejected(code) => code,
            Self::InvalidRequest => "ting_invalid_prepared_request",
            Self::OriginatorAuthenticationRequired => "ting_originator_authentication_required",
            Self::AuthorityUnavailable => "ting_authority_unavailable",
        }
    }
}

struct SendRequest {
    authority: TingSendAuthority,
    body: String,
    key: String,
    deadline: Instant,
    cancellation: CancellationToken,
    reply: oneshot::Sender<Result<TingAcceptance, TingFailure>>,
}

/// One prewarmed, reconnecting publisher socket with bounded in-memory work.
#[derive(Clone)]
pub struct TingSocket {
    requests: mpsc::Sender<SendRequest>,
    request_timeout: Duration,
}

impl TingSocket {
    /// Starts the driver. Cancellation closes the connection and abandons no
    /// durable work: callers retain their database leases until expiry/retry.
    #[must_use]
    pub fn start(settings: &TingSettings, cancellation: CancellationToken) -> Self {
        let (sender, receiver) = mpsc::channel(1);
        tokio::spawn(run(settings.clone(), receiver, cancellation));
        Self {
            requests: sender,
            request_timeout: settings.request_timeout,
        }
    }

    /// Sends exactly one attempt without retrying or changing its body bytes.
    ///
    /// # Errors
    /// Returns a redacted rejection, protocol/transport failure or uncertain
    /// timeout. The caller must obtain a fresh proof before another attempt.
    pub async fn send(
        &self,
        body: &str,
        authority: TingSendAuthority,
    ) -> Result<TingAcceptance, TingFailure> {
        if body.len() > MAX_BODY_BYTES
            || authority.proof_token.expose_secret().is_empty()
            || authority.testing.as_ref().is_some_and(|testing| {
                testing.app_secret.expose_secret().is_empty()
                    || testing.environment_key.expose_secret().is_empty()
            })
        {
            return Err(TingFailure::InvalidRequest);
        }
        let value: Value = serde_json::from_str(body).map_err(|_| TingFailure::InvalidRequest)?;
        let key = value
            .get("key")
            .and_then(Value::as_str)
            .filter(|key| !key.is_empty() && key.len() <= 200 && !key.chars().any(char::is_control))
            .ok_or(TingFailure::InvalidRequest)?
            .to_owned();
        let (reply, response) = oneshot::channel();
        let deadline = Instant::now() + self.request_timeout;
        // The worker may have less lease time left than this socket's timeout.
        // Dropping its publish future also cancels an already-started driver
        // request instead of letting that independent timeout keep it alive.
        let cancellation = CancellationToken::new();
        let _cancel_on_drop = cancellation.clone().drop_guard();
        let request = SendRequest {
            authority,
            body: body.to_owned(),
            key,
            deadline,
            cancellation,
            reply,
        };
        timeout_at(deadline, self.requests.send(request))
            .await
            .map_err(|_| TingFailure::Timeout)?
            .map_err(|_| TingFailure::Transport)?;
        timeout_at(deadline, response)
            .await
            .map_err(|_| TingFailure::Timeout)?
            .map_err(|_| TingFailure::Transport)?
    }
}

async fn run(
    settings: TingSettings,
    mut requests: mpsc::Receiver<SendRequest>,
    cancellation: CancellationToken,
) {
    let mut failures = 0_u32;
    loop {
        if requests.is_closed() || cancellation.is_cancelled() {
            return;
        }
        let Some(connected) = cancellation
            .run_until_cancelled(Box::pin(connect(&settings)))
            .await
        else {
            return;
        };
        match connected {
            Ok(mut socket) => {
                let started = Instant::now();
                loop {
                    tokio::select! {
                        biased;
                        () = cancellation.cancelled() => return,
                        request = requests.recv() => {
                            let Some(request) = request else { return; };
                            if request.reply.is_closed() || request.cancellation.is_cancelled()
                                || request.deadline <= Instant::now() { continue; }
                            let result = tokio::select! {
                                biased;
                                () = cancellation.cancelled() => return,
                                // A partial write or late acceptance is uncertain.
                                // Discard this connection; never reuse its proof.
                                () = request.cancellation.cancelled() => break,
                                result = timeout_at(request.deadline, send_one(&mut socket, &request)) => result,
                            };
                            let result = result.unwrap_or(Err(TingFailure::Timeout));
                            let reset = matches!(result, Err(TingFailure::Transport | TingFailure::Protocol | TingFailure::Timeout));
                            let _ = request.reply.send(result);
                            if reset { break; }
                        },
                        frame = socket.next() => {
                            match frame {
                                Some(Ok(Message::Ping(payload))) => {
                                    if !timeout(settings.request_timeout, socket.send(Message::Pong(payload))).await
                                        .is_ok_and(|result| result.is_ok()) { break; }
                                },
                                Some(Ok(Message::Pong(_))) => {},
                                // A publisher has no recipient subscriptions. Unsolicited
                                // application frames must not be mistaken for acceptance.
                                _ => break,
                            }
                        }
                    }
                }
                if started.elapsed() >= Duration::from_secs(60) {
                    failures = 0;
                }
            }
            Err(error) => {
                tracing::warn!(code = error.code(), "Ting publisher connection unavailable");
            }
        }
        let delay = reconnect_delay(failures);
        failures = failures.saturating_add(1);
        if cancellation
            .run_until_cancelled(sleep(delay))
            .await
            .is_none()
        {
            return;
        }
    }
}

async fn connect(settings: &TingSettings) -> Result<Socket, TingFailure> {
    let mut endpoint = settings.base_url.clone();
    let scheme = match endpoint.scheme() {
        "https" => "wss",
        "http" => "ws",
        _ => return Err(TingFailure::InvalidRequest),
    };
    endpoint
        .set_scheme(scheme)
        .map_err(|()| TingFailure::InvalidRequest)?;
    endpoint.set_path("/v1/ws");
    endpoint.set_query(Some("protocol=v1"));
    let config = WebSocketConfig::default()
        .max_message_size(Some(MAX_FRAME_BYTES))
        .max_frame_size(Some(MAX_FRAME_BYTES));
    timeout(settings.request_timeout, async {
        let (mut socket, _) = connect_async_with_config(endpoint.as_str(), Some(config), false)
            .await
            .map_err(|_| TingFailure::Transport)?;
        let ready = next_json(&mut socket).await?;
        if ready["op"] != "ready"
            || ready["protocol"] != "v1"
            || ready["receiver_id"].as_str().is_none_or(str::is_empty)
        {
            return Err(TingFailure::Protocol);
        }
        Ok(socket)
    })
    .await
    .map_err(|_| TingFailure::Timeout)?
}

async fn send_one(
    socket: &mut Socket,
    request: &SendRequest,
) -> Result<TingAcceptance, TingFailure> {
    let request_id = Uuid::new_v4().to_string();
    let mut frame = json!({"op":"send", "request_id":request_id,
        "proof_token":request.authority.proof_token.expose_secret(), "body":request.body});
    if let Some(testing) = &request.authority.testing {
        frame["headers"] = json!({
            "IAM_TEST_APP_SECRET":testing.app_secret.expose_secret(),
            "X-Testing-Environment-Key":testing.environment_key.expose_secret(),
        });
    }
    let frame = frame.to_string();
    if frame.len() > MAX_FRAME_BYTES {
        return Err(TingFailure::InvalidRequest);
    }
    socket
        .send(Message::Text(frame.into()))
        .await
        .map_err(|_| TingFailure::Transport)?;
    let response = next_json(socket).await?;
    if response["request_id"].as_str() != Some(&request_id) {
        return Err(TingFailure::Protocol);
    }
    if response["op"] == "error" {
        let code = response["error"]["code"]
            .as_str()
            .filter(|code| {
                !code.is_empty()
                    && code.len() <= 100
                    && code
                        .bytes()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
            })
            .unwrap_or("ting_rejected");
        return Err(TingFailure::Rejected(code.to_owned()));
    }
    acceptance(&response, &request.key)
}

fn acceptance(response: &Value, key: &str) -> Result<TingAcceptance, TingFailure> {
    if response["op"] != "accepted" || response["status"] != "accepted" || response["key"] != key {
        return Err(TingFailure::Protocol);
    }
    let id = response["id"]
        .as_str()
        .filter(|id| !id.is_empty() && id.len() <= 255 && !id.chars().any(char::is_control))
        .ok_or(TingFailure::Protocol)?
        .to_owned();
    let created_at = response["created_at"]
        .as_str()
        .filter(|time| time.ends_with('Z'))
        .ok_or(TingFailure::Protocol)?;
    let created_at =
        OffsetDateTime::parse(created_at, &Rfc3339).map_err(|_| TingFailure::Protocol)?;
    let silent = response["silent"].as_bool().ok_or(TingFailure::Protocol)?;
    Ok(TingAcceptance {
        id,
        created_at,
        silent,
    })
}

async fn next_json(socket: &mut Socket) -> Result<Value, TingFailure> {
    loop {
        match socket.next().await {
            Some(Ok(Message::Text(text))) => {
                return serde_json::from_str(&text).map_err(|_| TingFailure::Protocol);
            }
            Some(Ok(Message::Ping(payload))) => socket
                .send(Message::Pong(payload))
                .await
                .map_err(|_| TingFailure::Transport)?,
            Some(Ok(Message::Pong(_))) => {}
            _ => return Err(TingFailure::Transport),
        }
    }
}

fn reconnect_delay(failures: u32) -> Duration {
    let base = (1_u64 << failures.min(5)).min(30) * 1000;
    let randomness = u64::from(Uuid::new_v4().as_bytes()[0]);
    // +/-20%, capped at the documented maximum of thirty seconds.
    let millis = (base * (800 + randomness * 400 / 255) / 1000).min(30_000);
    Duration::from_millis(millis)
}

/// The durable worker depends on this boundary, never on a socket connection's
/// apparent authority. Implementations must mint fresh authority per invocation.
#[async_trait::async_trait]
pub trait TingPublisher: Send + Sync {
    /// Attempts the persisted request once.
    async fn publish(
        &self,
        claim: &crate::infrastructure::postgres::TingDeliveryClaim,
    ) -> Result<TingAcceptance, TingFailure>;
}

/// Shared publisher boundary for independent durable workers.
pub type SharedTingPublisher = Arc<dyn TingPublisher>;
