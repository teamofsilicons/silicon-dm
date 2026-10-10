# DM 0.9.0

Conversation listings include every accessible participant and `last_message_status`, with compact group/message summaries. Group-only listing uses client filtering.

WebSockets use consistent command, `.success` and `.error` names; `bundle` creates a bundle with the same transaction and idempotency guarantees as HTTP. Public identity fields use `member`. Durable delivery metadata lives in `data.metadata`.

Bundle codes start at `001` within each conversation, grow past `zzz`, and remain stable across retries. Message edits preserve old content in `history`; deletion sets `deleted_at` without adding a history entry. Message edits/deletes no longer require numeric versions. Draft and group concurrency tokens remain unchanged.

Migration 0024 backfills bundle codes; 0025 preserves existing edits and deletions in the history store; 0026 selects HTTP 3, WebSocket 5 and shared transport 2. Stop old API/worker processes before 0025, apply migrations and runtime grants, then start the new backend and update the gateway/frontend/CLI. Existing explicit old contract versions are rejected. Take a database snapshot before migrating; an old binary cannot run against the new schema.

Validation includes workspace tests and Clippy, an upgrade fixture seeded with old message revisions, bundle creation/retry/authorization checks, message history/deletion checks, web tests, and OpenAPI example validation.

## Deployment verification — 2026-09-17

Commits `a842fac` and `e0e5543` were pushed to `main`; tag `v0.9.0` identifies the release. Protocol, client, and CLI 0.9.0 were published to crates.io. CI passed.

Production and testing database snapshots were available before migration. Bootstrap task revision 15 exited successfully after applying migrations and runtime grants. API and worker deployments reached steady state on the new runtime image. The gateway, frontend, and documentation were deployed to their existing production domains.

Live checks confirmed `/ready` returns 204, `/api/v1/contracts` reports 0.9.0 with HTTP 3 / WebSocket 5 / shared 2, unauthenticated conversation requests return 401, and explicit HTTP 2 requests return 406. Shared WebSocket prewarming, ping, and `unsubscribe.success` were verified. Frontend, gateway health, and documentation return 200. Authenticated message/history/bundle flows passed local integration tests; saved live testing profiles were logged out, so those flows were not repeated against production.

The six-platform binary workflow passed Linux and macOS builds but failed the Windows discovery test with `Access is denied` during local store initialization. The combined Honeycomb artifact was not produced; this does not affect the deployed backend or published crates.
