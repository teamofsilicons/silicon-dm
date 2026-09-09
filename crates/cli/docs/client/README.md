# Rust client guide

`silicon-dm-client` is a stateless, typed client for public Silicon DM operations.
Its default HTTP/WebSocket client does not call IAM directly, hold IAM
application secrets, persist credentials, or start a daemon. Its protocol types
are independent of the backend crate. Enable the optional `runtime` feature for
the [durable relay and updater](runtime.md), using a caller-selected private
state directory. The stateful `dm` CLI uses that same SDK runtime.

## Installation and configuration

Add `silicon-dm-client = "0.3"` to your application's Cargo manifest to use the
published package. For development against this checkout, depend on
`crates/client` by path.

`Client::new` accepts a DM origin such as
`https://backend.dm.teamofsilicons.com`, or its `/api/v1` base. It appends
`/api/v1/` for an origin. HTTPS is required except on loopback hosts, where HTTP
supports local development. URLs containing credentials, a query or a fragment
are rejected. Redirects are disabled. Ordinary HTTP calls time out after 45
seconds. Credentials never appear in a `Client` debug representation.

```rust
use silicon_dm_client::{Client, MessageCreate, PageRequest};
use uuid::Uuid;

let dm = Client::new("https://backend.dm.teamofsilicons.com")?;
// Obtain the short-lived token from the actor, not their password.
let tokens = dm.login(&short_lived_token, &Uuid::new_v4().to_string()).await?;
// Persist tokens privately if your application needs persistence. Configure
// callbacks after login through LocalRuntime, or own the WebSocket directly.
let dm = dm.with_auth(&tokens.access_token, &tokens.organization_id);
let identity = dm.me().await?;
let conversation = dm.create_conversation(
    &[other_actor_public_id], &Uuid::new_v4().to_string()
).await?;
let mut content = MessageCreate::default();
content.text = Some("Hello".into());
content.metadata.insert("task_id".into(), serde_json::json!("task-42"));
let retry_key = Uuid::new_v4().to_string();
let message = dm.send_message(conversation.id, &content, &retry_key).await?;
let history = dm.messages(conversation.id, &PageRequest::default(), false).await?;
```

The example's `short_lived_token` and `other_actor_public_id` come from your
application; no token is embedded in documentation. For production deployments,
use IAM-issued credentials authorized for the selected organization. Organization
and actor authorization remain enforced by the backend. There is no OBO flow.

## Authentication and credential lifecycle

`login(slt, idempotency_key)` sends only `{slt}` to DM's login
endpoint. The backend performs the official IAM client exchange with its own
application credentials. The returned `Tokens` contains `access_token`,
`refresh_token`, `token_type`, `expires_in`, `scope`, `actor`, and
`organization_id`. The actor shape is `{type, id}`. Tokens deliberately do not
implement `Debug`.

`with_auth(token, organization_id)` builds an authenticated client without disk
I/O. `refresh(refresh_token, key)` returns the replacement token pair; save it
atomically before using it. `logout(token, key)` revokes the supplied token;
passing the refresh token revokes its family. Login, refresh and logout require
retry-safe idempotency keys. Reuse the same key and identical payload after an
uncertain response. Credentials may not authorize every operation: the server
returns its actual authorization decision; a client never invents permissions.

`me()` returns the current actor, organization, principal and session identifiers,
organization role and disclosed capabilities. A client can represent a list of
actors on a WebSocket only if the backend authorizes every actor.

`iam()` returns the backend's public `IamInfo` (`app_id`, `iam_base_url`,
`api_base_url`) without an authenticated session. The optional runtime's
`login_status(profile, test).await` verifies and refreshes a persisted login;
`webhook(profile, test, Some(&url))` configures a callback after login and
`webhook(profile, test, None)` unhooks it. See [runtime](runtime.md).

## ISI message routing

Set `MessageCreate.sender_id = Some("compose@writer:tos".into())` to send with
an ISI, or `recipient_id = Some("deliberate@cos:tos".into())` to address one.
The sender must be the authorized silicon account and the recipient must be a
conversation participant. Create conversations using canonical IDs (`cos:tos`).
The returned `Message.sender.id` stays canonical; `Message.content.sender_id`
and `Message.content.recipient_id` preserve the routing addresses. They survive
history, WebSocket and callback delivery, replies when supplied, bundles and
edits. Edits cannot change routing. Use a new idempotency key for a new route.
ISI does not change conversation visibility or create another IAM principal;
your receiving application dispatches the optional ISI. A socket's `actor_id`
and subscription IDs remain canonical, with the prefixed sender in its message.

## Operations

