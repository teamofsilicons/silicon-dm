//! Authenticated WebSocket lifecycle, replay, ACKs, and command dispatch.

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use axum::extract::ws::{CloseFrame, Message as SocketMessage, WebSocket};
use futures::StreamExt as _;
use time::OffsetDateTime;
use tokio::time::{Instant, MissedTickBehavior, interval_at};
use uuid::Uuid;

use crate::{
    AppError, AppResult,
    application::{
        auth::{AuthContext, PresentedCredential},
        commands::{
            OpenRealtimeSessionCommand, RecordReceiptCommand, SendMessageCommand,
            UpdateRealtimeActivityCommand,
        },
        messaging::{prepare_message_content, validate_device_id},
        ports::AuthenticationRequest,
        state::AppState,
    },
    domain::{ActorId, ActorRef},
};

use super::{
    ClientFrame, DeliveryWakeup, HubRegistration, PROTOCOL_VERSION, RealtimeTarget, ServerFrame,
};

const REPLAY_BATCH_SIZE: usize = 100;
const REPLAY_TIME_BUDGET: Duration = Duration::from_millis(25);
const REPLAY_INTERVAL: Duration = Duration::from_secs(1);
const HEARTBEAT_CLOSE_CODE: u16 = 4000;
const HEARTBEAT_CLOSE_REASON: &str = "heartbeat-timeout";

fn log_database_failure(error: &AppError, session_id: Uuid) {
    if let AppError::Database(source) = error {
        if let Some(database) = source.as_database_error() {
            tracing::warn!(%session_id, sqlstate = ?database.code(), table = database.table(), constraint = database.constraint(), "realtime database operation failed");
        } else {
            tracing::warn!(%session_id, "realtime database connection or decoding failed");
        }
    }
}

/// Runs one already-authenticated WebSocket until disconnect or heartbeat timeout.
///
/// Authentication happens before the HTTP upgrade. Credentials remain only in
/// this request-scoped task and are never stored in the realtime lease.
pub async fn serve_socket(
    mut socket: WebSocket,
    state: AppState,
    authority: AuthContext,
    actors: Vec<ActorRef>,
    consumer_id: String,
    requested_testing_generation: Option<i64>,
) {
    if let Err(error) = validate_device_id(&consumer_id) {
        let _ = send_error(&mut socket, &error).await;
        let _ = close_socket(&mut socket, 1008, "invalid-device-id").await;
        return;
    }
    let targets = actors.iter().cloned().map(|actor| RealtimeTarget {
        organization_id: authority.organization_id.clone(),
        actor,
    });
    let registration = state
        .realtime
        .register(targets, state.settings.realtime.outbound_capacity);
    let session_id = registration.connection_id();
    let Ok(opening_fence) = environment_fence(&state).await else {
        let _ = close_socket(&mut socket, 4001, "testing-environment-changed").await;
        return;
    };
    let lease_expires_at = lease_deadline(state.settings.realtime.heartbeat_timeout);
    let open = state
        .store
        .open_realtime_session(OpenRealtimeSessionCommand {
            session_id,
            instance_id: state.instance_id.to_string(),
            consumer_id: consumer_id.clone(),
            authenticated_subject: authority.actor.id.to_string(),
            organization_id: authority.organization_id.clone(),
            actors: actors.clone(),
            lease_expires_at,
        })
        .await;
    drop(opening_fence);
    if let Err(error) = open {
        log_database_failure(&error, session_id);
        tracing::warn!(%session_id, code = error.code(), "realtime session could not open");
        let _ = send_error(&mut socket, &error).await;
        let _ = close_socket(&mut socket, 1011, "session-open-failed").await;
        return;
    }

    let mut runtime = SessionRuntime {
        reset_cursors: state.testing_generation.is_some()
            && state.testing_generation != requested_testing_generation,
        actors: actors
            .into_iter()
            .map(|actor| (actor.id.clone(), actor))
            .collect(),
        authority,
        authorization_revision: -1,
        consumer_id,
        last_valid_pong: Instant::now(),
        pending_ping_id: None,
        registration,
        sent_through: BTreeMap::new(),
        next_replay_actor: 0,
        session_id,
        state,
    };
    let exit = match runtime.run(&mut socket).await {
        Ok(exit) => exit,
        Err(error) => {
            log_database_failure(&error, session_id);
            tracing::warn!(%session_id, code = error.code(), "realtime session failed");
            let _ = send_error(&mut socket, &error).await;
            SocketExit::server(1011, "internal-error")
        }
    };
    if exit.send_close {
        let _ = close_socket(&mut socket, exit.code, &exit.reason).await;
    }
    if let Ok(_closing_fence) = environment_fence(&runtime.state).await
        && let Err(error) = runtime
            .state
            .store
            .close_realtime_session(session_id, exit.code, &exit.reason)
            .await
    {
        log_database_failure(&error, session_id);
        tracing::warn!(%session_id, code = error.code(), "realtime session close was not persisted");
    }
}

