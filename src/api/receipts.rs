//! Reading in bulk: one request marks everything up to a message read (or delivered).

use axum::{Json, extract::State};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{
    extract::{ApiEnvelope, ApiPath, ApiQuery, Authenticated, ConversationAccess},
    handlers::{ConversationPath, ForQuery},
};
use crate::{
    AppError, AppResult,
    application::{commands::BulkReceiptCommand, messaging::validate_device_id, state::AppState},
    domain::ReceiptStatus,
};

/// Receipts recorded without a device id are recorded for this one.
const BULK_DEVICE_ID: &str = "bulk";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BulkReceiptInput {
    /// The last message to mark (a message code such as `0a3`, inclusive).
    up_to: String,
    /// With the generic type `receipts`: `read` or `delivered`.
    #[serde(default)]
    status: Option<ReceiptStatus>,
    /// The device the receipts are recorded for (default `bulk`).
    #[serde(default)]
    device_id: Option<String>,
}

/// 403 `read_only_view`: a custodian reads its Silicon's conversations but never marks them
/// read for it.
pub(super) fn read_only_view() -> AppError {
    AppError::forbidden(
        "read_only_view",
        "A custodian's view of a Silicon's conversation is read only: only the Silicon itself marks its messages read or delivered.",
    )
    .with_hint("Read without marking; the Silicon records its own receipts (`dm receipts read`).")
}

/// `POST /api/v1/conversations/{conversation}/receipts` with `receipts.read` or
/// `receipts.delivered` `{"up_to": "0a3"}`.
pub(super) async fn record_receipts(
    State(state): State<AppState>,
    Authenticated(context): Authenticated,
    access: ConversationAccess,
    ApiPath(path): ApiPath<ConversationPath>,
    ApiQuery(query): ApiQuery<ForQuery>,
    ApiEnvelope { kind, data }: ApiEnvelope<BulkReceiptInput>,
) -> AppResult<Json<Value>> {
    if query.for_account.is_some() || access != ConversationAccess::Participant {
        return Err(read_only_view());
    }
    let status = match (kind.as_str(), data.status) {
        ("receipts.read", None | Some(ReceiptStatus::Read)) => ReceiptStatus::Read,
        ("receipts.delivered", None | Some(ReceiptStatus::Delivered)) => ReceiptStatus::Delivered,
        ("receipts", Some(status)) => status,
        ("receipts", None) => {
            return Err(AppError::validation(
                "with type receipts, data.status must be read or delivered (or use type receipts.read / receipts.delivered)",
            ));
        }
        ("receipts.read" | "receipts.delivered", Some(_)) => {
            return Err(AppError::validation(
                "data.status contradicts the request type; omit it with receipts.read and receipts.delivered",
            ));
        }
        _ => {
            return Err(AppError::validation(
                "request type must be receipts.read or receipts.delivered",
            ));
        }
    };
    let device_id = data.device_id.unwrap_or_else(|| BULK_DEVICE_ID.to_owned());
    validate_device_id(&device_id)?;
    let up_to_sequence = state
        .store
        .message_sequence(path.conversation_id, data.up_to.trim())
        .await?;
    let outcome = state
        .store
        .record_receipts_up_to(&BulkReceiptCommand {
            conversation_id: path.conversation_id,
            recipient: context.actor.clone(),
            device_id,
            status,
            up_to_sequence,
        })
        .await?;
    Ok(Json(json!({
        "updated": outcome.updated,
        "unread_count": outcome.summary.unread_count,
        "last_read_message_id": outcome
            .summary
            .last_read_sequence
            .and_then(silicon_dm_protocol::message_code),
    })))
}
