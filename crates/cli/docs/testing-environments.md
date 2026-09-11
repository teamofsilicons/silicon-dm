# DM testing environments

All DM JSON HTTP bodies use the [type/data wire envelope](wire-format.md).
The CLI adds it automatically around command input files.

DM exposes the same messages, conversations, receipts, drafts, bundles, presence,
GIF, login, refresh, logout, and WebSocket APIs in production and testing. A test
key selects an isolated dataset and a paired IAM testing environment. It never
turns a production IAM identity into a test identity.

## Credentials and identifiers

| Value | Purpose | Secret |
| --- | --- | --- |
| DM environment UUID | Lifecycle URLs and `dm --test UUID` | No |
| DM root key | `X-Testing-Environment-Key` request header | Yes |
| IAM environment UUID | Records and validates the IAM pairing | No |
| IAM environment root key | Mandatory on every outbound IAM test request | Yes |
| Imported DM IAM app ID | Same canonical application ID, such as `tos>dm` | No |
| Imported DM IAM app secret | Authenticates the application inside its IAM test plane | Yes |
| IAM application access/refresh tokens | Represent a particular Carbon/Silicon | Yes |

DM root keys are 32 characters drawn uniformly from ASCII letters and digits.
They are case-sensitive. A UUID is never accepted as a root key. The server
stores a digest for key lookup and authenticated-encrypted ciphertext for
explicit authorized retrieval. It similarly encrypts IAM keys, imported app
secrets, planned mutation keys, and replay responses that contain keys.

Possession of the DM root key grants access to that sandbox and allows cleaning
it. Ordinary messaging still requires a Carbon/Silicon access token from the
paired IAM plane and follows normal organization and conversation permissions.
The root key does not impersonate an actor. Signing in requires an IAM-issued
short-lived token for the imported DM application.

## Ownership and management permissions

A current production organization member can create an environment. The
production organization owns it, and the creator's actor ID and actor kind are
recorded. Organization members may list and inspect their organization's
non-secret environment metadata. The creator and current organization
administrators/owners may change metadata, retrieve the root key, rotate it,
delete the environment, and restore it. IAM supplies current organization
membership and role information; caller-provided role fields are not trusted.

Management requests use **production DM authentication**, including when the
environment is deleted. Lifecycle URLs never reinterpret a test token as a
production token. Cleaning additionally accepts the matching root key without
an actor access token. A key for one sandbox cannot clean another sandbox.

## Prepare the paired IAM environment

Use the installed official IAM CLI and its own help for IAM setup:

```sh
iam --org tos env create dm-manual --description 'Disposable DM exercise'
```

Keep the IAM UUID and root key. Establish a test Carbon/Silicon inside that IAM
environment. Production identities and app secrets do not automatically carry
into the test plane. Import the registered DM application with the IAM test
selector:

```sh
iam --test "$IAM_TEST_ID" app import 'tos>dm'
```

Keep the returned **test-only** application secret. DM validates the IAM
`environments.current()` result against the supplied IAM environment UUID and
validates the imported application credentials before creating the sandbox.
An incorrect IAM UUID, missing key, wrong application ID, or rejected app secret
fails creation. DM requires the imported canonical ID to match its configured
DM application ID. There is no production-IAM fallback.

IAM test webhooks use IAM's signed testing envelope and the paired IAM key.
The public receiver remains `POST /webhook/`. Verification of exact bytes,
signature, timestamp, key version, event identity, and test binding happens
before an event affects any data or connection. A verified callback reaches
every active DM environment paired to that exact IAM environment, application
ID, and current IAM root key. Other pairings are excluded.

By default the pairing inherits the production application's configured
webhook signer. If IAM's imported application has a distinct signer, provide
both optional create fields `iam_webhook_secret` and
`iam_webhook_key_version`. The secret must satisfy the IAM SDK's webhook-secret
contract and contain at least 32 bytes; the version must be a positive integer.
DM validates these values before creating the environment and encrypts the
paired secret. Never put a webhook secret in a root-key header.

## CLI workflow

First log into production DM with a DM-targeted IAM
short-lived token. CLI login also requires the local relay webhook URL; it is
stored locally and is not sent to the backend.

Prepare a private JSON file for pairing. Its fields are:

```json
{
  "name": "dm-manual",
  "description": "Disposable integration data",
  "iam_environment_id": "00000000-0000-4000-8000-000000000001",
  "iam_environment_key": "REPLACE_WITH_REAL_IAM_ROOT_KEY",
  "iam_app_id": "tos>dm",
  "iam_app_secret": "REPLACE_WITH_IMPORTED_TEST_APP_SECRET"
}
```

The UUID above illustrates the shape; supply your actual IAM environment UUID.
Keep the file private and do not commit it. Create the DM environment with a
stable idempotency key:

```sh
dm --idempotency-key dm-manual-create-001 env create --data /private/pairing.json
dm env list
dm env show "$DM_TEST_ID"
```

The CLI saves the returned DM root key privately. Another holder can import a
shared key from a private file or standard input:

```sh
dm env import-key "$DM_TEST_ID" --key-file /private/dm-root-key.txt
```

Obtain a DM short-lived token **inside the paired IAM test environment** and
log in with the same DM test selector:

```sh
dm --test "$DM_TEST_ID" login --webhook http://127.0.0.1:9000/dm-events --token-file -
dm --test "$DM_TEST_ID" whoami
dm --test "$DM_TEST_ID" conversations list
```

Use `dm --help`, `dm env --help`, and the individual command's `--help` for
complete syntax. Prefix normal commands with `--test "$DM_TEST_ID"`; omitting
the selector chooses the production profile. A missing local test key fails
with an instruction to import/retrieve it, rather than guessing a sandbox.

Management and recovery commands use the production profile:

```sh
dm env key "$DM_TEST_ID"                     # saves the key privately
dm env key "$DM_TEST_ID" --show              # explicitly prints the secret
dm --idempotency-key rotate-001 env rotate-key "$DM_TEST_ID"
dm --test "$DM_TEST_ID" --idempotency-key clean-001 env clean
dm --idempotency-key delete-001 env delete "$DM_TEST_ID"
dm env list --include-deleted
dm --idempotency-key restore-001 env restore "$DM_TEST_ID"
```

`env clean` requires `--test`. The same action without a selected sandbox is
rejected. Clean keeps the environment and its current root key but clears its
user data and resets delivery history. Clients must reset their cached data
and resume position after this lifecycle change. Connections carry a testing
generation so a cursor from an earlier generation cannot skip new messages.

Rotation immediately invalidates the previous root key and closes existing
sandbox sockets. Share/import the replacement key with other holders. Restore
always creates a fresh root key; deleted keys never become valid again.

## REST API

Use `/api/v1` as the API prefix. Management requests require:

```http
Authorization: Bearer <production-DM-access-token>
X-Org-Id: <production-organization-id>
```

All mutations require `Idempotency-Key`, containing 8–255 characters accepted
by DM's idempotency-key grammar. Use one stable value for retries of the exact
same operation. A key reused for a different request returns HTTP 409.

| Method and path | Body | Result |
| --- | --- | --- |
| `POST /testing-environments` | Pairing JSON above | 201; environment metadata plus `root_key` |
| `GET /testing-environments?include_deleted=true` | None | `{ "items": [...] }` |
| `GET /testing-environments/{id}` | None | Environment metadata |
| `PATCH /testing-environments/{id}` | Optional `name`, `description` | Updated metadata |
| `GET /testing-environments/{id}/key` | None | `{ "environment_id": "...", "root_key": "..." }` |
| `POST /testing-environments/{id}/rotate-key` | None | Metadata plus replacement `root_key` |
| `POST /testing-environments/{id}/clean` | None | 204 |
| `DELETE /testing-environments/{id}` | None | 204; recoverable deletion |
| `POST /testing-environments/{id}/restore` | None | Metadata plus new `root_key` |

Environment metadata contains `environment_id`, `organization_id`,
`creator_actor_id`, `creator_actor_kind`, `name`, `description`,
`iam_environment_id`, `iam_app_id`, `status`, `version`, `created_at`,
`last_activity_at`, `deleted_at`, and `purge_after`. Dates use RFC 3339. Names
must contain 1–128 characters without control characters. Descriptions permit
up to 4096 characters. PATCH leaves omitted fields unchanged; an empty string
clears the displayed description. Deleted environments cannot be renamed or
cleaned until restored. The active root key is available only through explicit
key-returning operations, not ordinary metadata/list responses.

Normal test operations add this header to the same production API URLs:

```http
X-Testing-Environment-Key: <DM-root-key>
Authorization: Bearer <paired-IAM-test-DM-access-token>
X-Org-Id: <test-organization-id>
```

For test login, send the root header and `{"type":"login","data":{"slt":"..."}}` to `/auth/login`;
no access token exists yet. Keep using the root header for refresh, logout,
REST operations, and the WebSocket upgrade at `/api/v1/ws`. Unknown, malformed,
rotated, or deleted keys return 401. Repeated root headers return 422. The API
never tries production after a test key fails validation.

Clients that persist or queue mutations should also send
`X-Testing-Environment-Generation: <positive-integer>` using the generation
captured when that operation was created. A mismatch returns 409 before the
operation executes. This prevents a delayed request from repopulating a newly
cleaned sandbox. The header is optional for direct REST clients; it requires
the root-key header, and malformed or repeated values return 422. Lifecycle
control routes do not apply this generation check, so exact retries of a clean
still return their original result.

The WebSocket upgrade accepts `testing_generation` in its query string and
the ready frame returns the current `testing_generation`. Missing or stale
generations reset the requested resume cursor to zero. Persist the returned
generation alongside the cursor; after a change, reset cached data and review
old pending writes before explicitly resubmitting them.

To clean using root authority only, send the root header and an idempotency
key to the matching lifecycle URL. No bearer or organization header is needed
for this one action. Production creator/admin authentication may also clean.