struct SessionRuntime {
    reset_cursors: bool,
    actors: BTreeMap<ActorId, ActorRef>,
    authority: AuthContext,
    authorization_revision: i64,
    consumer_id: String,
    last_valid_pong: Instant,
    pending_ping_id: Option<String>,
    registration: HubRegistration,
    sent_through: BTreeMap<ActorId, i64>,
    next_replay_actor: usize,
    session_id: Uuid,
    state: AppState,
}

impl SessionRuntime {
    // Keep the socket event selection and its lifecycle fence in one visible loop.
    #[allow(clippy::too_many_lines)]
    async fn run(&mut self, socket: &mut WebSocket) -> AppResult<SocketExit> {
        let startup_fence = environment_fence(&self.state).await?;
        let mut authorization_changes = self.state.realtime.authorization_changes();
        let mut disconnects = self.state.realtime.disconnects();
        self.revalidate_authority().await?;
        self.authorization_revision =
            crate::api::webhook::authorization_revision(&self.state).await?;
        let actors = self.actors.values().cloned().collect::<Vec<_>>();
        let mut acknowledged = self
            .state
            .store
            .acknowledged_cursors(&self.authority.organization_id, &actors, &self.consumer_id)
            .await?;
        if self.reset_cursors {
            for sequence in acknowledged.values_mut() {
                *sequence = 0;
            }
        }
        self.sent_through = actors
            .iter()
            .map(|actor| {
                let through = acknowledged.get(actor.id.as_str()).copied().unwrap_or(0);
                (actor.id.clone(), through)
            })
            .collect();
        send_server_frame(
            socket,
            &ServerFrame::Ready {
                protocol_version: PROTOCOL_VERSION,
                testing_generation: self.state.testing_generation,
                connection_id: self.session_id,
                actors: self.actors.keys().cloned().collect(),
                acknowledged_through: acknowledged,
            },
        )
        .await?;
        drop(startup_fence);

        let heartbeat_interval = self.state.settings.realtime.heartbeat_interval;
        let mut heartbeat = interval_at(Instant::now() + heartbeat_interval, heartbeat_interval);
        heartbeat.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut replay = interval_at(Instant::now() + REPLAY_INTERVAL, REPLAY_INTERVAL);
        replay.set_missed_tick_behavior(MissedTickBehavior::Skip);

        loop {
            let event = tokio::select! {
                incoming = socket.next() => SessionEvent::Incoming(incoming),
                frame = self.registration.recv() => {
                    SessionEvent::LocalDelivery(frame)
                },
                _ = heartbeat.tick() => SessionEvent::Heartbeat,
                _ = replay.tick() => SessionEvent::Replay,
                _ = authorization_changes.changed() => SessionEvent::AuthorizationChanged,
                _ = disconnects.changed() => SessionEvent::Disconnect,
            };
            // Fence every command, lease write, and delivery read against a
            // simultaneous environment clean, key rotation, or deletion.
            let _environment_fence = match environment_fence(&self.state).await {
                Ok(fence) => fence,
                Err(error) => return Ok(authority_exit(&error)),
            };
            match event {
                SessionEvent::Incoming(Some(Ok(message))) => {
                    if matches!(message, SocketMessage::Text(_))
                        && let Err(error) = self.revalidate_authority().await
                    {
                        return Ok(authority_exit(&error));
                    }
                    if let Some(exit) = self.handle_socket_message(socket, message).await? {
                        return Ok(exit);
                    }
                }
                SessionEvent::Incoming(Some(Err(error))) => {
                    tracing::debug!(%self.session_id, error = ?error, "WebSocket transport closed");
                    return Ok(SocketExit::peer(1006, "transport-error"));
                }
                SessionEvent::Incoming(None) => {
                    return Ok(SocketExit::peer(1000, "client-disconnected"));
                }
                SessionEvent::LocalDelivery(Some(frame)) => {
                    if let Err(error) = self.check_authority_revision().await {
                        return Ok(authority_exit(&error));
                    }
                    self.handle_local_delivery(socket, frame.as_ref()).await?;
                }
                SessionEvent::LocalDelivery(None) => {
                    return Err(AppError::internal(anyhow::anyhow!(
                        "realtime local delivery queue closed unexpectedly"
                    )));
                }
                SessionEvent::Heartbeat => {
                    if let Err(error) = self.revalidate_authority().await {
                        return Ok(authority_exit(&error));
                    }
                    if let (Some(registry), Some(id), Some(generation)) = (
                        &self.state.testing,
                        self.state.testing_environment,
                        self.state.testing_generation,
                    ) && let Err(error) = registry.touch(id, generation).await
                    {
                        return Ok(authority_exit(&error));
                    }
                    if self.last_valid_pong.elapsed()
                        >= self.state.settings.realtime.heartbeat_timeout
                    {
                        return Ok(SocketExit::server(
                            HEARTBEAT_CLOSE_CODE,
                            HEARTBEAT_CLOSE_REASON,
                        ));
                    }
                    self.state
                        .store
                        .refresh_realtime_session(
                            self.session_id,
                            lease_deadline(self.state.settings.realtime.heartbeat_timeout),
                            false,
                        )
                        .await?;
                    let ping_id = Uuid::now_v7().to_string();
                    self.pending_ping_id = Some(ping_id.clone());
                    send_server_frame(socket, &ServerFrame::Ping { ping_id }).await?;
                }
                SessionEvent::Replay => {
                    if let Err(error) = self.check_authority_revision().await {
                        return Ok(authority_exit(&error));
                    }
                    self.replay_all(socket).await?;
                }
                SessionEvent::AuthorizationChanged => {
                    if let Err(error) = self.revalidate_authority().await {
                        return Ok(authority_exit(&error));
                    }
                }
                SessionEvent::Disconnect => {
                    let reason = disconnects
                        .borrow_and_update()
                        .clone()
                        .unwrap_or_else(|| "authorization-changed".to_owned());
                    return Ok(SocketExit::server(4001, &reason));
                }
            }
        }
    }

