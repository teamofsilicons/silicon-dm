# Rust client guide

`silicon-dm-client` provides typed HTTP operations for messages, conversations,
history, permissions, receipts and presence. Ting owns incoming delivery. The
stateless client opens no incoming connection, saves no credentials and starts no
process. The optional [runtime](runtime.md) stores profiles, configures Ting's
installed daemon directly, and runs the outgoing command relay used by the CLI.

This guide describes the Ting migration in this checkout. Use a matching released
client/server build, or a path dependency on `crates/client` while developing.
The [HTTP contract](../api/README.md) and [contract discovery](../contracts.md)
describe the server endpoints.

## HTTP login and sending

`Client::new` accepts a DM origin or `/api/v1` base. HTTPS is required except on
loopback; credentials, query strings and fragments in base URLs are rejected.
Redirects are disabled. Ordinary HTTP calls time out after 45 seconds.

```rust
use silicon_dm_client::{Client, MessageCreate, PageRequest};
use uuid::Uuid;

let public = Client::new("https://backend.dm.teamofsilicons.com")?;
let login_key = Uuid::new_v4().to_string(); // Persist before sending.
let tokens = public.login(&dm_slt, &login_key).await?;
let dm = public.with_auth(&tokens.access_token, &tokens.organization_id);
let identity = dm.me().await?;

let content = MessageCreate {
    text: Some("Hello".into()),
    ..MessageCreate::default()
};
let send_key = Uuid::new_v4().to_string(); // A new operation gets a fresh key.
let message = dm.send_message(&conversation_id, &content, &send_key).await?;
let history = dm.messages(&message.conversation_id, &PageRequest::default(), false).await?;
```

The SLT and conversation address come from your application. After an uncertain
response, retry the **same** operation with its original key and identical input.
Do not generate another key inside a retry loop. Save replacement tokens atomically
when using `refresh(refresh_token, key)`; `logout(refresh_token, key)` revokes that
family. `Tokens` deliberately has no `Debug` implementation.

## Explicit delivery consent

An authenticated recipient explicitly enrolls its own DM grant in Ting:

```rust
let registration_key = Uuid::new_v4().to_string();
let subscription = dm.register_delivery(&registration_key).await?;
```

DM performs the IAM OBO exchange for this recipient. This call neither logs the
recipient into Ting nor configures a destination. Retry a known registration with
its original key. An uncertain upstream outcome is reported rather than silently
re-enrolling; follow the returned recovery guidance. Signing in or reconnecting
must not automatically restore a revoked grant.

## Receiving raw Ting batches

Configure the consumer's local endpoint directly with Ting; see
[runtime setup](runtime.md). Ting hooks cover every eligible app for the selected
recipient and organization. They are not filtered to DM. Your endpoint receives
raw `{"tings":[...]}`, not a DM `type`/`data` envelope.

Authenticate the configured callback secret and `Ting-Webhook-Id` first. Load the
hook's trusted recipient binding from local configuration; never build it from
untrusted callback JSON. The local Ting payload omits `for`. Route all applications
by type and deduplicate durably by Ting ID and DM `delivery_id` where applicable.

```rust
use silicon_dm_client::ting::{HydratedTingItem, TingReceiverContext};

// trusted_hook_binding is persisted when this exact hook is configured.
let receiver: TingReceiverContext = trusted_hook_binding;
let outcomes = dm.hydrate_ting_batch(&raw_callback_body, &receiver).await?;
```

The helper verifies fresh DM identity and discovery, validates recognized
`<app_id>.sync.changed` references, and fetches current messages through normal
DM permissions. Its outcomes require explicit handling:

| Outcome | Consumer responsibility |
| --- | --- |
| `Message { reference, message }` | Apply the current message or deletion tombstone; preserve `reference.isi` when routing. |
| `Inaccessible { reference }` | Handle DM's 403/404 without exposing cached content as newly authorized. |
| `Failed { reference, error }` | Keep the failure visible and retry appropriate transient failures. |
| `Skipped(TingItem::Unrelated { .. })` | Dispatch to the appropriate app handler; DM has not accepted it. |
| Other `Skipped` outcomes | Handle rejected or stale-generation input explicitly. |

`ting::validate_batch(raw, receiver, identity)` performs validation without I/O;
its identity argument must come from authenticated DM `me()`. Neither helper
persists work, transforms the message schema, dispatches another callback, nor
sends Ting or DM acknowledgements.

