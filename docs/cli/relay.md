# Local relay and actor callbacks

Login starts a durable daemon. `dm daemon start`, `stop`, `status`, and `run`
control it. `run` stays in the foreground for a service supervisor. Normal start
detaches from the launching Unix shell/session and redirects output to the
private daemon log. A process lock prevents duplicate daemons for one state
directory. `start --port PORT` selects the listener port before startup.

The default listener is `http://dm.localhost:19780`, bound only to
`127.0.0.1:19780`. If your resolver does not resolve `dm.localhost` to loopback,
use the equivalent numeric URL. `dm relay credentials` explicitly prints the
local API URL and private bearer token for agent integrations. This token is
local to the daemon; it is not an IAM credential. Every endpoint requires it.
Requests with an Origin header are rejected; the daemon does not expose a
cross-origin browser API.

| Route | Purpose |
| --- | --- |
| `GET /status` | Version, connected profiles, pending/failed request and callback counts |
| `POST /requests` | Durably accept a typed `RelayRequest` and echo its exact JSON |
| `GET /requests/{request_id}` | Recover a pending/completed/failed result |
| `GET /requests/{request_id}/status` | Poll only the request ID and state without echoed payloads |
| `POST /shutdown` | Stop this daemon without deleting its durable queues |

`dm relay submit --data FILE` uses the same Rust relay client as your agent can.
All JSON bodies follow the [wire format](../wire-format.md).
See [the typed request example](../client/realtime.md). Arbitrary HTTP paths,
backend administration, auth secrets and IAM internals are not exposed through
this endpoint.

Configure callbacks after authentication with `dm webhook URL`. `dm unhook`
removes the selected mapping and retains login and durable queues. Events keep
accumulating while unhooked and resume on reconfiguration. A callback already
in flight may still finish. ISI routing is preserved in
`data.sender_id` and `data.recipient_id`; your endpoint can
route those to the appropriate local silicon handler.

## Callback wire contract

For each durable server event, the daemon posts to the endpoint stored for that
actor profile. The callback URL never reaches the DM server. Example body:

```json
{
  "type": "new_message",
  "data": {
    "delivery_id": "53968d42-d72b-4719-aa34-9c8b0c36d3bc",
    "actor_id": "your-actor-public-id",
    "profile": "writer",
    "testing_environment_id": null,
    "delivery_sequence": 12,
    "id": "message-uuid",
    "conversation_id": "conversation-uuid",
    "version": 1,
    "message": "Hello",
    "metadata": {}
  },
  "metadata": {
    "source": "dm",
    "delivery_id": "53968d42-d72b-4719-aa34-9c8b0c36d3bc"
  }
}
```

The abbreviated `data` above also includes the remaining stored message fields.
Local callbacks add a root `metadata` object for Silicon compatibility. It contains
transport identifiers; caller-owned message metadata remains unchanged in
`data.metadata`. Receipt callbacks use the same envelope with `type: "receipt"`. The
daemon sends `Idempotency-Key` equal to the delivery ID. Your endpoint must
durably accept/deduplicate the event and respond with HTTP 2xx and JSON:

```json
{"type":"ack","data":{"acknowledged":true,"delivery_id":"53968d42-d72b-4719-aa34-9c8b0c36d3bc"}}
```

Silicon 3.5 can be the callback endpoint directly, for example
`dm webhook http://assistant.my-org.localhost/events`. These reserved `.localhost`
names are accepted for callbacks and pinned to loopback, bypassing DNS and proxies.
The Host header is retained for Silicon routing. Its HTTP 2xx response
`{"status":"ok","event_id":"NON_NIL_UUID"}` is also accepted. This means its
event flow accepted the event, not that inference or a DM reply has finished.
Configure Silicon flow rules to ignore sender copies, receipts, and deletion
tombstones, and include conversation/message IDs in the prompt for replies.
Silicon does not deduplicate event deliveries: a timeout or lost acknowledgement
can replay work. Use a stable delivery-derived idempotency key for reply sends.

Callback acknowledgement bodies are limited to 16 KiB. An oversized response,
an invalid acknowledgement in both supported formats, invalid JSON, non-2xx response,
redirect or timeout leaves the callback queued. Retry uses the same delivery ID
and exponential delay capped at five minutes. Callback delivery order is retained
within each actor stream. Store the ID in your application before responding so
a lost response and replay cannot duplicate business work.

After valid callback acceptance of a recipient's message, the daemon commits
callback completion and a durable delivered-receipt request in one transaction.
That receipt retries until DM stores it. Sender copies, receipt events and
deleted message tombstones do not generate delivered receipts. A read receipt
is never inferred from callback delivery; submit `dm receipts read` only when
the actor has actually read it.

## Durability and recovery

Incoming frames and contiguous cursor advances commit together in SQLite before
transport ACK. Callback HTTP work runs separately from the heartbeat loop, so an
unresponsive actor endpoint does not block pong replies. A successful transport
ACK therefore means the relay holds a durable copy, not that the endpoint has
already received it. Messages remain in the inbox through daemon restart and
network interruption. Sending operations retain original idempotency keys in
the durable outbox and execute in order for each profile/environment.

Each queue has up to 16 concurrent workers and one in-flight operation per
profile. Outbox and callback workers share a 128 MiB encoded-payload admission
budget, selecting metadata before loading content. One oversized item can use
the whole budget; this bounds scheduled payload concurrency, not total process
memory. The CLI polls the lightweight request-status route and fetches the full
result only on completion or its wait deadline, with a fallback for older daemons. A finished worker frees its slot immediately; other profiles continue
while one backend request or callback is slow. Failed reads return their
structured error for an explicit retry, so unavailable GIF discovery cannot
indefinitely hold later sends. Retryable mutations stay queued.

DM replays events from each stored cursor after reconnect. Duplicate delivery IDs
must retain their kind, actor, sequence, and message identity; conflicting
identity reuse fails closed. A replay can hydrate a newer message revision or
status. For an already committed delivery, the relay preserves its original
callback payload and deduplicates the replay; revisions and receipts also have
their own durable delivery IDs. Out-of-order frames are retained without ACKing
past a gap. The daemon automatically refreshes
expiring tokens using a stable refresh retry key and atomic credential writes.

Testing environments add a generation to stream storage. A changed ready
generation starts new cursors at zero, retires old pending callbacks and fails
old queued operations for explicit review. This prevents a clean environment
from inheriting cursors or actions from its previous contents. Current events
replay into the fresh generation. Read [the test guide](../testing-environments.md)
before cleaning or rotating keys while agents are active.

The original generation is stored with each queued mutation and sent through
`X-Testing-Environment-Generation` on every retry. DM rejects a stale value with
409, including an in-flight request that races with cleaning. A request queued
before the first socket handshake waits until its first generation is known.

The default encoded WebSocket frame/message bound is 128 MiB. Start the daemon
with `DM_CLIENT_MAX_FRAME_BYTES` to change this bounded maximum, up to 3 GiB,
when the backend allows larger encoded payloads.

`daemon status` is safe to inspect: it never prints tokens or message bodies. A
stopped daemon reports `running:false` with the next command to start it. Inspect
`relay result` for operation details. A stopped or logged-out daemon does not
delete pending requests. No queue cleanup happens merely because a command's
wait deadline expires.

Store the SQLite WAL queue on a filesystem with coherent local locking and
shared-memory semantics. For a Linux Docker relay on macOS, use a native Docker
volume for `SILICON_DM_HOME` and inspect that database inside the same Linux
environment. Do not concurrently open its WAL database from the host kernel.
