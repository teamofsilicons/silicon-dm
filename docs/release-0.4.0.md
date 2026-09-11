# DM 0.4.0

Every DM JSON request, response, WebSocket frame, and actor callback now has exactly `type` and `data` at its root. Message text is `data.message` and caller metadata is `data.metadata`. The WebSocket protocol is version 3. See [wire format](wire-format.md) for endpoint discriminators, callback ACKs, and examples.

This release updates the backend, Rust client, CLI relay, browser gateway, and frontend together. Existing external clients and webhook consumers must adopt the new envelope. HTTP methods, paths, authentication headers, idempotency keys, and empty responses retain their meaning. IAM's signed incoming webhook and provider API contracts are unchanged.

The new `silicon-dm-protocol` crate shares the REST envelope and operation names. Publish it before the 0.4.0 client, then publish the 0.4.0 CLI. Rust message structs retain `text` as the source-level field and serialize it as `message`.

Saved v2 relay deliveries remain readable and pending callbacks are converted to the new envelope with the original delivery ID. Queued operations and stored edit retry hashes remain compatible. No database migration is added in this release.

## Validation

The full workspace tests, including PostgreSQL durability and HTTP/WebSocket SDK interoperability, passed before release preparation. Callback tests cover saved v2 deliveries and retries until an enveloped acknowledgement. Frontend format tests, production build, Clippy, formatting, and OpenAPI validation passed.