| Area | Methods | Important inputs |
| --- | --- | --- |
| Identity | `login`, `refresh`, `logout`, `me` | SLT/token and original retry key |
| Conversations | `conversations`, `create_conversation` | Page request; participant public IDs and retry key |
| Messages | `messages`, `message`, `send_message`, `edit_message`, `delete_message` | Conversation/message UUIDs, content, observed version, retry key |
| Receipts | `record_receipt` | Delivered/read state and stable device ID |
| Drafts | `draft`, `put_draft`, `delete_draft` | Full content; version zero for create or observed version for replacement; versions are retained across deletion |
| Bundles | `create_bundle`, `bundle` | 1–100 message UUIDs and a display message; Silicon authority |
| Presence | `presence`; `ClientFrame::Presence` over a socket | Actor public ID; activity or null |
| GIFs | `gifs` with `GifList` | Trending, search query, or recent |
| Sandbox management | `create_test_environment`, `test_environments`, `test_environment`, `update_test_environment`, `test_environment_key`, `rotate_test_environment_key`, `clean_test_environment`, `delete_test_environment`, `restore_test_environment` | Production owner/creator authority and stable mutation keys; clean may use the root key |
| Realtime | `connect`, `connect_with_generation` | Authorized actor IDs, stable device ID, last known sandbox generation |
| Local relay | `relay::RelayClient` | Local relay URL and its private local bearer |
| Optional runtime | `runtime::LocalRuntime::{login,start,start_with,run,client,store}` | Explicit state directory, local callback, and daemon executable; feature `runtime` |

Every public message and draft includes `metadata`, a JSON **object** defaulting
to `{}`. Numbers, booleans, strings, nulls, objects and arrays can be values within
that object. The root metadata value must remain an object. Sending, replay,
history, drafts, bundle display messages and revisions retain it. A reply sets
`reply_to_message_id`. A metadata-only revision still sends the complete content
with `edit_message`; omitted fields are removed by full replacement.

Attachments use `Attachment { permanent_url, name?, content_type?, size? }` and
are supplied links. Plain URLs inside text need no special treatment. DM does
not upload or fetch files. Attachments can be the entire message. Voice includes
`duration_milliseconds`; optional `voice_transcript` is supplied by the caller.
Historical voice rows may contain a null duration. GIFs contain `provider_id`,
`url`, optional `preview_url`, and optional `title`.

Message revisions keep the same message ID and increment the version. Deletion
returns a tombstone with `deleted_at`; update your view to remove its content.
Check both ID and version when processing events. Transport deduplication uses
the distinct `delivery_id`, not just `message.id`.

## Errors, pagination and retries

`Error::Api` preserves status, stable code, human-readable message, full response
body, `X-Request-ID`, and `Retry-After` when present. On a draft conflict the full
body can contain the current server draft. Do not replace that draft silently:
read it, merge intentionally, and resubmit its new observed version.

`Error::retryable()` identifies transport failures, HTTP 408/429 and server
failures. It does **not** retry automatically. For a retry-safe mutation, persist
its payload and key before sending, then retry both unchanged. If an operation
has only optimistic concurrency, an uncertain successful write may later return
a conflict; inspect current server state before deciding what to do. Idempotency
does not mean a new key can be substituted after a timeout.

`PageRequest` accepts `cursor` and a limit from 1 to 100. Copy `next_cursor` from
one response into the next request. A null cursor ends traversal. Message pages
are newest-first. `include_bundled_members=true` expands original members in
history; bundle details separately include the originals.

## Test environments

Manage environments with the production login. `TestEnvironmentCreate` contains
name, optional description, IAM test environment ID/key and the imported IAM
test application's ID/secret. The DM backend requires test IAM credentials and
cannot fall back to production IAM. Creation yields a fresh DM environment and
root key; store the root key privately.

```rust
let sandbox = Client::new(dm_base)?.with_test_key(dm_test_root_key)?;
let tokens = sandbox.login(&iam_test_slt, &login_key).await?;
let sandbox = sandbox.with_auth(tokens.access_token, tokens.organization_id);
let page = sandbox.conversations(&PageRequest::default()).await?;
```

The key is carried in `X-Testing-Environment-Key` on every selected HTTP request
and WebSocket upgrade. A key does not turn a production actor into a test actor:
the selected IAM sandbox still authenticates the actor. Use `without_test()`
with a production-authenticated client for management. Clean, rotate and restore
change the environment generation. See [realtime](realtime.md) for cursor reset
requirements and [the test guide](../testing-environments.md) for lifecycle rules.

## Updates

`check_update()` reads the latest published stable client version from crates.io
and returns `UpdateInfo`. It stores no timestamp and changes no application
files. The optional runtime also provides `UpdatePolicy` (enabled by default,
one check per hour) and `updates::after_command` to update the SDK dependency and
rebuild an explicitly selected Cargo application after its command finishes.
Callers own and may persist the policy; setting `enabled=false` opts out. A
linked library cannot replace code already running, so successful rebuilds
report that application restart is required. The CLI uses the shared runtime's
separate installed-executable update path. See [runtime and updates](runtime.md).
