# Silicon DM backend

Silicon DM is the Rust backend for organization-scoped direct messaging across
Silicon actors. It exposes the REST and WebSocket APIs described in
[`API_DOCS.md`](API_DOCS.md) and [`openapi.yaml`](openapi.yaml), persists durable
state in PostgreSQL, and integrates with IAM, Briefcase, and Giphy.

## Architecture

The repository is a modular monolith with explicit process boundaries:

- `dm-api` serves REST and WebSocket traffic and runs the local durable-delivery
  loop used to fan out events to connected sockets.
- `dm-worker` runs cross-instance lease, presence-expiry, idempotency, and
  retention maintenance. Live socket fan-out runs in each API process because
  its connection registry is process-local.
- `dm-migrate` applies the embedded, forward-only PostgreSQL migrations.

Application policy lives under `src/application`, domain invariants under
`src/domain`, adapters under `src/infrastructure`, HTTP transport under
`src/api`, and realtime transport under `src/realtime`. PostgreSQL remains the
source of truth; in-memory state is limited to process-local connections and
caches.

## Prerequisites

- Rust 1.98.0 (the pinned toolchain is installed automatically by `rustup`)
- PostgreSQL 16, or Docker with Compose for the development database
- Reachable IAM and Briefcase services for end-to-end operation
- A verified DM IAM application whose actor tokens include `obo.issue` and
  whose reviewed grants permit the configured Briefcase action
- A Giphy API key configured through `DM_GIPHY_API_KEY`

End-to-end release also requires the sibling services to implement their
published machine contracts. At the time of this repository snapshot, the
sibling IAM implementation does not mount its documented generic token
introspection endpoint and its closed OBO action registry does not admit the
DM or Briefcase actions used here. Briefcase also has an unresolved
delegated-authorization path. DM deliberately fails these calls
closed; see the integration gate in [`API_DOCS.md`](API_DOCS.md) and D-046 as
narrowed by D-048 and D-049 in [`decisions.md`](decisions.md) before promoting
a multi-service deployment.

`cargo-deny` is optional locally and is installed by CI for dependency policy
checks.

## Local development

Create a private local configuration and start PostgreSQL:

```sh
cp .env.example .env
docker compose up -d postgres
```

The Compose credentials and exposed port are disposable development defaults.
Override `POSTGRES_USER`, `POSTGRES_PASSWORD`, `POSTGRES_DB`, or
`DM_POSTGRES_PORT` in your shell when needed. Keep `DM_DATABASE_URL` in `.env`
in sync with those values.

Apply the schema before starting either runtime process:

```sh
cargo run --bin dm-migrate
cargo run --bin dm-api
```

The API listens on `DM_BIND_ADDR`. Its unauthenticated probes are:

- `GET /live` — process liveness only
- `GET /ready` — PostgreSQL connectivity, migration checksum, and schema-access readiness

Run the standalone maintenance worker in another terminal when exercising that
independently deployed process:

```sh
cargo run --bin dm-worker
```

Stop the development database with `docker compose down`. Add `--volumes` only
when you intentionally want to delete the local PostgreSQL data volume.

## Configuration

All runtime settings use the `DM_` prefix and are documented with safe local
defaults in [`.env.example`](.env.example). The migration process only requires
`DM_ENVIRONMENT`, `DM_DATABASE_URL`, the database pool settings, and
`DM_LOG_FILTER`; API and worker processes validate the complete configuration.

For a containerized process, bind on all interfaces and point the database URL
at the Compose service or production PostgreSQL hostname:

```text
DM_BIND_ADDR=0.0.0.0:8080
DM_DATABASE_URL=postgres://<user>:<password>@postgres:5432/<database>
```

Build the production image and select a process by overriding its default
command:

```sh
docker build --tag silicon-dm:local .
docker run --rm --env-file .env silicon-dm:local dm-migrate
docker run --rm --env-file .env --publish 8080:8080 silicon-dm:local
docker run --rm --env-file .env silicon-dm:local dm-worker
```

## Verification

The same checks run in CI:

```sh
cargo fmt --all -- --check
cargo check --locked --all-targets --all-features
cargo test --locked --all-targets --all-features
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo deny check
npx --yes @redocly/cli@2.49.0 lint openapi.yaml
```

Tests that use Testcontainers require a running Docker daemon.

## Deployment and security

- Run `dm-migrate` as an explicit release step; API and worker startup never
  mutate the schema.
- Run [`deploy/runtime-grants.sql`](deploy/runtime-grants.sql) as the migration
  owner with `psql -v runtime_role=<role>` after migrations. API and worker use
  that separate runtime role; readiness verifies its migration-table and DM
  schema access.
- Set `DM_ENVIRONMENT=production`. Production configuration rejects non-HTTPS
  public, IAM, Briefcase, and Giphy URLs.
- Inject `DM_IAM_APP_SECRET`, `DM_GIPHY_API_KEY`, and database credentials from
  a secret manager. Never bake them into an image or commit a populated `.env`.
- Production configuration requires `sslmode=verify-full` for PostgreSQL. Use a
  least-privilege runtime role and restrict migration privileges to the
  migration job where practical.
- Terminate public TLS at a trusted ingress, preserve WebSocket upgrades, and
  align ingress body and request-timeout limits with the `DM_` settings.
- Treat `/ready` as deployment infrastructure metadata and restrict it at the
  ingress when public exposure is unnecessary.
- The image copies only the three application executables into a minimal
  CA-enabled runtime stage and runs them as a fixed non-root user.

This repository is proprietary; see `Cargo.toml` for package metadata.
