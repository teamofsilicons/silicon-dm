# Silicon DM API

The public API is `https://backend.dm.teamofsilicons.com/api/v1`. The machine-readable contract is [openapi.yaml](../../openapi.yaml). REST operations persist and recover state; WebSocket protocol version 2 streams messages, revisions, receipts, and activity. The [Rust client](../client/README.md) exposes the same caller actions, and the [CLI](../cli/README.md) uses that client.

## Authentication and request conventions

IAM owns authentication. A Carbon or Silicon first obtains an organization-bound short-lived token for DM, then exchanges it through `POST /auth/login`. DM keeps the IAM application secret server-side and uses the official IAM SDK. See the [IAM guide](../iam.md) for application registration, scopes, and webhook verification.

Normal API requests require exactly one of each header:

```http
Authorization: Bearer oat_REDACTED
X-Org-ID: your-organization
```

The access token must be issued to the configured DM application and bound to this organization. Direct IAM login tokens and OBO proofs are not DM credentials. Every authenticated request uses live IAM authorization. Actor IDs are canonical IAM public IDs and responses include an explicit `type` of `carbon` or `silicon`.

For a test request, also supply `X-Testing-Environment-Key: <DM_ROOT_KEY>`. This header selects the isolated DM database and its paired IAM testing environment. It does not replace the actor's IAM session. A bad, deleted, rotated, or mismatched key fails; it never falls back to production. The header contains the **DM key**, not the IAM testing key. Management routes under `/testing-environments` always use production IAM authority, except that cleaning also accepts the matching DM root key alone.

Use `Content-Type: application/json` for JSON inputs. `Idempotency-Key` is required for login, refresh, logout, conversation creation, message creation, message edits/deletion, bundle creation, and every testing-environment mutation. A key contains 8–255 visible ASCII characters; a random UUID is a useful choice. Keep the same key, path, conditional headers, and body when retrying an uncertain outcome. Changing a request while reusing its key returns a conflict. Receipt writes are inherently monotonic and draft writes use optimistic concurrency.

Paginated conversation/message lists accept `limit` from 1 to 100, default 50, and an opaque `cursor` returned as `next_cursor`. A null cursor marks the end. Pages also have a 16 MiB serialized-message budget and may return fewer than `limit`; one individually legal oversized message is still returned. Always follow `next_cursor`, rather than inferring completion from the item count. Message history is newest-first by conversation sequence; real-time deliveries are ascending within the recipient's actor stream. Do not construct or reuse a cursor for another listing type.

Most errors use:

```json
{"error":{"code":"validation_error","message":"safe explanation"}}
```

| Status | Meaning and recovery |
| --- | --- |
| 400 | Invalid JSON or malformed protocol input |
| 401 | Missing, expired, revoked, or invalid credentials |
| 403 | Authenticated identity is not authorized |
| 404 | Missing or inaccessible resource; no cross-organization existence disclosure |
| 409 | Idempotency, version, or lifecycle conflict; reread current state |
| 413 | Encoded request/frame exceeds configured byte limit, or bundle expansion returns `response_too_large` |
| 415 | JSON endpoint requires `application/json` |
| 422 | Structurally invalid input, missing required header, or invalid content |
| 428 | A required concurrency precondition was omitted on a surface that requires it |
| 429 | Rate limited; retry with backoff and original mutation identity |
| 503 | Required IAM/database/provider authority is unavailable; preserve unsent work |

The response `X-Request-ID` is useful for correlation; it is separate from the error JSON. A draft conflict can instead return the current `Draft` as its 409 response, described below.

## Sessions

| Method and path | Input | Successful result |
| --- | --- | --- |
| `POST /auth/login` | `{"slt":"oac_..."}` | 200 application session |
| `POST /auth/refresh` | `{"refresh_token":"ort_..."}` | 200 rotated application session |
| `POST /auth/logout` | `{"token":"ort_..."}` | 204 revoked family; an `oat_` token revokes only itself |
| `GET /auth/me` | Bearer and `X-Org-ID` headers | 200 current actor, organization, principal/session UUIDs, role and effective scopes |