    async fn ensure_environment_active(&self) -> AppResult<()> {
        if let (Some(registry), Some(id), Some(generation)) = (
            &self.state.testing,
            self.state.testing_environment,
            self.state.testing_generation,
        ) {
            registry.ensure_active(id, generation).await?;
        }
        Ok(())
    }

    async fn check_authority_revision(&mut self) -> AppResult<()> {
        self.ensure_environment_active().await?;
        let revision = crate::api::webhook::authorization_revision(&self.state).await?;
        if revision != self.authorization_revision {
            self.revalidate_authority().await?;
            self.authorization_revision = revision;
        }
        Ok(())
    }

    async fn revalidate_authority(&mut self) -> AppResult<()> {
        self.ensure_environment_active().await?;
        let PresentedCredential::Bearer(token) = &self.authority.credential;
        let current = self
            .state
            .identity
            .authenticate(AuthenticationRequest::Bearer {
                token,
                organization_id: &self.authority.organization_id,
            })
            .await?;
        if current.actor != self.authority.actor
            || current.principal_id != self.authority.principal_id
            || self.actors.keys().any(|id| !current.may_represent(id))
        {
            return Err(AppError::Unauthorized);
        }
        self.authority = current;
        Ok(())
    }

    async fn handle_socket_message(
        &mut self,
        socket: &mut WebSocket,
        message: SocketMessage,
    ) -> AppResult<Option<SocketExit>> {
        match message {
            SocketMessage::Text(text) => {
                if text.len() > self.state.settings.server.max_body_bytes {
                    return Ok(Some(SocketExit::server(1009, "frame-too-large")));
                }
                let Ok(frame) = serde_json::from_str::<ClientFrame>(text.as_str()) else {
                    send_server_frame(
                        socket,
                        &ServerFrame::recoverable_error(
                            "invalid_frame",
                            "client frame is not valid protocol JSON",
                        ),
                    )
                    .await?;
                    return Ok(None);
                };
                // The parsed command owns its content; release the raw frame
                // before database work and response serialization duplicate it.
                drop(text);
                match self.handle_client_frame(frame).await {
                    Ok(ClientOutcome::None) => {}
                    Ok(ClientOutcome::Respond(frame)) => send_server_frame(socket, &frame).await?,
                    Ok(ClientOutcome::Replay(actor_id)) => {
                        self.replay_actor(socket, &actor_id).await?;
                    }
                    Err(error) => send_error(socket, &error).await?,
                }
                Ok(None)
            }
            SocketMessage::Binary(_) => {
                send_server_frame(
                    socket,
                    &ServerFrame::recoverable_error(
                        "unsupported_frame",
                        "binary application frames are not supported",
                    ),
                )
                .await?;
                Ok(None)
            }
            SocketMessage::Close(frame) => {
                let exit = frame.map_or_else(
                    || SocketExit::peer(1000, "client-close"),
                    |frame| {
                        SocketExit::peer(
                            normalize_close_code(frame.code),
                            safe_close_reason(frame.reason.as_str()),
                        )
                    },
                );
                Ok(Some(exit))
            }
            SocketMessage::Ping(_) | SocketMessage::Pong(_) => Ok(None),
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one exhaustive protocol dispatcher keeps every client frame variant visibly fail-closed"
    )]
    async fn handle_client_frame(&mut self, frame: ClientFrame) -> AppResult<ClientOutcome> {
        match frame {
            ClientFrame::Pong { ping_id } => {
                if self.pending_ping_id.as_deref() != Some(ping_id.as_str()) {
                    return Err(AppError::validation(
                        "pong does not match the latest server ping",
                    ));
                }
                self.pending_ping_id = None;
                self.last_valid_pong = Instant::now();
                self.state
                    .store
                    .refresh_realtime_session(
                        self.session_id,
                        lease_deadline(self.state.settings.realtime.heartbeat_timeout),
                        true,
                    )
                    .await?;
                Ok(ClientOutcome::None)
            }
            ClientFrame::Ack {
                actor_id,
                through_sequence,
            } => {
                let actor = self.actor(&actor_id)?.clone();
                let sent = self.sent_through.get(&actor_id).copied().unwrap_or(0);
                if through_sequence > sent {
                    return Err(AppError::validation(
                        "cannot ACK a sequence not emitted on this connection",
                    ));
                }
                self.state
                    .store
                    .acknowledge_deliveries(
                        &self.authority.organization_id,
                        &actor,
                        &self.consumer_id,
                        through_sequence,
                    )
                    .await?;
                Ok(ClientOutcome::None)
            }
            ClientFrame::Resume {
                actor_id,
                after_sequence,
            } => {
                if after_sequence < 0 {
                    return Err(AppError::validation("resume sequence must be non-negative"));
                }
                let after_sequence = if self.reset_cursors {
                    0
                } else {
                    after_sequence
                };
                let actor = self.actor(&actor_id)?;
                let high = self
                    .state
                    .store
                    .delivery_high_watermark(&self.authority.organization_id, actor)
                    .await?;
                if after_sequence > high {
                    return Err(AppError::validation(
                        "resume sequence exceeds the actor stream high watermark",
                    ));
                }
                self.sent_through.insert(actor_id.clone(), after_sequence);
                Ok(ClientOutcome::Replay(actor_id))
            }
            ClientFrame::Presence { actor_id, activity } => {
                self.actor(&actor_id)?;
                let expiry =
                    activity.map(|_| lease_deadline(self.state.settings.realtime.activity_ttl));
                self.state
                    .store
                    .update_realtime_activity(UpdateRealtimeActivityCommand {
                        session_id: self.session_id,
                        organization_id: self.authority.organization_id.clone(),
                        actor_id,
                        activity,
                        activity_expires_at: expiry,
                    })
                    .await?;
                Ok(ClientOutcome::None)
            }
            ClientFrame::Receipt {
                actor_id,
                conversation_id,
                message_id,
                status,
                device_id,
            } => {
                if device_id != self.consumer_id {
                    return Err(AppError::Forbidden);
                }
                let recipient = self.actor(&actor_id)?.clone();
                self.state
                    .store
                    .record_receipt_without_payload(RecordReceiptCommand {
                        organization_id: self.authority.organization_id.clone(),
                        conversation_id,
                        message_id,
                        recipient,
                        device_id,
                        status,
                    })
                    .await?;
                Ok(ClientOutcome::Respond(Box::new(
                    ServerFrame::ReceiptRecorded { message_id, status },
                )))
            }
            ClientFrame::SendMessage {
                actor_id,
                org_id,
                conversation_id,
                idempotency_key,
                message,
            } => {
                let mut message = *message;
                if org_id != self.authority.organization_id {
                    return Err(AppError::Forbidden);
                }
                let sender = self.actor(&actor_id)?.clone();
                if message
                    .sender_id
                    .as_ref()
                    .is_some_and(|requested| !requested.addresses(&sender))
                {
                    return Err(AppError::Forbidden);
                }
                if message.sender_id.is_none() {
                    message.sender_id = Some(actor_id);
                }
                self.state
                    .store
                    .require_participant(&org_id, &sender, conversation_id)
                    .await?;
                let message = prepare_message_content(&self.state, message)?;
                let accepted = self
                    .state
                    .store
                    .send_message(SendMessageCommand {
                        organization_id: org_id,
                        conversation_id,
                        sender,
                        content: message,
                        idempotency_key: idempotency_key.clone(),
                    })
                    .await?;
                Ok(ClientOutcome::Respond(Box::new(
                    ServerFrame::MessageAccepted {
                        idempotency_key,
                        message: Box::new(accepted),
                    },
                )))
            }
        }
    }

