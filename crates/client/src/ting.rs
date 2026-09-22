//! DM's adapter inside a generic Ting consumer. This module never accepts a
//! webhook batch, dispatches callbacks, or records Ting/DM delivered/read ACKs.
//!
//! Before calling it, the consumer must authenticate its configured callback
//! secret and `Ting-Webhook-Id`, then load that hook's persisted typed recipient
//! binding. Ting's local webhook deliberately omits `for`; raw JSON alone cannot
//! authenticate its recipient. Durable deduplication and unrelated applications
//! remain the generic consumer's work.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{Actor, ActorType, Client, Error, Identity, Message, Result};

const MAX_BATCH_BYTES: usize = 4 * 1024 * 1024;
const MAX_BATCH_ITEMS: usize = 100;

/// Trusted generic-consumer hook binding, never assembled from webhook JSON.
/// Match this to current `Client::me()` and `Client::iam()` before hydration.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TingReceiverContext {
    pub app_id: String,
    pub organization_id: String,
    pub actor: Actor,
    pub testing_environment_id: Option<Uuid>,
    pub testing_generation: Option<i64>,
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
    org_id: String,
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
    pub organization_id: String,
    pub conversation_id: String,
    pub message_id: String,
    pub delivery_id: Uuid,
    pub delivery_sequence: i64,
    pub isi: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TingRejection {
    InvalidReference,
    WrongOrganization,
    WrongRecipient,
    WrongEnvironment,
    GenerationMismatch,
    InvalidMetadata,
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
    StaleGeneration {
        index: usize,
        ting_id: String,
        generation: Option<i64>,
    },
    Reference(DmTingReference),
}

#[derive(Debug, thiserror::Error)]
pub enum TingBatchError {
    #[error("Ting batch exceeds the 4 MiB or 100 item bound")]
    TooLarge,
    #[error("Ting webhook must contain a JSON object with a tings array")]
    InvalidBatch,
    #[error("trusted Ting hook binding does not match the authenticated DM identity")]
    InvalidReceiver,
}