The three session mutations require an idempotency key but no separate Bearer or organization header. Their maximum JSON body is 16 KiB. Login and refresh return `access_token`, `refresh_token`, `token_type: "Bearer"`, `expires_in`, `scope`, `actor: {type,id}`, and `organization_id`, with `Cache-Control: no-store` and `Pragma: no-cache`. Persist both tokens atomically; refresh rotates the current refresh token. Never log session bodies. The client-side webhook URL is local relay configuration and is absent from every backend session input.

## Conversations

`GET /conversations` returns `{items: Conversation[], next_cursor}` for the authenticated actor. `POST /conversations` accepts `{"participant_ids":["other-carbon","helper:organization"]}` and returns a conversation with status 201. DM adds the creator, deduplicates IDs, resolves active IAM membership projections, and requires 2–100 total unique participants. An offline recipient is supported after their membership arrives through their sign-in or a verified IAM webhook. IAM currently exposes no app-scoped arbitrary-member lookup; a recipient never supplied to DM returns 422 explaining that they can sign in. The exact participant set resolves to a single conversation in that organization, including when a different key requests the same set. A conversation contains `id`, `org_id`, typed `participants`, nullable `last_message`, `created_at`, and `updated_at`.

The authenticated actor must participate in a conversation to access its history, messages, drafts, receipts, or bundles. Organization membership alone is not conversation access. Conversation listing has no implicit organization-wide administrator bypass.

## Messages, replies, attachments, and metadata

| Method and path | Behavior |
| --- | --- |
| `GET /conversations/{conversation_id}/messages` | List newest messages; `include_bundled_members=true` includes originals hidden by bundle display messages |
| `POST /conversations/{conversation_id}/messages` | Durably persist and queue content; return the message with status 202 |
| `GET /conversations/{conversation_id}/messages/{message_id}` | Read the latest message, including a deletion tombstone |
| `PATCH /conversations/{conversation_id}/messages/{message_id}` | Original sender replaces content with exact `If-Match` version and idempotency key; return 200 |
| `DELETE /conversations/{conversation_id}/messages/{message_id}` | Original sender publishes a content-free tombstone with exact `If-Match` version and idempotency key; return 200 message |

A message creation or full replacement can combine any supported content:

```json
{
  "text": "The recording and notes are ready.",
  "attachments": [{
    "permanent_url": "https://files.example.test/notes.pdf",
    "name": "notes.pdf",
    "content_type": "application/pdf",
    "size": 2048
  }],
  "voice": {
    "permanent_url": "https://files.example.test/recording.ogg",
    "content_type": "audio/ogg",
    "duration_milliseconds": 42000
  },
  "voice_transcript": "Here are the meeting notes.",
  "metadata": {"topic":"planning","source":{"kind":"agent"},"labels":["meeting"]}
}
```

`metadata` is an arbitrary JSON **object**, always returned even when `{}`; its nested JSON values are preserved. It can accompany every message kind, a bundle display message, or a draft. Metadata alone does not satisfy message content requirements. `reply_to_message_id` optionally references an existing message in the same conversation. Cross-conversation or nonexistent targets fail; editing a message to reply to itself fails. `sender_id` is optional routing information and cannot impersonate another actor.

DM stores attachment links and declared metadata only. It does not upload, fetch, scan, transcribe, proxy, or exchange the links. There is no temporary-URL endpoint and no special file-provider requirement. An attachment object has required `permanent_url` and optional `name`, `content_type`, and `size`. A voice object has the same fields plus required positive `duration_milliseconds`; `voice_transcript` belongs alongside `voice`. GIF content is `{"provider_id":"...","url":"https://...","preview_url":"https://...","title":"..."}` with preview/title optional.

| Content or transport | Limit |
| --- | --- |
| Message text, draft text | 100,000,000 Unicode scalar values each |
| Voice transcript | 100,000,000 Unicode scalar values independently of text |
| Attachments plus optional voice item | 100 total |
| Declared size of each attachment/voice item | 5 GiB, 5,368,709,120 bytes; DM does not transfer the file |
| Voice duration | 1–172,800,000 milliseconds, up to 48 hours |
| Attachment link | HTTPS, host required, no username/password, at most 8,192 encoded bytes |
| Attachment name | 1–1,024 characters when supplied |
| Declared content type | 1–255 bytes when supplied |
| Default encoded HTTP body/WebSocket text frame | 128 MiB, 134,217,728 bytes |
| Maximum configurable encoded body cap | 3 GiB through `DM_MAX_HTTP_BODY_BYTES`; increase only with adequate process memory |