    async fn handle_local_delivery(
        &mut self,
        socket: &mut WebSocket,
        wakeup: &DeliveryWakeup,
    ) -> AppResult<()> {
        let sent = self
            .sent_through
            .get(&wakeup.actor_id)
            .copied()
            .unwrap_or(0);
        if wakeup.sequence <= sent {
            return Ok(());
        }
        self.replay_actor(socket, &wakeup.actor_id).await
    }

    async fn replay_all(&mut self, socket: &mut WebSocket) -> AppResult<()> {
        let actor_ids = self.actors.keys().cloned().collect::<Vec<_>>();
        if actor_ids.is_empty() {
            return Ok(());
        }
        let started = Instant::now();
        let start = self.next_replay_actor % actor_ids.len();
        for offset in 0..actor_ids.len() {
            let index = (start + offset) % actor_ids.len();
            self.replay_actor(socket, &actor_ids[index]).await?;
            self.next_replay_actor = (index + 1) % actor_ids.len();
            if started.elapsed() >= REPLAY_TIME_BUDGET {
                break;
            }
        }
        Ok(())
    }

    async fn replay_actor(&mut self, socket: &mut WebSocket, actor_id: &ActorId) -> AppResult<()> {
        let actor = self.actor(actor_id)?.clone();
        let started = Instant::now();
        // Read one payload at a time. A count-bounded batch of 100 allowed
        // messages could otherwise hold 10 GB before the first socket write.
        for _ in 0..REPLAY_BATCH_SIZE {
            let after = self.sent_through.get(actor_id).copied().unwrap_or(0);
            let deliveries = self
                .state
                .store
                .replay_deliveries(&self.authority.organization_id, &actor, after, 1)
                .await?;
            let Some(delivery) = deliveries.into_iter().next() else {
                break;
            };
            if delivery.organization_id != self.authority.organization_id
                || delivery.target != actor
                || delivery.sequence <= after
            {
                return Err(AppError::internal(anyhow::anyhow!(
                    "actor delivery replay is not correctly scoped or ordered"
                )));
            }
            let sequence = delivery.sequence;
            let frame =
                ServerFrame::delivery(delivery.id, actor_id.clone(), sequence, delivery.payload);
            send_server_frame(socket, &frame).await?;
            self.sent_through.insert(actor_id.clone(), sequence);
            // Return to the socket loop for ACKs, heartbeats and revocation.
            // A single large write may exceed this budget; never start a second.
            if started.elapsed() >= REPLAY_TIME_BUDGET {
                break;
            }
        }
        Ok(())
    }

