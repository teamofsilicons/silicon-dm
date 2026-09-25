# Outgoing relay and Ting destinations

DM's local relay handles outgoing typed HTTP commands. Ting's system daemon owns
incoming delivery, reconnects, durable queues, retry/replay and acknowledgements.
There is no DM incoming socket, callback forwarder or automatic Delivered receipt.

## Configure a Ting endpoint

```sh
dm login --token-file -
dm delivery register
dm delivery login --token-file -
dm webhook http://localhost:9000/tings --all-apps --secret-file /private/callback-secret
dm delivery status
```

The two logins consume separate IAM SLTs for DM and Ting respectively. Delivery
registration is explicit recipient consent; neither login restores revoked grants.
Install/start Ting's official daemon before attachment. A missing Ting service is
an actionable error, not a reason to start a DM receiver.

The endpoint URL is configured directly in Ting and stays local. It receives all
eligible apps for the selected recipient and organization. `--all-apps` explicitly
accepts this generic-consumer contract; no DM app filter is implied.

## Incoming wire and acceptance

Ting sends a raw `{"tings":[...]}` body, `Ting-Webhook-Id`, and the configured
bearer secret. Authenticate these against the exact saved hook before interpreting
the batch. Local items omit `for`, so recipient identity must come from that
trusted hook binding, not arbitrary JSON.

Route all apps by type. DM events use `<app_id>.sync.changed` and carry references
including org, conversation, message code, delivery UUID and optional ISI. Fetch
the current message from DM under normal permissions. The Rust client's
`hydrate_ting_batch` validates and fetches references, but neither dispatches
callbacks nor acknowledges them. See [the consumer guide](../client/README.md).

Return **HTTP 204** only after the whole generic batch is durably accepted.
Ting handles retries with stable IDs; deduplicate before repeating business work.
A successful DM subset cannot acknowledge unrelated app items. HTTP 200 with the
old DM `ack` JSON or Silicon `status: ok` event result is not Ting acceptance.
No automatic adapter converts the old callback protocol.

Ting's delivery/read state is distinct from DM's Delivered/Read receipts.
Use `dm receipts delivered CONVERSATION MESSAGE` after the intended recipient
application accepts the message, and `dm receipts read` after it reads it.
Do not generate recipient receipts for sender copies, receipt events or tombstones.

Reuse the saved hook ID across restarts. Recover uncertain registration with
`dm webhook URL --all-apps --id HOOK_ID`; `--takeover` explicitly transfers that
same hook from another live receiver. `--health-url` configures Ting's optional
health check. `dm unhook` detaches the saved hook; explicit reattachment reuses its
ID. `delivery reconnect` rebinds an attached destination; `delivery logout` ends
this bound Ting session. DM logout remains separate.

## Outgoing loopback API

`dm daemon start|stop|status|run` controls only the outgoing relay. Login starts
it; `run` stays foreground for a supervisor. It is bound to `127.0.0.1`,
reachable as `http://dm.localhost:PORT`.

One relay process serves every DM home of an operating-system user: each
Silicon's `SILICON_HOME`, a Carbon's `~/.silicon-dm`, and any `SILICON_DM_HOME`.
When a home runs a DM command and the relay is already running, the home attaches
to it instead of starting another. Homes keep separate profiles, queues and
bearers; a request reaches only its own home's queue. The shared directory is
`~/.silicon-dm/relay` of the real user account (not `HOME` or `SILICON_HOME`),
overridable with an absolute `SILICON_DM_RELAY_HOME`. It holds the host lock,
`host.json` (PID, port and a private attach token), `stores.json` (every attached
home, so a restarted relay resumes all their queues) and `daemon.log`.

A new relay prefers the port the launching home last used (19780 by default, or
`start --port PORT`). If that port is in use it tries the next 100 ports, then any
free loopback port. The port in use is written to each attached home's
`relay_port` and reported as `host.port` by `daemon status`. `--port` is only a
preference: a running relay keeps its port and reports the request as
`host.requested_port`. `daemon stop` stops the relay for every home; queued
requests stay on disk and the next DM command from any home starts it again.
When a newer CLI finds an older shared relay, it stops it and starts its own.
The 16 concurrent outgoing requests and the 128 MiB payload budget are shared by
all homes.