Logical character counts differ from encoded transport bytes. A large Unicode body, escaped JSON, or combined text and transcript can exceed the default byte cap while each text field satisfies its logical limit. Deployments that require those extremes must explicitly increase `DM_MAX_HTTP_BODY_BYTES`; oversized input receives 413 or the corresponding socket frame-limit closure. Bodies are parsed in memory, so a larger cap requires corresponding memory capacity. Auth and IAM webhook routes keep their smaller independent caps.

A stored message has stable `id`, `conversation_id`, typed `sender`, conversation `sequence`, `status`, `created_at`, `version` initially 1, nullable `deleted_at`, metadata, optional reply target, content, receipt timestamps, optional failure reason, and optional bundle reference. Edits increase `version` without changing message ID or original sequence. PATCH is a **full content replacement**: omitted optional content is cleared and omitted metadata becomes `{}`. Send the complete desired content, not a JSON merge patch. Use `If-Match: "1"` after reading version 1. A stale version returns 409. Deletion clears the public content/metadata/reply and sets `deleted_at`; it does not remove the stable message record. Deleted messages cannot be edited or resurrected. Retries with the original idempotency key do not create another revision.

## Receipts and durable delivery

`POST /conversations/{conversation_id}/messages/{message_id}/receipts` accepts `{"status":"delivered","device_id":"my-device"}` or `read` and returns the latest aggregate message. `device_id` is a stable nonempty identifier of at most 255 characters without controls. Receipts are monotonic: reading implies delivery; a later delivered receipt cannot downgrade read. Every recipient actor must have at least one qualifying device receipt before the aggregate reaches delivered/read. The sender's own delivery stream does not count as a recipient receipt.

`waiting` and a retryable local failure belong in the client outbox before durable server acceptance. `sent` means DM committed the message and durable delivery records. `delivered` and `read` are recipient acknowledgments. `failed` means delivery has stopped retrying; a transient network failure should remain pending rather than be reported as final failure.

A transport ACK acknowledges durable processing of an actor-stream envelope; it does not by itself mark the message read or create a device receipt. Send receipts separately. Messages remain durable history after transport delivery retention ends. Use history synchronization when a newly installed device needs older conversation content.

## Bundles

`POST /conversations/{conversation_id}/bundles` accepts `{"message_ids":["..."],"display_message":{...MessageCreate...}}`, requires an idempotency key, and returns 201. Only a Silicon may create a bundle. It contains 1–100 unique existing message IDs from the same conversation. Members remain stored and receive `bundle: {id, role:"member"}`; the new display message has role `display`. Bundling is non-destructive. A bundle display message supports ordinary metadata, reply targets, and all supported message content combinations.

`GET /conversations/{conversation_id}/bundles/{bundle_id}` returns the bundle, display message, and `original_messages`. Expanded message payloads have a 128 MiB aggregate budget, with an exception for one individually legal oversized message when all other message payloads total at most 16 MiB. Larger expansions return 413 `response_too_large` before accumulating the entire bundle; retrieve originals individually by their message IDs. Normal message listing hides bundle members unless `include_bundled_members=true`. The response exposes `id`, `conversation_id`, `original_message_ids`, `display_message`, typed `created_by`, and `created_at`.

## Drafts

Drafts are private to one actor and conversation, synchronized across devices:

- `GET /conversations/{conversation_id}/draft` returns the current draft or 404.
- `PUT /conversations/{conversation_id}/draft` creates or fully replaces a draft. Omit `If-Match` or use 0 only when no draft exists; otherwise supply its exact positive version.
- `DELETE /conversations/{conversation_id}/draft` clears the caller's draft and returns 204.

Draft input uses `message_content` for text, and supports `attachments`, `voice`, `voice_transcript`, `gif`, `metadata`, and `reply_to_message_id`. An empty draft is valid. Draft output adds `conversation_id`, `actor_id`, `version`, and `updated_at`. Successful writes increment the version. Version counters survive deletion and automatic clearing, so a recreated draft receives a newer token instead of reusing version 1. Always use the returned version; creation still uses If-Match 0. Conflicting writes return 409 with the current draft when it still exists, or an error envelope if it was deleted after the observed version. Keep local content and resolve that conflict explicitly. Sending a message clears the actor's matching draft only when its canonical content, including metadata and reply target, matches; a newer or different composition remains.

