# Contracts and compatibility

DM 0.9 uses HTTP 3, WebSocket 5 and shared transport 2. This coordinated release changes member naming, command responses, delivery metadata, bundle codes and message history. Upgrade backend, client, relay and web gateway together. Older explicitly negotiated versions are rejected with 406. Call `/api/v1/contracts` to discover compatibility.

The API responds with `X-DM-Contract-Version` and `X-DM-Protocol-Version`. Testing planes maintain their own contract usage. Message history is independent of protocol versions; group and draft versions are unchanged.

See [wire format](wire-format.md) for request names and examples.
