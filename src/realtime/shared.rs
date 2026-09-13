//! A single prewarmed socket carries independently authenticated profile streams.
use super::{session::serve_transport, transport::Transport};
use crate::{
    AppError, AppResult,
    application::{ports::AuthenticationRequest, state::AppState},
    domain::OrganizationId,
};
use axum::{
    extract::{
        State, WebSocketUpgrade,
        ws::{CloseFrame, Message, WebSocket},
    },
    response::Response,
};
use futures::StreamExt as _;
use secrecy::SecretString;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::HashMap, time::Duration};
use tokio::sync::mpsc;

#[derive(Deserialize)]
#[serde(
    tag = "type",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum Input {
    Subscribe {
        subscription_id: String,
        token: SecretString,
        organization_id: OrganizationId,
        actor_id: String,
        device_id: String,
        testing_key: Option<SecretString>,
        testing_generation: Option<i64>,
        #[serde(default)]
        telemetry_enabled: Option<bool>,
    },
    Channel {
        subscription_id: String,
        frame: Value,
    },
    Unsubscribe {
        subscription_id: String,
    },
    Pong {
        ping_id: String,
    },
}
struct Channel {
    tx: mpsc::Sender<Message>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Channel {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub(crate) async fn open(
    State(mut state): State<AppState>,
    headers: axum::http::HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    if headers.get("x-dm-telemetry").is_some_and(|v| v == "off") {
        std::sync::Arc::make_mut(&mut state.settings)
            .telemetry
            .enabled = false;
    }
    upgrade
        .max_message_size(state.settings.server.max_body_bytes)
        .max_frame_size(state.settings.server.max_body_bytes)
        .on_upgrade(move |socket| run(socket, state))
}
async fn authorize(
    state: &AppState,
    input: Input,
    output: mpsc::Sender<(String, Message)>,
) -> AppResult<(String, Channel)> {
    use secrecy::ExposeSecret as _;
    let Input::Subscribe {
        subscription_id,
        token,
        organization_id,
        actor_id,
        device_id,
        testing_key,
        testing_generation,
        telemetry_enabled,
    } = input
    else {
        return Err(AppError::Unauthorized);
    };
    if subscription_id.is_empty() || subscription_id.len() > 128 {
        return Err(AppError::validation("invalid subscription ID"));
    }
    let mut selected = if let Some(secret) = testing_key {
        state
            .testing
            .as_ref()
            .ok_or(AppError::Unauthorized)?
            .state_for_key(state, secret.expose_secret())
            .await?
    } else {
        state.clone()
    };
    if telemetry_enabled == Some(false) {
        std::sync::Arc::make_mut(&mut selected.settings)
            .telemetry
            .enabled = false;
    }
    if testing_generation.is_some() && selected.testing_environment.is_none() {
        return Err(AppError::Unauthorized);
    }
    let authority = selected
        .identity
        .authenticate(AuthenticationRequest::Bearer {
            token: &token,
            organization_id: &organization_id,
        })
        .await?;
    let actor = actor_id
        .parse()
        .map_err(|_| AppError::validation("invalid actor ID"))?;
    if authority.organization_id != organization_id || !authority.may_represent(&actor) {
        return Err(AppError::Forbidden);
    }
    crate::application::messaging::validate_device_id(&device_id)?;
    let actor_ref = authority.actor.clone();
    if actor_ref.id != actor {
        return Err(AppError::Forbidden);
    }
    let (tx, rx) = mpsc::channel(64);
    let transport = Transport::Shared {
        incoming: rx,
        outgoing: output,
        id: subscription_id.clone(),
    };
    let task = tokio::spawn(serve_transport(
        transport,
        selected,
        authority,
        vec![actor_ref],
        device_id,
        testing_generation,
    ));
    Ok((subscription_id, Channel { tx, task }))
}
async fn run(mut socket: WebSocket, state: AppState) {
    let mut channels: HashMap<String, Channel> = HashMap::new();
    let (tx, mut rx) = mpsc::channel::<(String, Message)>(64);
    let mut timer = tokio::time::interval(Duration::from_secs(30));
    let mut last_pong = tokio::time::Instant::now();
    let mut ping = String::new();
    if socket
        .send(Message::Text(
            json!({"type":"prewarmed","data":{"protocol_version":1,"max_subscriptions":64}})
                .to_string()
                .into(),
        ))
        .await
        .is_err()
    {
        return;
    }
    loop {
        let result: Result<(), axum::Error> = tokio::select! {
            _ = timer.tick() => {
                if last_pong.elapsed() >= Duration::from_secs(120) {
                    let _ = socket.send(Message::Close(Some(CloseFrame {code:4000,reason:"heartbeat-timeout".into()}))).await;
                    break;
                }
                ping = uuid::Uuid::new_v4().to_string();
                socket.send(Message::Text(json!({"type":"ping","data":{"ping_id":ping}}).to_string().into())).await
            },
            Some((id, message)) = rx.recv() => {
                let frame = match message {
                    Message::Text(text) => serde_json::from_str::<Value>(&text).unwrap_or(Value::Null),
                    Message::Close(_) => { channels.remove(&id); json!({"type":"error","data":{"code":"subscription_closed","recoverable":false}}) },
                    _ => continue,
                };
                socket.send(Message::Text(json!({"type":"channel","data":{"subscription_id":id,"frame":frame}}).to_string().into())).await
            },
            incoming = socket.next() => {
                let Some(Ok(message)) = incoming else { break; };
                match message {
                    Message::Close(_) => break,
                    Message::Ping(bytes) => socket.send(Message::Pong(bytes)).await,
                    Message::Text(text) => {
                        let Ok(input) = serde_json::from_str::<Input>(&text) else { break };
                        match input {
                            Input::Subscribe {ref subscription_id, ..} => {
                                let id = subscription_id.clone();
                                if channels.contains_key(&id) || channels.len() >= 64 { break; }
                                match tokio::time::timeout(Duration::from_secs(30), authorize(&state, input, tx.clone())).await {
                                    Ok(Ok((id, channel))) => { channels.insert(id,channel); Ok(()) },
                                    _ => socket.send(Message::Text(json!({"type":"channel","data":{"subscription_id":id,"frame":{"type":"error","data":{"code":"subscription_rejected","message":"Refresh the profile or select a valid sandbox.","recoverable":false}}}}).to_string().into())).await,
                                }
                            },
                            Input::Channel {subscription_id, frame} => {
                                if let Some(channel) = channels.get(&subscription_id) {
                                    if channel.tx.try_send(Message::Text(frame.to_string().into())).is_err() { break; }
                                } else { break; }
                                Ok(())
                            },
                            Input::Unsubscribe {subscription_id} => { channels.remove(&subscription_id); Ok(()) },
                            Input::Pong {ping_id} => { if !ping.is_empty() && ping_id == ping { last_pong=tokio::time::Instant::now(); } Ok(()) },
                        }
                    },
                    _ => Ok(()),
                }
            }
        };
        if result.is_err() {
            break;
        }
    }
}
