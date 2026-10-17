//! DM's adapter inside a generic Ting consumer, for when the DM service hands its updates
//! to Ting (`DM_TING_URL` configured). This module never accepts a webhook batch,
//! dispatches callbacks, or records Ting/DM delivered/read ACKs.
//!
//! DM's Ting items are references, not content: `{"type": "dm.sync.changed", "for":
//! "<account uuid>", "key": <delivery id>, "data": {"schema_version": 2, "event",
//! "conversation_id", "message_id", "delivery_id", "delivery_sequence"}, "metadata":
//! {"isi"?}}`. Validate them with [`validate_batch`] against the account the consumer's hook
//! belongs to, then fetch each message through DM's HTTP API
//! ([`Client::hydrate_ting_batch`]) under the account's current permissions.
//!
//! Before calling it, the consumer must authenticate its configured callback secret and
//! `Ting-Webhook-Id`, then load that hook's persisted [`TingReceiverContext`]. Ting's local
//! webhook may omit `for`, so raw JSON alone cannot authenticate its recipient. Durable
//! deduplication and unrelated applications remain the generic consumer's work.
//!
//! While the DM service has no Ting configured (the default), nothing arrives this way:
//! read the sync feed instead (`GET /api/v1/sync?wait=30`, `dm inbox --watch`).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::{ActorType, Client, Error, Me, Message, Result};

const MAX_BATCH_BYTES: usize = 4 * 1024 * 1024;
const MAX_BATCH_ITEMS: usize = 100;
/// The reference schema DM 0.15 (contract 4) publishes.
pub const REFERENCE_SCHEMA_VERSION: u8 = 2;

