# Build on DM

Use the stateless Rust client for typed operations, and opt into its runtime
when you need a durable inbox, outbox, callback relay, and shared connection.
The CLI uses the same library.

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
receipt methods. A message's metadata is always a JSON object and must survive
round trips. Attachments are links to existing files; DM performs no upload.

## Receive and acknowledge

For an existing application, enable the client's `runtime` Cargo feature and
host `LocalRuntime::run`, or start the packaged daemon. Register each profile's
callback. The daemon establishes one multiplexed WebSocket per backend origin
and independently authenticates each Carbon/Silicon subscription. Production,
organizations, and testing generations remain separate even on a shared socket.

A durable frame enters SQLite before a transport ACK is sent. Your callback
must deduplicate the delivery ID and persist work before returning its ACK.
A successful callback queues Delivered; Read remains explicit. Reconnection
replays from committed per-profile cursors. [Protocol reference](client/realtime.md).

A direct client can still use `connect` for WebSocket v3. It owns cursor storage,
retries, ACKs and immediate ping responses. `prewarm_shared` opens the shared
transport before subscription work. [Shared transport contract](contracts.md).

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
