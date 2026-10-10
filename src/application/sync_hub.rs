//! Wakes long-polling `/api/v1/sync` requests. One Postgres `LISTEN dm_delivery` per
//! process fans notifications out to waiting requests, so a waiting request holds no
//! database connection.

use std::{sync::Arc, time::Duration};

use tokio::{
    sync::broadcast::{self, error::RecvError},
    time::Instant,
};
use tokio_util::sync::CancellationToken;

use crate::infrastructure::postgres::PostgresStore;

/// Fan-out of "something new for this account" signals.
#[derive(Clone)]
pub struct SyncHub {
    sender: broadcast::Sender<Arc<str>>,
}

impl Default for SyncHub {
    fn default() -> Self {
        let (sender, _) = broadcast::channel(4096);
        Self { sender }
    }
}

impl SyncHub {
    /// Starts listening (call before reading, so nothing committed in between is missed).
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<str>> {
        self.sender.subscribe()
    }

    /// Signals waiters of `account`.
    pub fn publish(&self, account: &str) {
        let _ = self.sender.send(Arc::from(account));
    }

    /// Waits until `account` is signalled or `deadline` passes; `true` when woken (or when
    /// signals were dropped, so the caller re-reads rather than miss one).
    pub async fn wait_for(
        &self,
        mut receiver: broadcast::Receiver<Arc<str>>,
        account: &str,
        deadline: Instant,
    ) -> bool {
        loop {
            match tokio::time::timeout_at(deadline, receiver.recv()).await {
                Err(_) | Ok(Err(RecvError::Closed)) => return false,
                Ok(Ok(signalled)) if &*signalled == account => return true,
                Ok(Ok(_)) => {}
                Ok(Err(RecvError::Lagged(_))) => return true,
            }
        }
    }

    /// Feeds the hub from Postgres until cancelled, reconnecting after failures.
    pub async fn run(self, store: PostgresStore, cancellation: CancellationToken) {
        loop {
            let listener = tokio::select! {
                () = cancellation.cancelled() => return,
                listener = store.subscribe_delivery_wakeups() => listener,
            };
            match listener {
                Ok(Some(mut listener)) => loop {
                    tokio::select! {
                        () = cancellation.cancelled() => return,
                        notification = listener.recv() => match notification {
                            Ok(notification) => {
                                if let Some(account) = serde_json::from_str::<serde_json::Value>(notification.payload())
                                    .ok()
                                    .and_then(|payload| payload.get("account").and_then(serde_json::Value::as_str).map(str::to_owned))
                                {
                                    self.publish(&account);
                                }
                            }
                            Err(error) => {
                                tracing::warn!(error = %error, "sync wakeup listener disconnected; reconnecting");
                                break;
                            }
                        }
                    }
                },
                Ok(None) => {
                    tracing::info!(
                        "the database pool is too small for a sync wakeup listener; long-polls time out instead"
                    );
                    cancellation.cancelled().await;
                    return;
                }
                Err(error) => {
                    tracing::warn!(
                        code = error.code(),
                        "sync wakeup listener unavailable; retrying"
                    );
                }
            }
            tokio::select! {
                () = cancellation.cancelled() => return,
                () = tokio::time::sleep(Duration::from_secs(5)) => {}
            }
        }
    }
}
