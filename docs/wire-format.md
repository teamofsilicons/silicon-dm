# DM wire format

DM 0.10 uses HTTP contract **3**. Incoming delivery belongs to Ting; DM’s former standalone and shared WebSocket routes return HTTP 410 with `delivery_moved_to_ting`. Coordinate the API, worker, SDK, CLI, web gateway and callback migration. Explicit unsupported HTTP contract versions return 406.

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

## HTTP commands and Ting delivery

Send messages, create bundles, update receipts and maintain presence through HTTP.
Message creation uses `message.create`; its direct success reply is
`message.create.successful`. Retrying with the original idempotency key preserves
the accepted operation. Command success is separate from incoming delivery and
requires no transport ACK. The retired Rust socket methods return migration
guidance without opening a connection.

DM publishes one Ting type, `{app_id}.sync.changed` (`tos>dm.sync.changed` for the
canonical application). The durable payload contains references, not message
content. `data.event` identifies the change: `message.created`, `message.updated`,
`message.deleted`, `message.delivered`, `message.read` or `message.failed`.
For example, DM hands this body to Ting:

```json
{
  "org_id": "tos",
  "type": "tos>dm.sync.changed",
  "for": "bob",
  "key": "22222222-2222-4222-8222-000000000001",
  "data": {
    "schema_version": 1,
    "event": "message.created",
    "org_id": "tos",
    "conversation_id": "alice::bob",
    "message_id": "000",
    "delivery_id": "22222222-2222-4222-8222-000000000001",
    "delivery_sequence": 42
  },
  "metadata": {
    "testing_environment_id": null,
    "testing_generation": null
  }
}
```

Testing metadata instead contains the selected environment UUID and positive
generation; optional `metadata.isi` preserves instance routing. The stable UUID
key and original body survive retries. Ting acceptance means the handoff was
accepted; it does not mark the DM message Delivered or Read.

Ting owns receiver connections, queues, replay and transport ACKs. Its local
callback receives raw `{"tings":[...]}` batches, not DM’s `type`/`data` envelope.
The configured hook serves every eligible app for that recipient/org. Authenticate
its saved secret and `Ting-Webhook-Id`, route all apps, and durably accept the
entire batch before returning HTTP 204. Old DM cumulative-ACK JSON is retired.
Validate DM references and fetch canonical content with the recipient’s current
DM authorization; the stateless hydration helper sends no ACK or receipt.
Send DM Delivered and Read receipts explicitly through HTTP.

## Reconciliation and presence

Use `/api/v1/sync` with its opaque actor/org/environment-bound cursor. Start or
recover with `sync_reset()`, load the accessible message snapshot, then resume
from that boundary to capture concurrent changes. Commit hydrated changes,
receipt work and the new cursor together. Ting sequence numbers and retained
legacy DM ACK cursors are not HTTP sync cursors. Reconcile on startup, reconnect
and periodically: silent or muted tings need not produce an immediate hint.
A changed testing generation invalidates the previous cursor and cached data.

Presence uses HTTP device leases at `/api/v1/presence/devices/{device_id}`.
Refresh the lease while active and delete it when leaving; expiry clears stale
activity. Typing updates are transient and must not be replayed as durable sends.

Draft saves use compare-and-swap versions. A stale save returns HTTP 409 with `data.error.code: draft_conflict`, recovery instructions, and the current draft fields in `data`. The save was not applied. Keep local content, resolve against the returned version, and retry explicitly.

The browser renews its selected DM profile after an HTTP 401 and retries the rejected request once, preserving the encoded body, idempotency key, draft version, testing generation, and cancellation signal. Access-token expiry alone does not sign the user out; rejected renewal, a signed-out profile, or a second HTTP 401 does. Temporary authentication outages retain the session for retry. The separate Ting cookie session and watch are revalidated against the selected DM account, organization and environment; a DM refresh does not create or renew a Ting login.

The browser gateway accepts public direct-conversation addresses and short message/bundle IDs for drafts, messages, and receipts as well as legacy UUIDs.
