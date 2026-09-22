# Build on DM

Use the stateless Rust client for typed HTTP operations and Ting reference
hydration. Its optional runtime provides a durable outgoing command relay and
explicit setup of destinations directly on Ting's installed system daemon.
The CLI uses the same library; DM runs no incoming callback relay.

## Authenticate and select a plane

Production: obtain an IAM SLT for `tos>dm`, exchange with `Client::login`, and
construct a client with the returned access token and organization ID. Keep the
rotating refresh token secure. Never collect IAM passwords or OTPs.

Sandbox: first bind the test app secret using `Client::with_test_key`, discover
with `iam()`, then perform the same login flow. The secret selects an environment;
the returned actor token supplies user authority. [Complete flow](testing-environments.md).

## Make a durable mutation

Generate a stable idempotency key for each logical mutation. Preserve it and the
same content across ambiguous failures. Use `send_message`, drafts, bundles, and
receipt methods. Keep message content in DM’s fixed schema and transport metadata
in the Ting reference envelope. Attachments are links to existing files; DM performs
no upload.

## Receive and acknowledge

Explicitly call `Client::register_delivery` for recipient consent, then log in
separately to Ting with the same typed account, organization and environment.
With the `runtime` feature, use `LocalRuntime::delivery_login` and
`delivery_attach` to configure a generic destination directly on Ting.

Authenticate the callback secret and saved `Ting-Webhook-Id`. Route every app in
the raw `{"tings":[...]}` batch, deduplicate, and durably accept the whole batch
before HTTP 204. `hydrate_ting_batch` validates DM references and fetches current
content under DM authorization; it does not acknowledge anything or send receipts.
Ting owns retry/replay. Explicit DM Delivered and Read remain separate operations.

Initialize history with an HTTP sync boundary, accessible snapshots, and cursor
resume. Reconcile on startup, reconnect and periodically because silent Ting
events produce no browser hint. Never translate a Ting sequence into a DM sync
cursor. DM socket methods are retired and return migration guidance without
network I/O. See [delivery migration](client/realtime.md) and
[contracts](contracts.md).

## Handle failures deliberately

Inspect API status, stable error code, request ID, and retry guidance. Authentication
failures require refresh or a new SLT. Conflicts require reading the latest
version. Retry transient faults with the original idempotency key. A stale sandbox
generation is a new world; do not replay old writes into it.

## Verify your consumer

Run the repository's consumer-driven wire tests to check independent SDK/backend
serialization. Test metadata, attachments, callbacks, duplicate delivery, reconnect,
revocation, and sandbox reset behavior. Negotiate documented contract versions and
follow [deprecation and compatibility policy](contracts.md).
