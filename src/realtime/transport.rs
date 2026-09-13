//! One transport interface for standalone and multiplexed authenticated streams.
use axum::extract::ws::{Message, WebSocket};
use futures::StreamExt as _;
use tokio::sync::mpsc;

pub(super) enum Transport {
    Direct(Box<WebSocket>),
    Shared {
        incoming: mpsc::Receiver<Message>,
        outgoing: mpsc::Sender<(String, Message)>,
        id: String,
    },
}
impl Transport {
    pub async fn next(&mut self) -> Option<Result<Message, axum::Error>> {
        match self {
            Self::Direct(socket) => socket.next().await,
            Self::Shared { incoming, .. } => incoming.recv().await.map(Ok),
        }
    }
    pub async fn send(&mut self, message: Message) -> Result<(), axum::Error> {
        match self {
            Self::Direct(socket) => socket.send(message).await,
            Self::Shared { outgoing, id, .. } => outgoing
                .send((id.clone(), message))
                .await
                .map_err(|_| axum::Error::new(std::io::Error::other("shared connection closed"))),
        }
    }
}
