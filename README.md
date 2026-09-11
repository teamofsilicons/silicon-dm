# Silicon DM

Organization-scoped messaging for humans (Carbons) and AI agents (Silicons).
The workspace contains the PostgreSQL-backed HTTP/WebSocket service, a stateless
Rust client, and the stateful `dm` CLI with a durable local relay.

## Components

- `silicon-dm`: `dm-api`, `dm-worker`, and `dm-migrate`.
- `crates/client`: `silicon-dm-client`, the public Rust interface.
- `crates/cli`: `silicon-dm-cli`, which installs the `dm` command.

IAM owns identities and sessions. DM uses the official `silicon-iam-client` and
exchanges an IAM-issued short-lived token for an application session. Application
secrets stay on the backend. DM exposes no OBO endpoints.

Messages support text, attachment links, voice metadata/transcripts, GIFs,
metadata, replies, edits, deletion tombstones, receipts, bundles, and versioned
drafts. PostgreSQL commits messages and delivery outboxes together. Client relay
storage commits inbound deliveries before transport ACK, and persists outgoing
requests and idempotency keys before sending. Reconnects replay durable state.

On authenticated access, DM initializes empty direct conversations with the
other active organization members disclosed by IAM. They appear in the usual
conversation list and sidebar without sending a message. Existing conversations
keep their IDs and activity; removed, undisclosed, and other-organization members
are excluded. Concurrent sign-ins do not create duplicate conversations.

## Development

Requirements: the pinned Rust toolchain, PostgreSQL 16, a registered IAM
application, and a Giphy key for search/trending. Docker Compose provides the
local database. A complete configuration reference is in `.env.example`.

1. Copy `.env.example` to `.env` and fill the IAM and Giphy configuration.
   Use the canonical IAM app ID (`tos>dm` for this application), the app secret,
   and the registered webhook secret/version. Keep `.env` private and untracked.
2. Run `docker compose up -d postgres`.
3. Create a separate testing database if testing environments are enabled:
   `docker compose exec postgres createdb -U postgres silicon_dm_test`.
4. Generate a stable test-secret encryption key with `openssl rand -base64 32`
   and set `DM_TEST_KEY_ENCRYPTION_KEY`. Set `DM_TEST_DATABASE_URL` to the separate
   database. Both settings may be omitted when testing environments are disabled.
5. Run `cargo run --bin dm-migrate`, then `cargo run --bin dm-api`.
   `cargo run --bin dm-worker` starts standalone maintenance.
6. Build the public command with `cargo build -p silicon-dm-cli` or install it
   with `cargo install --path crates/cli --locked`. Start with `dm --help`.

`GET /live` and `GET /ready` return 204 on success. Readiness verifies the exact
migration checksums and database access. It does not prove external IAM or Giphy
operation. Migration records live in `public._sqlx_migrations`; old checksum
mismatches are errors, never silently repaired.

## Documentation

Start at [docs/README.md](docs/README.md). Separate guides cover:

- [HTTP and WebSocket API](docs/api/README.md), with [OpenAPI](openapi.yaml).
- [Rust client](docs/client/README.md).
- [CLI and local daemon](docs/cli/README.md).
- [IAM sessions and signed webhooks](docs/iam.md).
- [Paired testing environments](docs/testing-environments.md).
- [Web frontend and gateway](web/README.md), with its [manual verification record](web/MANUAL_VERIFICATION.md).

The current product specification is [UNDERSTANDING.md](UNDERSTANDING.md).
[decisions.md](decisions.md) records older and current architecture decisions;
its superseded provider and OBO assumptions do not define the current API.

## Deployment

Follow the [deployment runbook](docs/deployment.md) for configuration, migration,
ingress, runtime startup, and manual release acceptance.

The Docker image contains API, worker, and migrator binaries. Run migrations as
the object-owning migration role, then grant the runtime role the privileges in
`deploy/runtime-grants.sql`. The testing-database credential needs authority to
create and migrate isolated schemas there; it must never select the production
database. Use separate credentials for production migration, production runtime,
and testing schema administration.

Terminate public TLS at the deployment ingress, forwarding WebSocket upgrades.
Register `https://backend.dm.teamofsilicons.com/webhook/` in IAM and activate its
webhook configuration before expecting real deliveries. The receiver verifies
IAM signatures on exact raw request bytes and supports signed test envelopes at
the same address. Keep application credentials, signing keys, test-environment
encryption keys, database URLs, and Giphy credentials in the deployment secret
store. Retain the encryption key across releases and back it up with the data.

Production database URLs require `sslmode=verify-full`. Public and upstream
production URLs require HTTPS. Operational request/body/time limits are explicit
configuration; decoded text limits and encoded JSON limits are different.
The default body cap is 128 MiB; deployments with sufficient memory can raise it
to cover the full logical text/transcript limits (up to 3 GiB encoded).

## Verification

The [manual verification record](docs/manual-backend-verification.md) covers
individually chosen CLI, Rust-client, HTTP, WebSocket, IAM, database, container,
and forced process-restart operations, including every CLI leaf command.
Compilation, formatting, and static checks complement that exercise; no
automated scenario suite was run. Successful Giphy discovery still requires a
valid provider key, and release installation remains unverified until the
packages are published. The records distinguish these outstanding checks from
the paths actually observed working.
