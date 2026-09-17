# DM 0.9.0

Conversation listings include every accessible participant and `last_message_status`, with compact group/message summaries. Group-only listing uses client filtering.

WebSockets use consistent command, `.success` and `.error` names; `bundle` creates a bundle with the same transaction and idempotency guarantees as HTTP. Public identity fields use `member`. Durable delivery metadata lives in `data.metadata`.

Bundle codes start at `001` within each conversation, grow past `zzz`, and remain stable across retries. Message edits preserve old content in `history`; deletion sets `deleted_at` without adding a history entry. Message edits/deletes no longer require numeric versions. Draft and group concurrency tokens remain unchanged.

Migration 0024 backfills bundle codes; 0025 preserves existing edits and deletions in the history store; 0026 selects HTTP 3, WebSocket 5 and shared transport 2. Stop old API/worker processes before 0025, apply migrations and runtime grants, then start the new backend and update the gateway/frontend/CLI. Existing explicit old contract versions are rejected. Take a database snapshot before migrating; an old binary cannot run against the new schema.

Validation includes workspace tests and Clippy, an upgrade fixture seeded with old message revisions, bundle creation/retry/authorization checks, message history/deletion checks, web tests, and OpenAPI example validation.