`dm relay credentials` explicitly prints this home's local bearer and URLs; this
bearer is not an IAM token. Every endpoint requires it. Requests with an Origin
header are rejected; this is not a cross-origin browser API.

| Route | Purpose |
| --- | --- |
| `GET /status` | Outgoing profile/queue state and explicit Ting migration status. |
| `POST /requests` | Durably accept a typed request and echo its exact JSON. |
| `GET /requests/{id}` | Read the original request and pending/completed/failed result. |
| `GET /requests/{id}/status` | Read lightweight progress. |
| `POST /shutdown` | Stop the shared relay for every home without deleting requests. |
| `POST /homes` | Attach a home. Requires the attach token from `host.json`, not a home's bearer. |

`dm relay submit --data FILE` submits a `type: request` envelope containing a
`RelayRequest`. Keep its request UUID and operation idempotency key unchanged when
retrying. A local 202 ACK proves disk persistence; it does not prove backend
success, Ting acceptance or recipient delivery. Use `dm relay result REQUEST_ID`
to recover an uncertain outcome. Ordinary message, draft, group and receipt
commands use this same outgoing relay; no auth secret belongs in a queued command.

Work is ordered in lanes within each profile/environment. Requests about one
conversation (sends, edits, deletes, receipts, drafts, bundles and reads of that
conversation) run strictly in order, so a later read observes earlier sends, but
conversations do not wait for each other: a send retrying in one conversation
never delays another. Presence has its own lane. Every other request (groups,
conversation lists, creating conversations) keeps order with everything queued
before it, except requests already failing and retrying. Up to 16 lanes run
concurrently with a 128 MiB encoded-payload admission budget.

A submitted request starts at once; the relay does not wait for a periodic scan.
One HTTP connection pool (HTTP/2 when the backend offers it) serves every home and
profile, and the relay keeps it warm with a light `GET /live` every 20 seconds
while any profile is logged in, replacing it after the machine sleeps. Access
tokens are refreshed in the background at 80% of their lifetime (at most five
minutes early), so a send after a long idle period does not wait for a refresh.
A request that fails in transit, or whose refresh meets a momentary 5xx/429, is
retried once immediately on a new connection before normal backoff applies.

`dm messages send` does not need the relay to be ready. When the relay is not
running, or does not yet serve this home, the CLI writes the request straight into
the home's durable queue, launches `dm daemon start` detached, and returns; the
relay sends the request as soon as it attaches the home. A larger single request
runs alone; this is not a total process-memory limit. Retryable mutations retain
their original key; failed reads return structured errors for explicit retry.
Presence uses HTTP leases and expires across relay restart.

Before a sandbox mutation, HTTP `/iam` must confirm its saved generation. The
original generation accompanies each retry, preventing old commands from writing
to a cleaned environment. No socket handshake is involved.

## Upgrading existing state

A relay from an older release served one home only. When the shared relay
attaches that home, it stops the recognized old relay (using that home's own
port and bearer), waits for its lock and then serves the home's existing queue.
An unrelated service on that port is not stopped; the shared relay uses another
port.
`daemon status` reports `incoming_delivery.code: delivery_moved_to_ting` and
`forwarding: false`; `delivery status` separately reports Ting state.

Fresh installations create no DM incoming inbox or cursor tables. Existing rows
remain unchanged for explicit inspection/migration, counted by
`retained_legacy_deliveries` and `retained_legacy_pending_deliveries`.
`pending_webhooks` is zero because there is no DM incoming worker. Login or Ting
attachment does not replay these rows or translate old callback ACKs.

Use DM history and the SDK's initial boundary/snapshot/sync recovery procedure to
initialize a consumer; a new Ting hook is not a complete message-history import.
DM messages remain available after Ting delivery records expire. See
[initial history and recovery](../client/README.md#initial-history-and-recovery).

Store the outgoing SQLite WAL on a filesystem with coherent local locking and
shared-memory semantics. A stopped/logged-out relay retains pending requests;
wait timeouts do not delete work. Ting and the generic consumer have separate
service lifecycles.
