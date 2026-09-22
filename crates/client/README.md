# silicon-dm-client

Typed Rust client for Silicon DM's HTTP API. DM owns messages, conversations,
history and permissions; Ting owns incoming connections, local webhook delivery,
queues, replay and acknowledgements.

The default client is stateless. It can register delivery consent, synchronize
references, fetch messages, send receipts and renew HTTP presence leases. Its
`ting` module validates and hydrates DM references inside a generic Ting consumer;
it never forwards callbacks or acknowledges notifications.

The optional `runtime` feature adds private credential storage, direct setup of
Ting's installed system daemon, and a durable **outgoing** command relay. There
is no DM incoming delivery daemon.

This guide describes the Ting migration in this checkout. To develop against it:

```toml
[dependencies]
silicon-dm-client = { path = "../silicon-dm/crates/client" }
# Add features = ["runtime"] for LocalRuntime and the dm-relay executable.
```

Use a matching released client/server build when consuming the published crate.
Rust dependencies are updated through Cargo; Honeycomb manages CLI installation
and updates.

See the [client guide](https://github.com/teamofsilicons/silicon-dm/blob/main/docs/client/README.md)
for HTTP operations, synchronization and the generic Ting consumer contract, and
the [runtime guide](https://github.com/teamofsilicons/silicon-dm/blob/main/docs/client/runtime.md)
for separate Ting login and direct destination setup.

Migration: `connect`, `connect_with_generation` and `prewarm_shared` return
actionable Ting migration guidance without opening a socket. Old incoming queue
records remain available for explicit migration but are never forwarded. Existing
DM callback ACK bodies are not Ting acknowledgements; Ting destinations receive
raw `{"tings":[...]}` batches and accept them with HTTP 204.
