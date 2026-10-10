# Silicon DM 0.5.0

## Rollout status

The 0.5 backend, worker, browser gateway, and frontend are deployed in production.
The protocol, Rust client, and CLI packages are published as **0.5.0** on crates.io.
The [source branch](https://github.com/teamofsilicons/silicon-dm/tree/codex/dm-understanding-0.5)
contains the implementation and deployment configuration.

Production diagnostics are arriving in the dedicated Space Station table. Report
email delivery is configured through the existing Team of Silicons Postmark
server, using private backend credentials and its transactional stream. The sender
domain has verified DKIM and Return-Path records. Sandbox reports are simulated.

The docs are live at [docs.dm.teamofsilicons.com](https://docs.dm.teamofsilicons.com/).
The [Vercel mirror](https://silicon-dm-docs.vercel.app/) remains available.

```sh
curl -fsSL https://docs.dm.teamofsilicons.com/install.sh | sh
```

To build the implementation locally:

```sh
git clone --branch codex/dm-understanding-0.5 https://github.com/teamofsilicons/silicon-dm.git
cd silicon-dm
cargo build --workspace
```

Use a backend running the same implementation for new shared transport, reports,
telemetry collection and app-secret discovery.

## Changes

This release implements the updated product understanding for sandbox entry,
shared relay connections, CLI behavior, contract lifecycle, and hosted documentation.

- IAM SDK 1.8 enables test app-secret discovery and sandbox identity login.
- Auto-discovered IAM environments synchronize metadata and resets without manual pairing.
- One local relay connection per backend independently authenticates all registered profiles.
- The daemon checks hourly for CLI updates while the CLI is idle.
- Test context appears on CLI stderr even when commands fail.
- Browser sign-in and account settings accept a test app secret, display a named identity banner, and restore production on exit.
- HTTP and realtime contracts have negotiation, compatibility discovery, local lifecycle counters, and seven-day idle sunset after deprecation.
- CLI reports support an optional PR link and durable Postmark notifications.
- Documentation starts with installation and usage, then covers development and protocols.

Space Station telemetry is enabled by default with CLI/web/server opt-out. The
`silicondm` table receives production diagnostics; sandbox diagnostics remain in
their isolated schemas. Bug reports queue Postmark notifications with retries.
Existing HTTP v1,
WebSocket v3, and manually paired sandbox consumers retain their compatibility paths.

Deploy the migrations before starting the new API or worker. The frontend must
point to a backend that includes the sandbox-discovery routes. Upgrade server
before client/CLI 0.5, whose shared transport is new. Old clients can continue
using the standalone socket on the new server.

## Verification — September 13, 2026

- 70 Rust tests pass across the workspace, including PostgreSQL integration tests.
- 14 web tests pass; TypeScript checking and the production web build pass.
- Rust formatting, Clippy with warnings denied, and cargo-deny checks pass.
- OpenAPI validation passes; all 27 documentation pages and local links validate.
- Two differently written backend URLs and two authenticated daemon profiles were
  verified to use one physical shared WebSocket.
- IAM sandbox discovery, concurrent selection, reset generations, revoked selectors,
  production isolation and refusal of app-secret administrative authority are covered.
- Postmark failure/retry, report idempotency and suppression of real sandbox mail
  were verified against a mock provider; no test email was sent to maintainers.
- The live `tos / silicondm` Space Station table received and acknowledged diagnostic
  verification records. Its ingest key is kept in private deployment configuration.

- Production migrations completed with authenticated TLS and credential-continuity
  checks, followed by a successful API/worker CloudFormation rollout.
- The new browser gateway passed its live health check; the frontend is deployed
  at [dm.teamofsilicons.com](https://dm.teamofsilicons.com).
- Live contract discovery returns service 0.5.0 and shared protocol 1; the shared
  WebSocket returns its prewarmed frame before any profile subscribes.
- The published crate family passed Cargo's package verification, including a
  combined workspace publication dry run before uploading. A fresh isolated
  crates.io installation reports `dm 0.5.0`; its public `iam --json` call succeeds.

The requested docs domain resolves to Vercel through authoritative DNS and both
Google and Cloudflare public resolvers. HTTPS serves the docs with a valid
certificate. All GitHub CI checks passed, including the release image smoke test.
Postmark credentials were authenticated against the live provider and the sender
domain was verified. No production bug-report email was sent during verification.
