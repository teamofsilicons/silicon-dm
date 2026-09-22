//! Internal transport retained for the legacy session implementation.
use axum::extract::ws::{Message, WebSocket};
use futures::StreamExt as _;

pub(super) enum Transport {
    Direct(Box<WebSocket>),
}
impl Transport {
    pub async fn next(&mut self) -> Option<Result<Message, axum::Error>> {
        match self {
            Self::Direct(socket) => socket.next().await,
        }
    }
    pub async fn send(&mut self, message: Message) -> Result<(), axum::Error> {
        match self {
            Self::Direct(socket) => socket.send(message).await,
        }
    }
}