All API responses are marked `Cache-Control: no-store`; handle the explicitly
returned root keys as secrets. Request IDs support diagnosis without exposing
credentials.

## Stateless Rust client

The client never reads local profile files. The caller supplies the production
or test credentials explicitly:

```rust,no_run
use silicon_dm_client::{Client, PageRequest};

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let test = Client::new("https://backend.dm.teamofsilicons.com")?
    .with_test_key(std::env::var("DM_TEST_ROOT_KEY")?)?
    // Persist this value from the sandbox's authenticated ready frame.
    .with_testing_generation(std::env::var("DM_TEST_GENERATION")?.parse()?)?
    .with_auth(std::env::var("DM_TEST_ACCESS_TOKEN")?, "tos");
let page = test.conversations(&PageRequest::default()).await?;
println!("{} conversations", page.items.len());
# Ok(()) }
```

Use a separate production `Client::new(...).with_auth(...)` for
`create_test_environment`, `test_environments`, `test_environment`,
`update_test_environment`, `test_environment_key`,
`rotate_test_environment_key`, `delete_test_environment`, and
`restore_test_environment`. Each mutation takes an explicit idempotency key.
Use a client with `.with_test_key(...)` for normal sandbox commands and
`clean_test_environment(id, key)`. The key argument to a mutation is its
**idempotency key**; `.with_test_key(...)` is the **root key** selector.

Persist returned tokens and delivery cursors in your own application if
needed. The SDK itself is stateless. Reuse the original idempotency key and
body after timeouts or lost responses. Root-key mutations return the original
encrypted-journal response on retry, rather than generating another key.

## Lifecycle, retention, and recovery

An environment becomes inactive after 15 days without use. The API runs
maintenance at startup and every five minutes, soft-deleting eligible
environments. Valid key requests and authenticated realtime activity advance
`last_activity_at`. Passive production listings do not keep a sandbox alive.

Manual and inactivity deletion both remove the active root key, revoke
connections, and retain the schema for 30 days from deletion. Authorized
production callers can find deleted environments with `include_deleted=true`
and restore them before `purge_after`. Restore preserves retained messages,
drafts, and delivery history and issues a fresh key.

Once `purge_after` is reached, restore fails even if periodic maintenance has
not yet run. Maintenance destroys the entire sandbox's data/helper schemas,
control record, and mutation journal. Failed purge work remains marked as
`purging` and resumes on later maintenance; it cannot be restored or accessed.
An interrupted create remains non-accessible and can resume with the original
idempotency key. Stale incomplete creations are cleaned up after one hour.

Cleaning is deliberately different from deletion: it removes user data now,
preserves the environment/IAM pairing/current root key, and offers no data
recovery. Its destructive database transaction also records the clean's
mutation identity. If the response is lost, repeating that identity cannot
remove messages written after the first clean finished.

## Server configuration and database isolation

```dotenv
DM_TEST_DATABASE_URL=postgres://dm_test_owner:password@localhost:5432/silicon_dm_test
DM_TEST_DATABASE_MAX_CONNECTIONS=4
DM_TEST_KEY_ENCRYPTION_KEY=<base64-encoded-32-random-bytes>
DM_IAM_WEBHOOK_SECRET=<the-secret-registered-with-IAM>
DM_IAM_WEBHOOK_KEY_VERSION=1
```

Generate the encryption key once with `openssl rand -base64 32`, place it in
private service configuration, and back it up securely. Changing/loss of this
key prevents decryption of existing IAM pairings and root-key receipts. Key
rotation needs a deliberate re-encryption migration; it is not accomplished by
replacing the environment variable.

Omitting `DM_TEST_DATABASE_URL` disables test creation/routing. The test URL
must address a separate database. Startup compares the actual connected
database name and server address/port to reject a production/test alias.
Production's TLS verification rules also apply to the test URL.

Production stores only the test lifecycle/key control records. All sandbox
data lives in **one separate shared PostgreSQL database**, with a schema per
DM environment UUID and a separate helper schema. Every test data row includes
an immutable, checked `testing_environment_id`; a row cannot be relabeled as
another environment. The schema uses the same product migrations, constraints,
functions, indexes, and application methods as production. Queries use pools
whose connections have a fixed schema search path. Neither production nor
`public` is a fallback data schema, and no per-environment database is provisioned.

The test database role must be permitted to create and own schemas and their
objects, because creation, upgrade, and permanent purge operate there. Product
migrations are installed transactionally and their checksums tracked per
sandbox. The production migration journal remains in
`public._sqlx_migrations` so it is stable across data-plane selection.

Requests and WebSocket operations hold shared database lifecycle locks;
clean/rotate/delete/restore/purge take the exclusive lock. Per-environment hubs,
pools, workers, IAM clients, and lifecycle generations isolate delivery,
authorization invalidation, and reconnection. Internal migration versions,
authorization generations, and clean receipts survive clean as infrastructure
metadata; all message/conversation/draft/receipt/presence/GIF state is removed.