    fn actor(&self, actor_id: &ActorId) -> AppResult<&ActorRef> {
        self.actors.get(actor_id).ok_or(AppError::Forbidden)
    }
}

enum SessionEvent {
    Incoming(Option<Result<SocketMessage, axum::Error>>),
    LocalDelivery(Option<Arc<DeliveryWakeup>>),
    Heartbeat,
    Replay,
    AuthorizationChanged,
    Disconnect,
}

fn authority_exit(error: &AppError) -> SocketExit {
    if matches!(
        error,
        AppError::DependencyUnavailable { .. } | AppError::Database(_) | AppError::RateLimited
    ) {
        SocketExit::server(1013, "authorization-unavailable")
    } else {
        SocketExit::server(4001, "authorization-revoked")
    }
}

async fn environment_fence(
    state: &AppState,
) -> AppResult<Option<sqlx::Transaction<'static, sqlx::Postgres>>> {
    match (
        &state.testing,
        state.testing_environment,
        state.testing_generation,
    ) {
        (Some(registry), Some(id), Some(generation)) => {
            registry.request_fence(id, generation).await.map(Some)
        }
        _ => Ok(None),
    }
}

enum ClientOutcome {
    None,
    Respond(Box<ServerFrame>),
    Replay(ActorId),
}

struct SocketExit {
    code: u16,
    reason: String,
    send_close: bool,
}

