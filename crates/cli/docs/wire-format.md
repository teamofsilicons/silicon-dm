# DM message schema

DM 0.8 uses HTTP contract **2** and WebSocket frames **4** (including frames inside shared transport 1). Upgrade the API, worker, CLI/relay, web frontend, and webhook consumers together. Requests use `{ "type": "...", "data": {...} }`. Durable message events additionally carry root transport `metadata`.

## Address a recipient

Authenticated senders can send directly to an authorized Carbon or Silicon ID:

```sh
dm messages send cos:tos --text 'hey'
dm messages send cos:tos --text "what's up" --reply-to 000
dm messages send g:tos:team --attachment https://files.example/report.pdf
```

`POST /api/v1/conversations/cos:tos/messages` resolves or creates the permitted direct conversation. `POST /api/v1/messages` also accepts `recipient_id` inside `data`. IAM permission checks still apply. Other conversation routes accept the recipient to resolve an existing chat.

Direct conversation addresses order Carbon before Silicon, then lexically within the same type: `saket::cos:tos` is the same chat in both directions. Groups retain immutable `g:tos:team` addresses. Always scope identifiers by organization and testing environment. Internal conversation/message UUIDs stay intact and existing UUID aliases remain accepted.

## Short message IDs

`message-id` is a lowercase base36 code local to the conversation, not a UUID. It starts at `000`, advances through `001` … `009`, `00a` … `zzz`, then grows to `1000`. There are 46,656 three-character values. Length grows again whenever the current space is exhausted. Existing messages derive their codes from the durable conversation counter, so no history is renumbered.

Edits, deletion and idempotent retries preserve the code. Deleted codes are never reused. Identify a message using `(organization, testing environment, conversation_id, message-id)`; caches must not key on the short code alone. No public message `sequence` is needed.

## Send content

```json
{
  "type": "message.created",
  "data": {
    "recipient_id": "cos:tos",
    "message": "broo check this",
    "attachments": ["https://files.example/report.pdf"],
    "voice_transcript": null,
    "reply": null
  }
}
```

Text alone, attachments alone, or both are valid. Omitted/empty text with no attachments is rejected. Attachments are HTTPS URL strings, at most 100 per message. DM does not upload or fetch them. Audio and GIF URLs use this same list. An audio message can include `voice_transcript`; the transcript requires an attachment.

A reply input needs only `"reply": {"message-id": "000"}`. DM resolves the target in the same authorized conversation and supplies its sender and current content. Client-supplied quotations are ignored. The quoted content is null once the original is deleted. Text and transcripts retain the existing 100,000,000-character limits.

## Fixed message output

Every message snapshot includes the same fields, including null timestamps and empty lists:

```json
{
  "type": "message.created",
  "data": {
    "message-id": "001",
    "conversation_id": "saket::cos:tos",
    "recipient_id": "cos:tos",
    "sender": {"id": "saket", "type": "carbon"},
    "message": "what's up",
    "attachments": [],
    "voice_transcript": null,
    "reply": {
      "message-id": "000",
      "sender": {"id": "cos:tos", "type": "silicon"},
      "content": {"message": "hey", "attachments": [], "voice_transcript": null}
    },
    "bundle": null,
    "version": 1,
    "created_at": "2026-09-17T07:17:32Z",
    "updated_at": null,
    "deleted_at": null,
    "delivered_at": null,
    "read_at": null
  },
  "metadata": {
    "source": "dm",
    "delivery_id": "01a0ae3a-5186-7e22-9bbe-73818b2f0881",
    "delivery_sequence": 12
  }
}
```

`bundle` preserves the existing bundle feature and is null for ordinary messages. Message bodies have no `actor_id`, `sequence`, `status`, `profile`, caller `metadata`, or separate `voice`/`gif` object. No `interface_attachment_ids` are emitted. A deleted message keeps its identity, sender, version and timestamps with `message: null`, empty attachments, and null transcript/reply. Attachment-only messages use `message: ""`.

History responses identify the intended account/group in `recipient_id`. Each callback identifies the receiving account in `recipient_id`, including a sender's own copy; `conversation_id` identifies the chat/group. The `sender` object identifies the actual author.

## Events and transport

| Event | Meaning |
| --- | --- |
| `message.created` | A version-1 message snapshot |
| `message.updated` | An edited message snapshot |
| `message.deleted` | A content-free tombstone |
| `message.delivered` | Delivery receipt with the current full message |
| `message.read` | Read receipt with the current full message |
| `message.failed` | Failed-delivery notification with the current full message |

`version` advances on edits/deletion, not receipts. `updated_at` records the most recent content edit. Receipt timestamps remain null until recorded. Replayed delivery records hydrate the current message; an older delivery can therefore carry a newer edit/tombstone. Apply content by conversation, code and version; never resurrect deleted content.

`delivery_id` remains a UUID, stable across transport retries. `delivery_sequence` is the receiving account's ordered ACK/replay cursor, independent of message codes. These transport fields appear once, in root `metadata`, on durable WebSocket events and callbacks. HTTP message responses have only `type` and `data`, since an HTTP response is not an actor-stream delivery.

| HTTP operation | Type |
| --- | --- |
| POST `/conversations/{recipient-or-chat}/messages` or `/messages` | `message.created` |
| GET `/conversations/{chat}/messages` | `messages` (paginated `data.items`) |
| GET `/conversations/{chat}/messages/{code}` | `message` |
| PATCH `/conversations/{chat}/messages/{code}` | `message.updated` |
| DELETE `/conversations/{chat}/messages/{code}` | `message.deleted` |
| POST `/conversations/{chat}/messages/{code}/receipts` | `receipt` (current full message response) |

HTTP authorization, idempotency keys, conditional versions, and opaque cursors retain their meaning. PATCH replaces content. Errors retain `type: "error"` and `data.error`. Bodyless requests and 204 responses stay bodyless.

WebSocket control commands retain `type`/`data`: `new_message` sends content with `actor_id`, `org_id`, `conversation_id` (recipient accepted), and `idempotency_key`. `message_accepted` confirms it with the full message and retry key. `receipt` commands use the chat and short `message_id`; `receipt_recorded` confirms the short reference. ACK/resume still select an `actor_id` stream with `through_sequence`/`after_sequence`. Heartbeats are unsequenced.

Callbacks no longer expose local profile or testing-selector fields. A callback endpoint should be configured for its intended environment. Persist the event before responding with HTTP 2xx and:

```json
{"type":"ack","data":{"acknowledged":true,"delivery_id":"received UUID"}}
```

Silicon endpoints may alternatively respond `{"status":"ok","event_id":"NON_NIL_UUID"}`. The outgoing `Idempotency-Key` is the delivery UUID. Missing or mismatched acknowledgements leave delivery pending. Saved legacy message callbacks are projected to the fixed shape without changing delivery IDs; old receipt-only queue entries retain their original receipt contract.

See [OpenAPI](../openapi.yaml) for schemas and [contracts](contracts.md) for negotiation.