DM references retain DM's organization handle, while full Ting inbox objects
carry Ting's canonical organization ID in their outer `org_id`. Before validating
those objects, bind the trusted receiver with
`receiver.with_ting_organizations(&authenticated_ting_orgs)?`, using the same
recipient's authenticated `/v1/orgs` response. The helper resolves exactly one
matching ID or handle and rejects missing, ambiguous or malformed mappings.
Never supply organizations from callback JSON. Full inbox objects require this
canonical binding; local webhook items omit the outer organization and continue
to use the authenticated hook binding and exact DM reference organization.

Ting accepts **the entire batch** only when the endpoint returns HTTP 204. Persist
acceptance according to the generic consumer's contract before responding; do not
return 204 merely because the DM subset succeeded. An old DM
`{"type":"ack","data":...}` response, or Silicon event-result JSON, is not a
Ting ACK. Ting manages delivery retries and replay.

## Initial history and recovery

Ting notifications are references, not a complete initial history. Initialize a
new consumer with `sync_reset()` to capture a boundary, load accessible
`conversations()` and `messages()` snapshots, then resume `sync()` from that
boundary. Capturing the boundary first preserves updates that arrive while the
snapshot is loading.

```rust
use silicon_dm_client::SyncRequest;

let anchor = dm.sync_reset().await?;
// Load and persist all accessible conversation/message snapshot pages here.
let request = SyncRequest {
    cursor: Some(anchor.cursor),
    limit: Some(100),
    reset: false,
};
let page = dm.sync(&request).await?;
// Fetch current messages for page.events; commit applied work and page.cursor
// together. Continue with that cursor while page.has_more is true.
```

Retain the returned cursor even on an empty final page. Resume the stored cursor
after reconnect or recovery; never substitute a Ting sequence or the largest
sequence observed in a batch. Cursors bind actor, org, environment and generation
and expire after 24 hours. On `Error::sync_reset_required()`, repeat the
boundary/snapshot/resume procedure. The SDK performs no background polling or
automatic snapshot reset. Consumers must scope their own state and deduplication
to the same identity and environment.

## DM receipts and presence

Ting's transport acceptance/read state is separate from DM message state. Send
Delivered only after the intended recipient application accepts the message, and
Read only when that recipient actually reads it. Fetching a reference, receiving
a sender copy, or applying a deletion tombstone does not itself justify a receipt.

```rust
use silicon_dm_client::ReceiptStatus;

// Run only after this recipient has accepted the non-deleted message.
dm.record_receipt(&message.conversation_id, &message.id,
    ReceiptStatus::Delivered, &stable_device_id).await?;
let lease = dm.renew_presence(&stable_device_id, None).await?;
// Renew according to lease.lease_expires_at while the device remains active.
dm.close_presence(&stable_device_id).await?;
```

`presence(actor_id)` reads presence; `renew_presence(device_id, activity)` and
`close_presence(device_id)` use HTTP. Presence leases are independent of Ting
connectivity. Keep the device ID stable across restarts.

## Other operations and message shape

Messages retain their DM schema, content, attachments, transcripts, replies,
bundles, history and identifiers. A message code is scoped by conversation.
Groups, drafts, GIFs, sandbox management and normal send/edit/delete operations
continue through HTTP; see the [API guide](../api/README.md).

Optional ISI addresses belong in `MessageCreate.sender_id` and `recipient_id`,
for example `deliberate@si:cos`. They do not create another IAM principal or
change conversation permissions. Canonical identity stays in `Message.sender`;
route the validated Ting reference's optional `isi` in the receiving application.
Edits cannot change the original route.

`PageRequest` takes the server's opaque `next_cursor` and a limit of 1–100. A null
`next_cursor` ends ordinary listing; do not infer completion from page length.
`Error::Api` retains status, code, body, request ID and retry guidance.
`Error::retryable()` classifies failures but performs no retry. Inspect current
state after uncertain optimistic-concurrency writes before resubmitting.

## Sandboxes and retired sockets

Use `with_test_key` with the DM audience's IAM test app secret and verify the
selected environment through `iam()`. Bind sandbox writes and hydration with
`with_testing_generation(generation)` after validating a positive generation
from discovery. A production token is not a test credential. The receiver
binding must match the environment and generation exactly; a clean or restore
requires explicit setup for the new generation. Ting login uses its own verified
test credentials, described in [runtime setup](runtime.md).

`connect`, `connect_with_generation` and `prewarm_shared` remain callable for
source compatibility but immediately return `Error::Configuration` with actionable
Ting migration guidance. They never open a socket. Backend DM WebSocket routes return HTTP 410.
There is no DM receive fallback; Ting owns connections and delivery ACKs.

Rust clients remain ordinary Cargo dependencies. `check_update()` only reports
release information; it never changes the consuming project. Honeycomb manages
CLI installation and updates.