impl SocketExit {
    fn server(code: u16, reason: impl Into<String>) -> Self {
        Self {
            code,
            reason: reason.into(),
            send_close: true,
        }
    }

    fn peer(code: u16, reason: impl Into<String>) -> Self {
        Self {
            code,
            reason: reason.into(),
            send_close: false,
        }
    }
}

async fn send_error(socket: &mut WebSocket, error: &AppError) -> AppResult<()> {
    send_server_frame(
        socket,
        &ServerFrame::recoverable_error(error.code(), error.to_string()),
    )
    .await
}

async fn send_server_frame(socket: &mut WebSocket, frame: &ServerFrame) -> AppResult<()> {
    let encoded = serde_json::to_string(frame).map_err(AppError::internal)?;
    socket
        .send(SocketMessage::Text(encoded.into()))
        .await
        .map_err(|error| AppError::internal(anyhow::Error::new(error)))
}

async fn close_socket(socket: &mut WebSocket, code: u16, reason: &str) -> AppResult<()> {
    socket
        .send(SocketMessage::Close(Some(CloseFrame {
            code,
            reason: reason.to_owned().into(),
        })))
        .await
        .map_err(|error| AppError::internal(anyhow::Error::new(error)))
}

fn lease_deadline(duration: Duration) -> OffsetDateTime {
    let now = OffsetDateTime::now_utc();
    let duration = match time::Duration::try_from(duration) {
        Ok(duration) => duration,
        Err(_) => time::Duration::days(1),
    };
    match now.checked_add(duration) {
        Some(deadline) => deadline,
        None => now + time::Duration::days(1),
    }
}

const fn normalize_close_code(code: u16) -> u16 {
    if code >= 1000 && code <= 4999 {
        code
    } else {
        1002
    }
}

fn safe_close_reason(reason: &str) -> String {
    if reason.is_empty() {
        return "client-close".to_owned();
    }
    let mut output = String::new();
    for character in reason.chars() {
        if character.is_control() || output.len() + character.len_utf8() > 123 {
            break;
        }
        output.push(character);
    }
    if output.is_empty() {
        "client-close".to_owned()
    } else {
        output
    }
}

#[cfg(test)]
mod tests {
    use super::{normalize_close_code, safe_close_reason};

    #[test]
    fn persisted_close_metadata_is_bounded() {
        assert_eq!(normalize_close_code(999), 1002);
        assert_eq!(normalize_close_code(4000), 4000);
        assert_eq!(safe_close_reason("normal"), "normal");
        assert_eq!(safe_close_reason("bad\nreason"), "bad");
        assert!(safe_close_reason(&"x".repeat(200)).len() <= 123);
    }
}
