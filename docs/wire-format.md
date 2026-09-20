# DM wire format

DM 0.9 uses HTTP contract **3**, WebSocket protocol **5**, and shared transport **2**. Upgrade the API, worker, CLI/relay, web gateway and callback consumers together. Explicit unsupported contract versions return 406.

Requests and responses use `{"type":"...","data":{...}}`. Full schemas and examples are in `openapi.yaml`.

## Conversations and identifiers

`GET /api/v1/conversations` lists normal DMs and accessible groups. Participants include everyone with access. Filter its items for groups; there is no separate GET `/groups`. Group summaries omit `is_public`, `version`, and `invited_members`; group administration endpoints retain policy details.

Conversation IDs are immutable addresses such as `alice::bob` and `g:tos:engineering`. Message IDs use conversation-local lowercase base36 codes, starting at `000`, padded to three characters and expanding to `1000` after `zzz`. Legacy message/conversation UUID aliases remain accepted. Bundle IDs independently start at `001` in each conversation. A bundle reference is `{"id":"001","role":"display"}` on its summary or `{"id":"001","role":"member"}` on an original message.

Conversation `last_message_status` is `sent`, `delivered`, `read`, or null. Group delivery/read requires every recipient. `updated_at` advances on messages, edits, deletion, reactions and group details/membership changes. Receipts, presence, typing and private drafts do not reorder conversations.

## Messages and history

A message contains `message-id`, `conversation_id`, `recipient_id`, `sender`, `message`, `attachments`, `voice_transcript`, `reply`, `bundle`, `history`, `created_at`, `updated_at`, `deleted_at`, `delivered_at` and `read_at`.

Attachments are credential-free HTTPS URLs. Text or at least one attachment is required. Attachment-only messages have empty text. Reply content is resolved by the server. Caller-supplied content cannot change sender authority or conversation routing.

New messages have `history: []`. Every accepted edit appends the previous content snapshot, oldest first:

```json
{
  "message": "See you Friday",
  "history": [
    {
      "message": "See you Thursday",
      "attachments": [],
      "voice_transcript": null,
      "reply": null,
      "created_at": "2026-09-17T10:00:00Z"
    }
  ]
}
```

A history entry's timestamp is when that content became current. Message edits/deletes have no numeric version or `If-Match`. Edits serialize under the message lock; later edits preserve the content they replace. Idempotency keys prevent duplicate retry entries. Group and draft concurrency tokens remain separate.

Deletion sets `deleted_at` without appending history. Stored history is retained; deleted public snapshots hide text, attachments, transcript, reply and history. Deleted messages cannot be edited or resurrected. Use `updated_at` to compare edits and always preserve a known deletion when merging delayed snapshots.

## WebSockets

Standalone `/api/v1/ws` requires repeated `members`, `org_id`, and stable `device_id` query parameters plus bearer authorization. Device IDs are nonempty client-generated strings of at most 255 UTF-8 bytes without control characters. They are not required to be UUIDs. `connection.ready` gives a UUID connection ID, authorized members and saved ACK cursors.

| Command | Success | Failure |
|---|---|---|
| `message.create` | `message.create.successful` | `message.create.error` |
| `bundle` | `bundle.success` | `bundle.error` |
| `receipt` | `receipt.success` | `receipt.error` |
| `presence` | `presence.success` | `presence.error` |
| `ack` | `ack.success` | `ack.error` |
| `resume` | `resume.success` | `resume.error` |
| server `ping` | client `ping.success` | client `ping.error` |
| shared `subscribe` | channel `subscribe.success` | channel `subscribe.error` |
| shared `unsubscribe` | outer `unsubscribe.success` | outer `unsubscribe.error` |

Commands use `member_id`. Message/bundle commands additionally provide `org_id`, `conversation_id` and `idempotency_key`. Message content is directly in `data`. Bundle commands add `message_ids` and `display_message`: an authorized Silicon selects 1–100 distinct, previously unbundled messages in the same conversation and supplies the summary. Creation is atomic and identical retries return the original IDs. The CLI may use the equivalent HTTP operation; SDK callers can send `ClientFrame::CreateBundle` on either socket.

Command success replies have no delivery sequence and need no ACK. Command errors preserve available identifiers and include `code`, `message`, `recoverable`. Recoverable means the socket remains usable, not that the unchanged request will succeed. Unrecognized or malformed envelopes use `connection.error`.

`message.created`, `message.updated`, `message.deleted`, `message.delivered`, `message.read` and `message.failed` are durable broadcasts. Every participant, including the sender's devices, receives `message.created` for a new message. Their transport fields are mirrored at top-level `metadata` and inside `data.metadata`:

```json
{"type":"message.created","metadata":{"source":"dm","delivery_id":"22222222-2222-4222-8222-000000000001","delivery_sequence":42},"data":{"message-id":"000","metadata":{"source":"dm","delivery_id":"22222222-2222-4222-8222-000000000001","delivery_sequence":42}}}
```

This abbreviated example shows placement; each delivery includes the full message snapshot. Callbacks use the same envelope. Metadata is not a content-history entry.

ACKs are cumulative per member stream and device, across conversations. Reconnection automatically replays after the stored ACK. Explicit `resume` can replay earlier retained events without lowering the stored ACK. ACK only after durable local processing; never ACK beyond an emitted sequence. Changed testing generations reset local cursors and caches; production uses null.

Shared `/api/v1/ws/shared` first sends `connection.prewarmed`. Subscribe independently for each account, token, organization and device, up to 64 subscriptions. Wrap inner frames as `{"type":"channel","data":{"subscription_id":"work","frame":{...}}}`. Each channel has its own heartbeat and authorization. Answer outer heartbeats too. Unsubscribe removes just one channel; `subscription.closed` is an unsolicited closure notice.

Creation requests use `message.create` over HTTP and WebSocket. The direct success reply to the initiating request uses `message.create.successful`; failed creates never emit it. Separately, every participant receives a durable `message.created` notification, including the sender's devices. Durable notifications and callbacks carry `metadata.delivery_id` and `metadata.delivery_sequence` and need ACKs after durable processing; command replies do not. During upgrades, clients also read the 0.9.3 delivery names `message.create` and `message.create.successful` when delivery metadata is present, and older `message.create.success` command replies.

Draft saves use compare-and-swap versions. A stale save returns HTTP 409 with `data.error.code: draft_conflict`, recovery instructions, and the current draft fields in `data`. The save was not applied. Keep local content, resolve against the returned version, and retry explicitly.

The browser renews its saved IAM session and reconnects after an authority-related WebSocket close. An HTTP 401 also renews the selected profile and retries the rejected request once, preserving the encoded body, idempotency key, draft version, testing generation, and cancellation signal. Access-token expiry alone does not sign the user out; rejected renewal, a signed-out profile, or a second HTTP 401 does. Temporary authentication outages retain the session for retry.

The browser gateway accepts public direct-conversation addresses and short message/bundle IDs for drafts, messages, and receipts as well as legacy UUIDs.
