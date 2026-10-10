//! Typed commands for the local CLI relay. This module does not persist state.
use crate::{Client, GifList, Result, models::*};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

/// Public DM operations accepted by the local daemon, without auth secrets.
/// Mutations include their retry key in the command so crash recovery preserves it.
/// `for` names one of the custodian's Silicons where the service allows it.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum Operation {
    Me,
    Delivery,
    ListGroups {
        #[serde(default)]
        page: PageRequest,
    },
    GetGroup {
        group_id: String,
    },
    CreateGroup {
        group: GroupCreate,
        idempotency_key: String,
    },
    UpdateGroup {
        group_id: String,
        settings: GroupSettings,
        version: i64,
        idempotency_key: String,
    },
    InviteGroupMembers {
        group_id: String,
        member_ids: Vec<String>,
        idempotency_key: String,
    },
    RemoveGroupMembers {
        group_id: String,
        member_ids: Vec<String>,
        idempotency_key: String,
    },
    AddGroupOwners {
        group_id: String,
        member_ids: Vec<String>,
        idempotency_key: String,
    },
    RemoveGroupOwners {
        group_id: String,
        member_ids: Vec<String>,
        idempotency_key: String,
    },
    ListInvitations {
        #[serde(default, rename = "for", skip_serializing_if = "Option::is_none")]
        for_account: Option<String>,
    },
    AcceptInvitation {
        group_id: String,
    },
    DeclineInvitation {
        group_id: String,
        #[serde(default, rename = "for", skip_serializing_if = "Option::is_none")]
        for_account: Option<String>,
    },
    LeaveGroup {
        group_id: String,
    },
    GetAllowList {
        #[serde(default, rename = "for", skip_serializing_if = "Option::is_none")]
        for_account: Option<String>,
    },
    Allow {
        account_ids: Vec<String>,
        #[serde(default, rename = "for", skip_serializing_if = "Option::is_none")]
        for_account: Option<String>,
    },
    Disallow {
        account_ids: Vec<String>,
        #[serde(default, rename = "for", skip_serializing_if = "Option::is_none")]
        for_account: Option<String>,
    },
    ListConversations {
        #[serde(default)]
        page: PageRequest,
        #[serde(default, rename = "for", skip_serializing_if = "Option::is_none")]
        for_account: Option<String>,
    },
    CreateConversation {
        participant_ids: Vec<String>,
        idempotency_key: String,
    },
    ListMessages {
        conversation_id: String,
        #[serde(default)]
        page: PageRequest,
        #[serde(default)]
        include_bundled_members: bool,
    },
    GetMessage {
        conversation_id: String,
        message_id: String,
    },
    SendMessage {
        conversation_id: String,
        message: MessageCreate,
        idempotency_key: String,
    },
    EditMessage {
        conversation_id: String,
        message_id: String,
        message: MessageCreate,
        idempotency_key: String,
    },
    DeleteMessage {
        conversation_id: String,
        message_id: String,
        idempotency_key: String,
    },
    Receipt {
        conversation_id: String,
        message_id: String,
        status: ReceiptStatus,
        device_id: String,
    },
    GetDraft {
        conversation_id: String,
    },
    PutDraft {
        conversation_id: String,
        draft: DraftInput,
        version: i64,
    },
    DeleteDraft {
        conversation_id: String,
    },
    CreateBundle {
        conversation_id: String,
        bundle: BundleCreate,
        idempotency_key: String,
    },
    GetBundle {
        conversation_id: String,
        bundle_id: String,
    },
    GetPresence {
        actor_id: String,
    },
    /// Transient activity renews the local daemon's account-owned HTTP device lease.
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
            Self::CreateGroup { .. }
                | Self::UpdateGroup { .. }
                | Self::InviteGroupMembers { .. }
                | Self::RemoveGroupMembers { .. }
                | Self::AddGroupOwners { .. }
                | Self::RemoveGroupOwners { .. }
                | Self::AcceptInvitation { .. }
                | Self::DeclineInvitation { .. }
                | Self::LeaveGroup { .. }
                | Self::Allow { .. }
                | Self::Disallow { .. }
                | Self::CreateConversation { .. }
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
    /// Executes a command with the caller's stable HTTP presence device identity.
    pub async fn execute_with_device(&self, client: &Client, device_id: &str) -> Result<Value> {
        if let Self::SetPresence { activity } = self {
            return Ok(serde_json::to_value(
                client.renew_presence(device_id, *activity).await?,
            )?);
        }
        self.execute(client).await
    }
    /// Calls only the public DM client. Presence writes additionally need a device ID.
    pub async fn execute(&self, client: &Client) -> Result<Value> {
        Ok(match self {
            Self::ListGroups { page } => serde_json::to_value(client.groups(page).await?)?,
            Self::GetGroup { group_id } => serde_json::to_value(client.group(group_id).await?)?,
            Self::CreateGroup {
                group,
                idempotency_key,
            } => serde_json::to_value(client.create_group(group, idempotency_key).await?)?,
            Self::UpdateGroup {
                group_id,
                settings,
                version,
                idempotency_key,
            } => serde_json::to_value(
                client
                    .update_group(group_id, settings, *version, idempotency_key)
                    .await?,
            )?,
            Self::InviteGroupMembers {
                group_id,
                member_ids,
                idempotency_key,
            } => serde_json::to_value(
                client
                    .invite_group_members(group_id, member_ids, idempotency_key)
                    .await?,
            )?,
            Self::RemoveGroupMembers {
                group_id,
                member_ids,
                idempotency_key,
            } => serde_json::to_value(
                client
                    .remove_group_members(group_id, member_ids, idempotency_key)
                    .await?,
            )?,
            Self::AddGroupOwners {
                group_id,
                member_ids,
                idempotency_key,
            } => serde_json::to_value(
                client
                    .add_group_owners(group_id, member_ids, idempotency_key)
                    .await?,
            )?,
            Self::RemoveGroupOwners {
                group_id,
                member_ids,
                idempotency_key,
            } => serde_json::to_value(
                client
                    .remove_group_owners(group_id, member_ids, idempotency_key)
                    .await?,
            )?,
            Self::ListInvitations { for_account } => {
                serde_json::to_value(client.invitations(for_account.as_deref()).await?)?
            }
            Self::AcceptInvitation { group_id } => {
                serde_json::to_value(client.accept_invitation(group_id).await?)?
            }
            Self::DeclineInvitation {
                group_id,
                for_account,
            } => {
                client
                    .decline_invitation(group_id, for_account.as_deref())
                    .await?;
                json!({"declined":true,"group_id":group_id})
            }
            Self::LeaveGroup { group_id } => {
                client.leave_group(group_id).await?;
                json!({"left":true,"group_id":group_id})
            }
            Self::GetAllowList { for_account } => {
                serde_json::to_value(client.allow_list(for_account.as_deref()).await?)?
            }
            Self::Allow {
                account_ids,
                for_account,
            } => serde_json::to_value(client.allow(account_ids, for_account.as_deref()).await?)?,
            Self::Disallow {
                account_ids,
                for_account,
            } => serde_json::to_value(client.disallow(account_ids, for_account.as_deref()).await?)?,
            Self::Me => serde_json::to_value(client.me().await?)?,
            Self::Delivery => serde_json::to_value(client.delivery().await?)?,
            Self::ListConversations { page, for_account } => serde_json::to_value(
                client
                    .conversations_for(page, for_account.as_deref())
                    .await?,
            )?,
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
                    .messages(conversation_id, page, *include_bundled_members)
                    .await?,
            )?,
            Self::GetMessage {
                conversation_id,
                message_id,
            } => serde_json::to_value(client.message(conversation_id, message_id).await?)?,
            Self::SendMessage {
                conversation_id,
                message,
                idempotency_key,
            } => serde_json::to_value(
                client
                    .send_message(conversation_id, message, idempotency_key)
                    .await?,
            )?,
            Self::EditMessage {
                conversation_id,
                message_id,
                message,
                idempotency_key,
            } => serde_json::to_value(
                client
                    .edit_message(conversation_id, message_id, message, idempotency_key)
                    .await?,
            )?,
            Self::DeleteMessage {
                conversation_id,
                message_id,
                idempotency_key,
            } => serde_json::to_value(
                client
                    .delete_message(conversation_id, message_id, idempotency_key)
                    .await?,
            )?,
            Self::Receipt {
                conversation_id,
                message_id,
                status,
                device_id,
            } => serde_json::to_value(
                client
                    .record_receipt(conversation_id, message_id, *status, device_id)
                    .await?,
            )?,
            Self::GetDraft { conversation_id } => {
                serde_json::to_value(client.draft(conversation_id).await?)?
            }
            Self::PutDraft {
                conversation_id,
                draft,
                version,
            } => serde_json::to_value(client.put_draft(conversation_id, draft, *version).await?)?,
            Self::DeleteDraft { conversation_id } => {
                client.delete_draft(conversation_id).await?;
                json!({"deleted":true})
            }
            Self::CreateBundle {
                conversation_id,
                bundle,
                idempotency_key,
            } => serde_json::to_value(
                client
                    .create_bundle(conversation_id, bundle, idempotency_key)
                    .await?,
            )?,
            Self::GetBundle {
                conversation_id,
                bundle_id,
            } => serde_json::to_value(client.bundle(conversation_id, bundle_id).await?)?,
            Self::GetPresence { actor_id } => {
                serde_json::to_value(client.presence(actor_id).await?)?
            }
            Self::SetPresence { .. } => {
                return Err(crate::Error::Configuration(
                    "presence changes require execute_with_device() or Client::renew_presence() with a stable device ID".into(),
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
    /// Local profile name (see `dm profiles list`).
    pub profile: String,
    /// Retired with test environments. Requests that name one are refused instead of
    /// running against the signed-in account; requests queued by older releases carry null.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub testing_environment_id: Option<Uuid>,
    /// Retired with test environments; ignored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub testing_generation: Option<i64>,
    pub request: Operation,
}
impl RelayRequest {
    pub fn new(profile: impl Into<String>, request: Operation) -> Self {
        Self {
            request_id: Uuid::new_v4(),
            profile: profile.into(),
            testing_environment_id: None,
            testing_generation: None,
            request,
        }
    }
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
/// Loopback-only HTTP relay client; its bearer is a local credential of this DM home, never
/// a DM access token.
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
        self.submit_json(&crate::Envelope::new("request", request))
            .await
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
        .json::<crate::Envelope<RelayAcknowledgement>>()
        .await?
        .data)
    }
    pub async fn result(&self, id: Uuid) -> Result<RelayResult> {
        let url = self
            .base
            .join(&format!("requests/{id}"))
            .map_err(|e| crate::Error::Configuration(e.to_string()))?;
        Ok(
            crate::checked(self.http.get(url).bearer_auth(&self.token).send().await?)
                .await?
                .json::<crate::Envelope<RelayResult>>()
                .await?
                .data,
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
                .json::<crate::Envelope<RelayRequestStatus>>()
                .await?
                .data,
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
                .json::<crate::Envelope<Value>>()
                .await?
                .data,
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
    /// Asks the shared relay to serve a DM home; requires the relay's own token
    /// from its private host record. Returns that home's status.
    pub async fn attach(&self, directory: &std::path::Path) -> Result<Value> {
        let url = self
            .base
            .join("homes")
            .map_err(|e| crate::Error::Configuration(e.to_string()))?;
        let body = crate::Envelope::new("attach", json!({ "directory": directory }));
        Ok(crate::checked(
            self.http
                .post(url)
                .bearer_auth(&self.token)
                .json(&body)
                .send()
                .await?,
        )
        .await?
        .json::<crate::Envelope<Value>>()
        .await?
        .data)
    }
}