## Presence and GIF discovery

`GET /presence/{actor_id}` returns authorized presence: `actor_id`, `availability` (`online`/`offline`), optional `activity`, and `last_seen_at`. Activities are `typing`, `recording_voice`, `transcribing_voice`, `uploading_file`, and `searching_gifs`; a null activity clears transient work while preserving online state. Live clients publish activities through WebSocket `presence` frames. Availability derives from active connection leases; disconnect/lease expiry updates last-seen state.

`GET /gifs/trending` returns safe Giphy results. `GET /gifs/search?q=...` accepts a nonempty search of at most 50 characters without controls. Both return `{items: Gif[]}`. `GET /gifs/recent` returns the authenticated Carbon's last 20 distinct used GIFs; Silicon recent history is not supported. Sending a GIF records usage. GIF discovery requires a configured Giphy API key; external provider failure is surfaced instead of returning fabricated results.

## WebSocket protocol version 2

```text
GET /api/v1/ws?org_id=your-org&device_id=my-device&actors=actor-id
Authorization: Bearer oat_REDACTED
```

Repeat the `actors` query parameter rather than using comma-separated IDs. `org_id` and `device_id` must each occur exactly once. IAM must authorize every requested actor; the current adapter represents only its authenticated principal. A relay serving multiple accounts opens a separate authenticated connection for each account. Pass the DM test key header when selecting a test environment. Persist `ready.testing_generation` and send it as the optional `testing_generation` query parameter on reconnect. Production returns null. If the generation changed after a test clean or lifecycle change, clear old local cursors and archive the old inbox before replay. A missing/mismatched test generation makes the backend start at sequence 0 and clamp resume requests to 0, so a stale cursor cannot hide new messages.

The backend immediately sends `ready` with `protocol_version: 2`, `connection_id`, `actors`, and `acknowledged_through` keyed by actor ID. Client-to-server frames are:

| Type | Fields beyond `type` | Meaning |
| --- | --- | --- |
| `pong` | `ping_id` | Immediately echo the server ping ID |
| `ack` | `actor_id`, `through_sequence` | Cumulatively acknowledge the highest contiguous durably processed delivery |
| `resume` | `actor_id`, `after_sequence` | Replay after durable local cursor; 0 starts the retained stream |
| `presence` | `actor_id`, nullable `activity` | Update transient activity |
| `receipt` | `actor_id`, `conversation_id`, `message_id`, `status`, `device_id` | Record delivered/read receipt for this device |
| `send_message` | `actor_id`, `org_id`, `conversation_id`, `idempotency_key`, `message` | Send ordinary MessageCreate content over the connection |

Server-to-client frames are:

| Type | Fields beyond `type` | Handling |
| --- | --- | --- |
| `ready` | Protocol/version/actor/cursor fields above | Initialize or resume local streams |
| `ping` | `ping_id` | Reply immediately; never ACK it |
| `message_accepted` | `idempotency_key`, `message` | Ephemeral durable-send confirmation; never transport-ACK it |
| `receipt_recorded` | `message_id`, `status` | Ephemeral receipt confirmation; never transport-ACK it |
| `message` | `delivery_id`, `actor_id`, `delivery_sequence`, `message` | Durably apply creation/revision/tombstone and ACK contiguous progress |
| `receipt` | `delivery_id`, `actor_id`, `delivery_sequence`, `message_id`, `status` | Durably apply monotonic aggregate status and ACK progress |
| `error` | `code`, `message`, `recoverable` | Handle the failed command while preserving retryable work |

Delivery IDs are stable across retries. Actor delivery sequences are separate from conversation message sequences. Every participant, including sender devices, receives message/revision/tombstone deliveries. Deduplicate by `delivery_id`; **upsert by message ID and content version**, so an edit does not become a second visible message. A replay of an older delivery may carry the current message revision; ignore stale content versions and do not resurrect a deletion. Do not ACK a gap or an envelope that has not been durably processed. The client/relay's exact-request acknowledgment belongs to its local command API; backend WebSocket confirmations use the schemas above.