/// The account a consumer's Ting hook belongs to, stored by the consumer when it attached
/// the hook; never assembled from webhook JSON. Match it to the current `Client::me()`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TingReceiverContext {
    /// The producer app (`dm`); its items have type `{app_id}.sync.changed`.
    pub app_id: String,
    /// The recipient's permanent Silicon Accounts uuid (compared exactly).
    pub account_uuid: String,
    /// The recipient's current public id, for display and for older `for` strings.
    pub account_id: String,
    pub kind: ActorType,
}
impl TingReceiverContext {
    /// The receiver context of the account behind `me` for producer `app_id`.
    pub fn for_account(app_id: impl Into<String>, me: &Me) -> Option<Self> {
        Some(Self {
            app_id: app_id.into(),
            account_uuid: me.account.uuid.clone()?,
            account_id: me.account.id.clone(),
            kind: me.account.actor_type,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DmEventKind {
    #[serde(rename = "message.created")]
    Created,
    #[serde(rename = "message.updated")]
    Updated,
    #[serde(rename = "message.deleted")]
    Deleted,
    #[serde(rename = "message.delivered")]
    Delivered,
    #[serde(rename = "message.read")]
    Read,
    #[serde(rename = "message.failed")]
    Failed,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReferenceData {
    schema_version: u8,
    event: DmEventKind,
    conversation_id: String,
    message_id: String,
    delivery_id: Uuid,
    delivery_sequence: i64,
}

/// A validated DM hint. Its sequence is not an HTTP sync cursor or an ACK.
#[derive(Clone, Debug)]
pub struct DmTingReference {
    pub index: usize,
    pub ting_id: String,
    pub event: DmEventKind,
    pub conversation_id: String,
    pub message_id: String,
    pub delivery_id: Uuid,
    pub delivery_sequence: i64,
    pub isi: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TingRejection {
    InvalidReference,
    WrongRecipient,
    InvalidMetadata,
    /// A reference from before Silicon Accounts (schema 1, organization-scoped); DM no longer
    /// produces them and they cannot be matched to an account.
    LegacyReference,
}

/// One outcome per input item. Ignored/rejected items are not acknowledged.
#[derive(Clone, Debug)]
pub enum TingItem {
    Unrelated {
        index: usize,
    },
    Rejected {
        index: usize,
        ting_id: Option<String>,
        reason: TingRejection,
    },
    Reference(DmTingReference),
}

#[derive(Debug, thiserror::Error)]
pub enum TingBatchError {
    #[error("Ting batch exceeds the 4 MiB or 100 item bound")]
    TooLarge,
    #[error("Ting webhook must contain a JSON object with a tings array")]
    InvalidBatch,
    #[error("the hook's stored account does not match the account DM authenticated")]
    InvalidReceiver,
}

/// Validates references without I/O. `me` must come from an authenticated DM `me()`;
/// receiver binding and callback authentication belong to the consumer.
pub fn validate_batch(
    raw: &[u8],
    receiver: &TingReceiverContext,
    me: &Me,
) -> std::result::Result<Vec<TingItem>, TingBatchError> {
    validate_receiver(receiver, me)?;
    if raw.len() > MAX_BATCH_BYTES {
        return Err(TingBatchError::TooLarge);
    }
    let batch: Value = serde_json::from_slice(raw).map_err(|_| TingBatchError::InvalidBatch)?;
    let tings = batch
        .get("tings")
        .and_then(Value::as_array)
        .ok_or(TingBatchError::InvalidBatch)?;
    if tings.len() > MAX_BATCH_ITEMS {
        return Err(TingBatchError::TooLarge);
    }
    let event_type = format!("{}.sync.changed", receiver.app_id);
    Ok(tings
        .iter()
        .enumerate()
        .map(|(index, item)| {
            if item.get("type").and_then(Value::as_str) != Some(event_type.as_str()) {
                return TingItem::Unrelated { index };
            }
            validate_item(index, item, receiver)
        })
        .collect())
}

fn validate_receiver(
    receiver: &TingReceiverContext,
    me: &Me,
) -> std::result::Result<(), TingBatchError> {
    // Account kind and uuid come from verified DM identity, never from a guessed id prefix.
    if !bounded_text(&receiver.app_id, 255)
        || !bounded_text(&receiver.account_uuid, 255)
        || me.account.uuid.as_deref() != Some(receiver.account_uuid.as_str())
        || me.account.actor_type != receiver.kind
    {
        return Err(TingBatchError::InvalidReceiver);
    }
    Ok(())
}

fn validate_item(index: usize, item: &Value, receiver: &TingReceiverContext) -> TingItem {
    let ting_id = item
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| bounded_text(id, 255));
    let rejected = |reason| TingItem::Rejected {
        index,
        ting_id: ting_id.map(str::to_owned),
        reason,
    };
    let Some(ting_id) = ting_id else {
        return rejected(TingRejection::InvalidReference);
    };
    // Organization-scoped references were produced before Silicon Accounts.
    if item.get("org_id").is_some()
        || item.pointer("/data/org_id").is_some()
        || item.pointer("/data/schema_version").and_then(Value::as_u64) == Some(1)
    {
        return rejected(TingRejection::LegacyReference);
    }
    // Ting inbox objects carry the immutable UUID in `for`; local hooks may omit it.
    // Also accept the structured UUID form used by earlier adapters.
    if let Some(target) = item.get("for") {
        let matches = match target {
            Value::Object(object) => {
                object.get("uuid").and_then(Value::as_str) == Some(receiver.account_uuid.as_str())
            }
            Value::String(id) => *id == receiver.account_uuid,
            _ => false,
        };
        if !matches {
            return rejected(TingRejection::WrongRecipient);
        }
    }
    let Ok(data) =
        serde_json::from_value::<ReferenceData>(item.get("data").cloned().unwrap_or(Value::Null))
    else {
        return rejected(TingRejection::InvalidReference);
    };
    if data.schema_version != REFERENCE_SCHEMA_VERSION
        || data.delivery_id.is_nil()
        || data.delivery_sequence < 1
        || crate::validate_conversation_id(&data.conversation_id).is_err()
        || silicon_dm_protocol::message_sequence(&data.message_id).is_none()
        || item.get("key").and_then(Value::as_str) != Some(data.delivery_id.to_string().as_str())
    {
        return rejected(TingRejection::InvalidReference);
    }
    let empty = serde_json::Map::new();
    let metadata = match item.get("metadata") {
        None | Some(Value::Null) => &empty,
        Some(Value::Object(metadata)) => metadata,
        Some(_) => return rejected(TingRejection::InvalidMetadata),
    };
    if metadata.keys().any(|key| key != "isi") {
        return rejected(TingRejection::InvalidMetadata);
    }
    let isi = match metadata.get("isi") {
        None => None,
        Some(value) => match value.as_str() {
            Some(isi)
                if receiver.kind == ActorType::Silicon
                    && bounded_text(isi, 255)
                    && !isi
                        .chars()
                        .any(|c| c.is_whitespace() || matches!(c, ':' | '@')) =>
            {
                Some(isi.to_owned())
            }
            _ => return rejected(TingRejection::InvalidMetadata),
        },
    };
    TingItem::Reference(DmTingReference {
        index,
        ting_id: ting_id.to_owned(),
        event: data.event,
        conversation_id: data.conversation_id,
        message_id: data.message_id,
        delivery_id: data.delivery_id,
        delivery_sequence: data.delivery_sequence,
        isi,
    })
}

fn bounded_text(value: &str, maximum: usize) -> bool {
    !value.is_empty() && value.len() <= maximum && !value.chars().any(char::is_control)
}

/// Each reference hydrates independently; transient failures stay visible for the
/// generic consumer to retry without losing other apps' or items' work.
#[derive(Debug)]
pub enum HydratedTingItem {
    Skipped(TingItem),
    Message {
        reference: DmTingReference,
        message: Box<Message>,
    },
    Inaccessible {
        reference: DmTingReference,
    },
    Failed {
        reference: DmTingReference,
        error: Error,
    },
}

impl Client {
    /// Validates a generic consumer's batch against the account DM authenticates now, then
    /// fetches recognized messages through normal authenticated HTTP GETs. Deleted messages
    /// remain ordinary DM tombstones; 403/404 are inaccessible. This method never
    /// dispatches callbacks, changes receipts or ACKs a Ting.
    pub async fn hydrate_ting_batch(
        &self,
        raw: &[u8],
        receiver: &TingReceiverContext,
    ) -> Result<Vec<HydratedTingItem>> {
        if raw.len() > MAX_BATCH_BYTES {
            return Err(Error::Configuration(TingBatchError::TooLarge.to_string()));
        }
        let me = self.me().await?;
        let items = validate_batch(raw, receiver, &me)
            .map_err(|error| Error::Configuration(error.to_string()))?;
        let mut hydrated = Vec::with_capacity(items.len());
        for item in items {
            let TingItem::Reference(reference) = item else {
                hydrated.push(HydratedTingItem::Skipped(item));
                continue;
            };
            hydrated.push(
                match self
                    .message(&reference.conversation_id, &reference.message_id)
                    .await
                {
                    Ok(message) if message.id == reference.message_id => {
                        HydratedTingItem::Message {
                            reference,
                            message: Box::new(message),
                        }
                    }
                    Ok(_) => HydratedTingItem::Failed {
                        reference,
                        error: Error::Configuration(
                            "DM message response did not match its validated reference".into(),
                        ),
                    },
                    Err(Error::Api {
                        status: 403 | 404, ..
                    }) => HydratedTingItem::Inaccessible { reference },
                    Err(error) => HydratedTingItem::Failed { reference, error },
                },
            );
        }
        Ok(hydrated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Actor, Delivery};
    use serde_json::json;

    fn context() -> (TingReceiverContext, Me) {
        let me = Me {
            account: Actor::new(ActorType::Silicon, "si:cos").with_uuid("K1E"),
            custodian: None,
            custodian_of: Vec::new(),
            scopes: Vec::new(),
            via_app: None,
            delivery: Delivery::default(),
            speech: None,
        };
        (
            TingReceiverContext::for_account("dm", &me).expect("uuid"),
            me,
        )
    }

    fn event() -> Value {
        let id = Uuid::new_v4();
        json!({"id":"msg_123", "type":"dm.sync.changed", "created_at":"2026-10-10T10:00:00Z", "key":id,
            "metadata":{"isi":"planner"},
            "data":{"schema_version":2,"event":"message.created","conversation_id":"c:alice::si:cos",
                "message_id":"000","delivery_id":id,"delivery_sequence":1}})
    }

    fn validate(item: Value, receiver: &TingReceiverContext, me: &Me) -> TingItem {
        validate_batch(
            &serde_json::to_vec(&json!({"tings":[item]})).unwrap_or_default(),
            receiver,
            me,
        )
        .unwrap_or_default()
        .remove(0)
    }

    #[test]
    fn references_bind_to_the_account_uuid_and_leave_other_apps_unclaimed() {
        let (receiver, me) = context();
        let mut other = event();
        other["type"] = json!("remind.sync.changed");
        let items = validate_batch(
            &serde_json::to_vec(&json!({"tings":[event(),other]})).unwrap_or_default(),
            &receiver,
            &me,
        )
        .unwrap_or_default();
        assert!(
            matches!(&items[0],TingItem::Reference(reference) if reference.isi.as_deref()==Some("planner") && reference.delivery_sequence==1)
        );
        assert!(matches!(items[1], TingItem::Unrelated { index: 1 }));
        let mut addressed = event();
        addressed["for"] = json!({"uuid":"K1E","id":"si:cos"});
        assert!(matches!(
            validate(addressed, &receiver, &me),
            TingItem::Reference(_)
        ));
        for wrong in [
            json!({"uuid":"k1e","id":"si:cos"}),
            json!({"id":"si:cos"}),
            json!("si:cos"),
        ] {
            let mut item = event();
            item["for"] = wrong;
            assert!(matches!(
                validate(item, &receiver, &me),
                TingItem::Rejected {
                    reason: TingRejection::WrongRecipient,
                    ..
                }
            ));
        }
        let mut wrong_kind = receiver.clone();
        wrong_kind.kind = ActorType::Carbon;
        assert!(matches!(
            validate_batch(b"{\"tings\":[]}", &wrong_kind, &me),
            Err(TingBatchError::InvalidReceiver)
        ));
        let mut wrong_account = receiver;
        wrong_account.account_uuid = "other".into();
        assert!(matches!(
            validate_batch(b"{\"tings\":[]}", &wrong_account, &me),
            Err(TingBatchError::InvalidReceiver)
        ));
    }

    #[test]
    fn schema_key_and_metadata_are_validated_before_hydration() {
        let (receiver, me) = context();
        for (pointer, value) in [
            ("/data/schema_version", json!(3)),
            ("/data/delivery_sequence", json!(0)),
            ("/data/conversation_id", json!("https://evil.invalid")),
            ("/data/message_id", json!("../secret")),
            ("/data/event", json!("other.event")),
            ("/key", json!(Uuid::new_v4())),
            ("/metadata/isi", json!("bad@route")),
            ("/metadata", json!("not an object")),
        ] {
            let mut candidate = event();
            *candidate.pointer_mut(pointer).unwrap_or(&mut Value::Null) = value;
            assert!(
                matches!(
                    validate(candidate, &receiver, &me),
                    TingItem::Rejected { .. }
                ),
                "{pointer}"
            );
        }
        let mut extra = event();
        extra["metadata"]["testing_generation"] = json!(1);
        assert!(matches!(
            validate(extra, &receiver, &me),
            TingItem::Rejected {
                reason: TingRejection::InvalidMetadata,
                ..
            }
        ));
        let mut payload = event();
        payload["data"]["message"] = json!("message content is not the reference schema");
        assert!(matches!(
            validate(payload, &receiver, &me),
            TingItem::Rejected {
                reason: TingRejection::InvalidReference,
                ..
            }
        ));
        let mut bare = event();
        bare.as_object_mut().map(|o| o.remove("metadata"));
        assert!(matches!(
            validate(bare, &receiver, &me),
            TingItem::Reference(_)
        ));
    }

    #[test]
    fn organization_scoped_references_from_before_silicon_accounts_are_rejected() {
        let (receiver, me) = context();
        let mut outer = event();
        outer["org_id"] = json!("tos");
        let mut inner = event();
        inner["data"]["org_id"] = json!("tos");
        let mut first_schema = event();
        first_schema["data"]["schema_version"] = json!(1);
        for item in [outer, inner, first_schema] {
            assert!(matches!(
                validate(item, &receiver, &me),
                TingItem::Rejected {
                    reason: TingRejection::LegacyReference,
                    ..
                }
            ));
        }
    }

    #[test]
    fn batch_limits_and_shape_are_enforced_without_item_processing() {
        let (receiver, me) = context();
        for invalid in [b"[]".as_slice(), b"{}", b"{\"tings\":null}", b"{"] {
            assert!(matches!(
                validate_batch(invalid, &receiver, &me),
                Err(TingBatchError::InvalidBatch)
            ));
        }
        assert!(matches!(
            validate_batch(&vec![b' '; MAX_BATCH_BYTES + 1], &receiver, &me),
            Err(TingBatchError::TooLarge)
        ));
        let too_many =
            serde_json::to_vec(&json!({"tings":vec![Value::Null;101]})).unwrap_or_default();
        assert!(matches!(
            validate_batch(&too_many, &receiver, &me),
            Err(TingBatchError::TooLarge)
        ));
    }
}
