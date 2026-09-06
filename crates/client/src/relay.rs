//! Typed commands for the local CLI relay. This module does not persist state.
use crate::{Client, GifList, Result, models::*};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

/// Public DM operations accepted by the local daemon, without auth secrets.
/// Mutations include their retry key in the command so crash recovery preserves it.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum Operation {
    Me,
    ListConversations {
        #[serde(default)]
        page: PageRequest,
    },
    CreateConversation {
        participant_ids: Vec<String>,
        idempotency_key: String,
    },
    ListMessages {
        conversation_id: Uuid,
        #[serde(default)]
        page: PageRequest,
        #[serde(default)]
        include_bundled_members: bool,
    },
    GetMessage {
        conversation_id: Uuid,
        message_id: Uuid,
    },
    SendMessage {
        conversation_id: Uuid,
        message: MessageCreate,
        idempotency_key: String,
    },
    EditMessage {
        conversation_id: Uuid,
        message_id: Uuid,
        message: MessageCreate,
        version: i64,
        idempotency_key: String,
    },
    DeleteMessage {
        conversation_id: Uuid,
        message_id: Uuid,
        version: i64,
        idempotency_key: String,
    },
    Receipt {
        conversation_id: Uuid,
        message_id: Uuid,
        status: ReceiptStatus,
        device_id: String,
    },
    GetDraft {
        conversation_id: Uuid,
    },
    PutDraft {
        conversation_id: Uuid,
        draft: DraftInput,
        version: i64,
    },
    DeleteDraft {
        conversation_id: Uuid,
    },
    CreateBundle {
        conversation_id: Uuid,
        bundle: BundleCreate,
        idempotency_key: String,
    },
    GetBundle {
        conversation_id: Uuid,
        bundle_id: Uuid,
    },
    GetPresence {
        actor_id: String,
    },
    /// Transient activity can only be sent by an active daemon WebSocket.
    SetPresence {
        activity: Option<Activity>,
    },
    TrendingGifs,
    SearchGifs {
        query: String,
    },
    RecentGifs,
}
impl Operation {
    /// Whether execution changes backend state (presence is transient).
    pub fn is_mutation(&self) -> bool {
        matches!(
            self,
            Self::CreateConversation { .. }
                | Self::SendMessage { .. }
                | Self::EditMessage { .. }
                | Self::DeleteMessage { .. }
                | Self::Receipt { .. }
                | Self::PutDraft { .. }
                | Self::DeleteDraft { .. }
                | Self::CreateBundle { .. }
                | Self::SetPresence { .. }
        )
    }
    /// Calls only the public DM client. Presence writes require a live socket.
    pub async fn execute(&self, client: &Client) -> Result<Value> {
        Ok(match self {
            Self::Me => serde_json::to_value(client.me().await?)?,
            Self::ListConversations { page } => {
                serde_json::to_value(client.conversations(page).await?)?
            }
            Self::CreateConversation {
                participant_ids,
                idempotency_key,
            } => serde_json::to_value(
                client
                    .create_conversation(participant_ids, idempotency_key)
                    .await?,
            )?,
            Self::ListMessages {
                conversation_id,
                page,
                include_bundled_members,
            } => serde_json::to_value(
                client
                    .messages(*conversation_id, page, *include_bundled_members)
                    .await?,
            )?,
            Self::GetMessage {
                conversation_id,
                message_id,
            } => serde_json::to_value(client.message(*conversation_id, *message_id).await?)?,
            Self::SendMessage {
                conversation_id,
                message,
                idempotency_key,
            } => serde_json::to_value(
                client
                    .send_message(*conversation_id, message, idempotency_key)
                    .await?,
            )?,
            Self::EditMessage {
                conversation_id,
                message_id,
                message,
                version,
                idempotency_key,
            } => serde_json::to_value(
                client
                    .edit_message(
                        *conversation_id,
                        *message_id,
                        message,
                        *version,
                        idempotency_key,
                    )
                    .await?,
            )?,
            Self::DeleteMessage {
                conversation_id,
                message_id,
                version,
                idempotency_key,
            } => serde_json::to_value(
                client
                    .delete_message(*conversation_id, *message_id, *version, idempotency_key)
                    .await?,
            )?,
            Self::Receipt {
                conversation_id,
                message_id,
                status,
                device_id,
            } => serde_json::to_value(
                client
                    .record_receipt(*conversation_id, *message_id, *status, device_id)
                    .await?,
            )?,
            Self::GetDraft { conversation_id } => {
                serde_json::to_value(client.draft(*conversation_id).await?)?
            }
            Self::PutDraft {
                conversation_id,
                draft,
                version,
            } => serde_json::to_value(client.put_draft(*conversation_id, draft, *version).await?)?,
            Self::DeleteDraft { conversation_id } => {
                client.delete_draft(*conversation_id).await?;
                json!({"deleted":true})
            }
            Self::CreateBundle {
                conversation_id,
                bundle,
                idempotency_key,
            } => serde_json::to_value(
                client
                    .create_bundle(*conversation_id, bundle, idempotency_key)
                    .await?,
            )?,
            Self::GetBundle {
                conversation_id,
                bundle_id,
            } => serde_json::to_value(client.bundle(*conversation_id, *bundle_id).await?)?,
            Self::GetPresence { actor_id } => {
                serde_json::to_value(client.presence(actor_id).await?)?
            }
            Self::SetPresence { .. } => {
                return Err(crate::Error::Configuration(
                    "presence changes require the local relay daemon".into(),
                ));
            }
            Self::TrendingGifs => serde_json::to_value(client.gifs(GifList::Trending).await?)?,
            Self::SearchGifs { query } => {
                serde_json::to_value(client.gifs(GifList::Search(query)).await?)?
            }
            Self::RecentGifs => serde_json::to_value(client.gifs(GifList::Recent).await?)?,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RelayRequest {
    pub request_id: Uuid,
    /// Local profile name, with optional testing environment UUID.
    pub profile: String,
    #[serde(default)]
    pub testing_environment_id: Option<Uuid>,
    /// Expected sandbox state; the daemon captures its known generation when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub testing_generation: Option<i64>,
    pub request: Operation,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RelayAcknowledgement {
    pub acknowledged: bool,
    pub request_id: Uuid,
    /// Exact original JSON supplied to the local relay.
    pub request: Value,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RelayResult {
    pub request_id: Uuid,
    pub state: String,
    #[serde(default)]
    pub result: Option<Value>,
    #[serde(default)]
    pub error: Option<Value>,
    pub request: Value,
}
/// Lightweight progress information; the exact request and response remain in
/// [`RelayResult`] and need only be fetched after waiting finishes.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RelayRequestStatus {
    pub request_id: Uuid,
    pub state: String,
}
/// Loopback-only HTTP relay client; bearer is a local credential, not an IAM token.
#[derive(Clone)]
pub struct RelayClient {
    http: reqwest::Client,
    base: url::Url,
    token: String,
}
impl RelayClient {
    pub fn new(base: &str, token: impl Into<String>) -> Result<Self> {
        let base = url::Url::parse(base).map_err(|e| crate::Error::Configuration(e.to_string()))?;
        crate::validate_endpoint(&base)?;
        if !matches!(
            base.host_str(),
            Some("127.0.0.1" | "localhost" | "dm.localhost" | "[::1]" | "::1")
        ) {
            return Err(crate::Error::Configuration(
                "relay must use a loopback URL".into(),
            ));
        }
        Ok(Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .no_proxy()
                .build()?,
            base,
            token: token.into(),
        })
    }
    pub async fn submit(&self, request: &RelayRequest) -> Result<RelayAcknowledgement> {
        self.submit_json(request).await
    }
    /// Preserves unknown JSON properties when acknowledging the caller's exact request.
    pub async fn submit_value(&self, request: &Value) -> Result<RelayAcknowledgement> {
        self.submit_json(request).await
    }
    async fn submit_json<T: Serialize + ?Sized>(
        &self,
        request: &T,
    ) -> Result<RelayAcknowledgement> {
        let url = self
            .base
            .join("requests")
            .map_err(|e| crate::Error::Configuration(e.to_string()))?;
        Ok(crate::checked(
            self.http
                .post(url)
                .bearer_auth(&self.token)
                .json(request)
                .send()
                .await?,
        )
        .await?
        .json()
        .await?)
    }
    pub async fn result(&self, id: Uuid) -> Result<RelayResult> {
        let url = self
            .base
            .join(&format!("requests/{id}"))
            .map_err(|e| crate::Error::Configuration(e.to_string()))?;
        Ok(
            crate::checked(self.http.get(url).bearer_auth(&self.token).send().await?)
                .await?
                .json()
                .await?,
        )
    }
    /// Reads progress without transferring the queued request or result body.
    /// Daemons released before this route existed return HTTP 404; callers can
    /// fall back to [`Self::result`] for those installations.
    pub async fn request_status(&self, id: Uuid) -> Result<RelayRequestStatus> {
        let url = self
            .base
            .join(&format!("requests/{id}/status"))
            .map_err(|e| crate::Error::Configuration(e.to_string()))?;
        Ok(
            crate::checked(self.http.get(url).bearer_auth(&self.token).send().await?)
                .await?
                .json()
                .await?,
        )
    }
    pub async fn status(&self) -> Result<Value> {
        let url = self
            .base
            .join("status")
            .map_err(|e| crate::Error::Configuration(e.to_string()))?;
        Ok(
            crate::checked(self.http.get(url).bearer_auth(&self.token).send().await?)
                .await?
                .json()
                .await?,
        )
    }
    pub async fn stop(&self) -> Result<()> {
        let url = self
            .base
            .join("shutdown")
            .map_err(|e| crate::Error::Configuration(e.to_string()))?;
        crate::checked(self.http.post(url).bearer_auth(&self.token).send().await?).await?;
        Ok(())
    }
}