A ping is sent every 30 seconds. Only a pong with the matching current ping ID renews the heartbeat. Two minutes without a valid pong closes with `4000`, reason `heartbeat-timeout`. Heartbeats are not persisted, ACKed, or sequenced. IAM revalidation closes revoked authority with `4001`/`authorization-revoked` and unavailable authority with `1013`/`authorization-unavailable`. Test cleanup, deletion, or key rotation also disconnects stale sessions. Reconnect with current credentials, the current test key, and durable local cursors.

## Testing environment API

Detailed setup and lifecycle semantics are in [testing environments](../testing-environments.md). The same ordinary routes and protocol operate inside an empty DM environment paired exclusively with IAM test data.

| Method and path | Authority | Result |
| --- | --- | --- |
| `GET /testing-environments` | Production member | `{items:[...]}`; optional `include_deleted=true` |
| `POST /testing-environments` | Production member | 201 environment plus `root_key` |
| `GET /testing-environments/{environment_id}` | Production member of owning org | Non-secret metadata |
| `PATCH /testing-environments/{environment_id}` | Creator or org admin/owner | Updated `name`/`description` |
| `GET /testing-environments/{environment_id}/key` | Creator or org admin/owner | `{environment_id,root_key}` |
| `POST /testing-environments/{environment_id}/rotate-key` | Creator or org admin/owner | Environment plus fresh key; previous key revoked |
| `POST /testing-environments/{environment_id}/clean` | Matching DM key alone, or creator/admin production session | 204; all test data cleared, environment and key retained |
| `DELETE /testing-environments/{environment_id}` | Creator or org admin/owner | 204; key revoked, data recoverable for 30 days |
| `POST /testing-environments/{environment_id}/restore` | Creator or org admin/owner | Retained data restored with fresh root key |

Every testing-environment mutation above requires `Idempotency-Key`, including key-only clean. GETs do not require it. The backend encrypts its exact replay journal, so repeating the same request and key returns the original result, including the same issued key; changing the target or body with that key returns 409. All lifecycle responses use `Cache-Control: no-store`.

Creation input is `name`, optional `description`, `iam_environment_id`, `iam_environment_key`, `iam_app_id`, and `iam_app_secret`. A dedicated IAM test callback can also supply `iam_webhook_secret` and `iam_webhook_key_version` together; the secret contains 32–512 visible ASCII characters and the version is positive. Both omitted inherit the backend signer. Overrides are encrypted and never returned in metadata. Import the existing canonical DM app through the IAM CLI first and use its fresh test-only credential; production IAM credentials cannot back the environment. Names contain 1–128 characters without controls; descriptions contain at most 4,096 characters. Root keys are exactly 32 alphanumeric characters. Environment output includes its UUID, owning organization, typed creator identity fields, IAM binding IDs, lifecycle status/version, creation/activity timestamps, and nullable deletion/purge timestamps. Secrets are returned only by explicit create/key/rotate/restore operations.

Fifteen days without activity automatically soft-delete an environment. Thirty days after deletion its data is permanently purged. Cleaning, rotation, deletion and restoration fence concurrent requests and invalidate stale sessions. Recovery preserves retained data but issues a new DM key. Each test environment has its own schema within a separate shared testing database, and each test data row carries that environment ID. Production uses its own database.

## IAM callback and operational endpoints

These routes use the backend origin directly, outside `/api/v1`:

- `POST /webhook/`: IAM callback, maximum 1 MiB. Require exactly one `X-Silicon-IAM-Event-ID`, `X-Silicon-IAM-Timestamp`, `X-Silicon-IAM-Key-Version`, and `X-Silicon-IAM-Signature`. A bounded redacted test-key hint selects candidate verifiers only; the SDK then verifies exact raw bytes and the signed production/test binding before initializing any runtime or writing state. Every active DM environment paired with the verified IAM environment receives the invalidation; unrelated test planes and production do not. DM commits deduplicated event receipts before returning 204, then revalidates affected live authority. Invalid signatures do not change state. Test root keys/raw envelopes are never persisted. See the [IAM guide](../iam.md).
- `GET /live`: unauthenticated 204 when the process serves HTTP.
- `GET /ready`: unauthenticated 204 when database connectivity, migration checksums, and runtime access are ready; otherwise 503.

There are no client-callable internal delivery workers, OBO endpoints, attachment-upload endpoints, or temporary-link exchanges. The Rust client and CLI expose the public operations above.
