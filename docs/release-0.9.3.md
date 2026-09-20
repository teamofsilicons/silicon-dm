# DM 0.9.3

Message creation uses `message.create`, with `message.create.successful` confirming
creation to the sender. Failed actions no longer include a success acknowledgement.
The CLI limits Silicon messages to Carbon recipients to 140 Unicode characters;
the explicit long-message override remains available.

Draft conflicts report the stale version and preserve the current server draft.
HTTP failures retain diagnostic details, and the gateway accepts the current
conversation identifier format.

The website renews expired access tokens before retrying an HTTP request or
reconnecting its WebSocket. HTTP recovery retries once with the same encoded body,
idempotency key, conditional headers, and testing-generation fence. Temporary
network or server failures retain the session; rejected refresh credentials end
it. The gateway also preserves streamed 401 responses so the browser can renew
the affected profile instead of receiving a raw transport failure.

Local validation includes 41 web tests, nine browser regression checks, the
production web build, and Rust workspace tests and Clippy. Release preparation
does not establish publication or deployment. Update the backend, gateway,
frontend, client, and CLI together; no database migration is required.