/// Validates references without I/O. `identity` must come from authenticated DM
/// `me()`; receiver binding and callback authentication belong to the consumer.
pub fn validate_batch(
    raw: &[u8],
    receiver: &TingReceiverContext,
    identity: &Identity,
) -> std::result::Result<Vec<TingItem>, TingBatchError> {
    validate_receiver(receiver, identity)?;
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
    identity: &Identity,
) -> std::result::Result<(), TingBatchError> {
    let paired = match (receiver.testing_environment_id, receiver.testing_generation) {
        (None, None) => true,
        (Some(id), Some(generation)) => !id.is_nil() && generation > 0,
        _ => false,
    };
    // Actor type comes from verified DM identity, never from a guessed ID prefix.
    if !paired
        || !bounded_text(&receiver.app_id, 255)
        || !bounded_text(&receiver.organization_id, 255)
        || !bounded_text(&receiver.actor.id, 255)
        || receiver.actor != identity.actor
        || receiver.organization_id != identity.organization_id
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
    // Full inbox objects may include `for`; local webhook items omit it.
    if item
        .get("for")
        .is_some_and(|target| target.as_str() != Some(receiver.actor.id.as_str()))
    {
        return rejected(TingRejection::WrongRecipient);
    }
    if item
        .get("org_id")
        .is_some_and(|org| org.as_str() != Some(receiver.organization_id.as_str()))
    {
        return rejected(TingRejection::WrongOrganization);
    }
    let Ok(data) =
        serde_json::from_value::<ReferenceData>(item.get("data").cloned().unwrap_or(Value::Null))
    else {
        return rejected(TingRejection::InvalidReference);
    };
    if data.org_id != receiver.organization_id {
        return rejected(TingRejection::WrongOrganization);
    }
    if data.schema_version != 1
        || data.delivery_id.is_nil()
        || data.delivery_sequence < 1
        || crate::validate_conversation_id(&data.conversation_id).is_err()
        || silicon_dm_protocol::message_sequence(&data.message_id).is_none()
        || item.get("key").and_then(Value::as_str) != Some(data.delivery_id.to_string().as_str())
    {
        return rejected(TingRejection::InvalidReference);
    }
    let Some(metadata) = item.get("metadata").and_then(Value::as_object) else {
        return rejected(TingRejection::InvalidMetadata);
    };
    if metadata.keys().any(|key| {
        !matches!(
            key.as_str(),
            "testing_environment_id" | "testing_generation" | "isi"
        )
    }) {
        return rejected(TingRejection::InvalidMetadata);
    }
    if metadata.get("testing_environment_id") != Some(&json!(receiver.testing_environment_id)) {
        return rejected(TingRejection::WrongEnvironment);
    }
    let Some(generation) = metadata.get("testing_generation") else {
        return rejected(TingRejection::InvalidMetadata);
    };
    if generation != &json!(receiver.testing_generation) {
        if receiver.testing_generation.is_some_and(|current| {
            generation
                .as_i64()
                .is_some_and(|value| value > 0 && value < current)
        }) {
            return TingItem::StaleGeneration {
                index,
                ting_id: ting_id.to_owned(),
                generation: generation.as_i64(),
            };
        }
        return rejected(TingRejection::GenerationMismatch);
    }
    let isi = match metadata.get("isi") {
        None => None,
        Some(value) => match value.as_str() {
            Some(isi)
                if receiver.actor.actor_type == ActorType::Silicon
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
        organization_id: data.org_id,
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
    /// Validates a generic consumer's batch against fresh DM identity/discovery,
    /// then fetches recognized messages through normal authenticated HTTP GETs.
    /// Deleted messages remain ordinary DM tombstones; 403/404 are inaccessible.
    /// This method never dispatches callbacks, changes receipts or ACKs a Ting.
    pub async fn hydrate_ting_batch(
        &self,
        raw: &[u8],
        receiver: &TingReceiverContext,
    ) -> Result<Vec<HydratedTingItem>> {
        if raw.len() > MAX_BATCH_BYTES {
            return Err(Error::Configuration(TingBatchError::TooLarge.to_string()));
        }
        if self.organization.as_deref() != Some(receiver.organization_id.as_str()) {
            return Err(Error::Configuration(
                TingBatchError::InvalidReceiver.to_string(),
            ));
        }
        let identity = self.me().await?;
        let info = self.iam().await?;
        if info.app_id != receiver.app_id
            || info.testing_environment_id != receiver.testing_environment_id
            || info.testing_generation != receiver.testing_generation
            || self.testing_generation != receiver.testing_generation
            || self.test_key.is_some() != receiver.testing_environment_id.is_some()
        {
            return Err(Error::Configuration("Ting hook binding or DM client belongs to another environment/generation; refresh IAM discovery and the consumer binding".into()));
        }
        let items = validate_batch(raw, receiver, &identity)
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
                    Ok(message)
                        if message.id == reference.message_id
                            && message.conversation_id == reference.conversation_id =>
                    {
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

    fn context() -> (TingReceiverContext, Identity) {
        let actor = Actor {
            actor_type: ActorType::Silicon,
            id: "cos:tos".into(),
        };
        (
            TingReceiverContext {
                app_id: "tos>dm".into(),
                organization_id: "tos".into(),
                actor: actor.clone(),
                testing_environment_id: None,
                testing_generation: None,
            },
            Identity {
                actor,
                organization_id: "tos".into(),
                principal_id: "cos:tos".into(),
                session_id: None,
                org_role: None,
                capabilities: Vec::new(),
            },
        )
    }

    fn event() -> Value {
        let id = Uuid::new_v4();
        json!({"id":"msg_123", "type":"tos>dm.sync.changed", "created_at":"2026-09-22T10:00:00Z", "key":id,
            "metadata":{"testing_environment_id":null,"testing_generation":null,"isi":"planner"},
            "data":{"schema_version":1,"event":"message.created","org_id":"tos","conversation_id":"alice::cos:tos",
                "message_id":"000","delivery_id":id,"delivery_sequence":1}})
    }

    fn validate(item: Value, receiver: &TingReceiverContext, identity: &Identity) -> TingItem {
        validate_batch(
            &serde_json::to_vec(&json!({"tings":[item]})).unwrap(),
            receiver,
            identity,
        )
        .unwrap()
        .remove(0)
    }

    #[test]
    fn local_callback_uses_trusted_typed_binding_and_leaves_other_apps_unclaimed() {
        let (receiver, identity) = context();
        let mut other = event();
        other["type"] = json!("tos>remind.sync.changed");
        let items = validate_batch(
            &serde_json::to_vec(&json!({"tings":[event(),other]})).unwrap(),
            &receiver,
            &identity,
        )
        .unwrap();
        assert!(
            matches!(&items[0],TingItem::Reference(reference) if reference.isi.as_deref()==Some("planner") && reference.delivery_sequence==1)
        );
        assert!(matches!(items[1], TingItem::Unrelated { index: 1 }));
        let mut wrong_kind = receiver.clone();
        wrong_kind.actor.actor_type = ActorType::Carbon;
        assert!(matches!(
            validate_batch(b"{\"tings\":[]}", &wrong_kind, &identity),
            Err(TingBatchError::InvalidReceiver)
        ));
        let mut wrong_recipient = event();
        wrong_recipient["for"] = json!("alice");
        assert!(matches!(
            validate(wrong_recipient, &receiver, &identity),
            TingItem::Rejected {
                reason: TingRejection::WrongRecipient,
                ..
            }
        ));
    }

    #[test]
    fn schema_tenant_key_and_metadata_are_validated_before_hydration() {
        let (receiver, identity) = context();
        for (pointer, value) in [
            ("/data/schema_version", json!(2)),
            ("/data/delivery_sequence", json!(0)),
            ("/data/conversation_id", json!("https://evil.invalid")),
            ("/data/message_id", json!("../secret")),
            ("/data/event", json!("other.event")),
            ("/key", json!(Uuid::new_v4())),
            ("/data/org_id", json!("other")),
            ("/metadata/testing_environment_id", json!(Uuid::new_v4())),
            ("/metadata/isi", json!("bad@route")),
        ] {
            let mut candidate = event();
            *candidate.pointer_mut(pointer).unwrap() = value;
            assert!(
                matches!(
                    validate(candidate, &receiver, &identity),
                    TingItem::Rejected { .. }
                ),
                "{pointer}"
            );
        }
        let mut payload = event();
        payload["data"]["message"] = json!("message content is not the reference schema");
        assert!(matches!(
            validate(payload, &receiver, &identity),
            TingItem::Rejected {
                reason: TingRejection::InvalidReference,
                ..
            }
        ));
        let mut missing = event();
        missing["metadata"]
            .as_object_mut()
            .unwrap()
            .remove("testing_generation");
        assert!(matches!(
            validate(missing, &receiver, &identity),
            TingItem::Rejected {
                reason: TingRejection::InvalidMetadata,
                ..
            }
        ));
    }

    #[test]
    fn stale_generation_is_distinct_from_another_environment_or_future_generation() {
        let (mut receiver, identity) = context();
        let environment = Uuid::new_v4();
        receiver.testing_environment_id = Some(environment);
        receiver.testing_generation = Some(2);
        let mut candidate = event();
        candidate["metadata"]["testing_environment_id"] = json!(environment);
        candidate["metadata"]["testing_generation"] = json!(1);
        assert!(matches!(
            validate(candidate.clone(), &receiver, &identity),
            TingItem::StaleGeneration {
                generation: Some(1),
                ..
            }
        ));
        let mut wrong_org = candidate.clone();
        wrong_org["data"]["org_id"] = json!("other");
        assert!(matches!(
            validate(wrong_org, &receiver, &identity),
            TingItem::Rejected {
                reason: TingRejection::WrongOrganization,
                ..
            }
        ));
        candidate["metadata"]["testing_generation"] = json!(3);
        assert!(matches!(
            validate(candidate.clone(), &receiver, &identity),
            TingItem::Rejected {
                reason: TingRejection::GenerationMismatch,
                ..
            }
        ));
        candidate["metadata"]["testing_environment_id"] = json!(Uuid::new_v4());
        assert!(matches!(
            validate(candidate, &receiver, &identity),
            TingItem::Rejected {
                reason: TingRejection::WrongEnvironment,
                ..
            }
        ));
    }

    #[test]
    fn batch_limits_and_shape_are_enforced_without_item_processing() {
        let (receiver, identity) = context();
        for invalid in [b"[]".as_slice(), b"{}", b"{\"tings\":null}", b"{"] {
            assert!(matches!(
                validate_batch(invalid, &receiver, &identity),
                Err(TingBatchError::InvalidBatch)
            ));
        }
        assert!(matches!(
            validate_batch(&vec![b' '; MAX_BATCH_BYTES + 1], &receiver, &identity),
            Err(TingBatchError::TooLarge)
        ));
        let too_many = serde_json::to_vec(&json!({"tings":vec![Value::Null;101]})).unwrap();
        assert!(matches!(
            validate_batch(&too_many, &receiver, &identity),
            Err(TingBatchError::TooLarge)
        ));
    }
}
