# Ting delivery and retired DM sockets

DM's public `/api/v1/ws` and `/api/v1/ws/shared` endpoints are retired and return
HTTP 410 with `delivery_moved_to_ting`. Rust `connect`,
`connect_with_generation`, and `prewarm_shared` return migration guidance without
opening a socket. There is no fallback to an older DM WebSocket version.

Ting owns incoming connections, heartbeats, reconnects, local destinations,
delivery queues, replay and ACKs. DM retains messages, history and permissions;
normal sends, fetches, edits, receipts and presence use HTTP.

## Consumer migration

1. Explicitly register the recipient's DM grant with `Client::register_delivery`
   or `dm delivery register`.
2. Log in separately to Ting using its own IAM-bound SLT. Install/start the
   official Ting system daemon.
3. Attach a generic local endpoint directly to Ting with
   `dm webhook URL --all-apps`, or `LocalRuntime::delivery_attach`.
4. Authenticate its saved hook ID/secret and handle raw `{"tings":[...]}` batches
   for every eligible app. Return HTTP 204 after accepting the entire batch.
5. Validate and hydrate recognized DM references through the stateless SDK helper;
   initialize and recover message history through HTTP sync and snapshots.

The helper never runs an incoming daemon, forwards another callback, stores an
incoming queue or emits a receipt. Old DM callback ACK JSON and Silicon event
result JSON are not Ting ACKs. Ting transport/read state never automatically
changes DM Delivered or Read status.

An HTTP sync cursor is opaque and bound to the authenticated actor/org/environment.
Never use Ting sequence numbers as cursors. Start recovery with `sync_reset()`,
then load the accessible snapshot and resume from that boundary so concurrent
updates are not skipped. Sandbox generation comes from `/iam`; bind writes and
hydration to it rather than waiting for a retired socket's ready frame.

The DM local relay continues to durably queue outgoing HTTP commands. Existing
legacy inbox/cursor records are retained but never forwarded; status reports
`delivery_moved_to_ting` and retained counts. No login or destination attachment
silently migrates them.

See [the Rust client guide](README.md) for hydration, receipts and sync,
[the runtime guide](runtime.md) for separate Ting login and direct attachment,
and [the outgoing relay guide](../cli/relay.md) for command submission/recovery.
