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
it; `run` stays foreground for a supervisor. Its default address is
`http://dm.localhost:19780`, bound to `127.0.0.1`. A process lock permits one host
per state directory. `start --port PORT` chooses the listener port.

`dm relay credentials` explicitly prints its local bearer and URLs; this bearer
is not an IAM token. Every endpoint requires it. Requests with an Origin header
are rejected; this is not a cross-origin browser API.

| Route | Purpose |
| --- | --- |
| `GET /status` | Outgoing profile/queue state and explicit Ting migration status. |
| `POST /requests` | Durably accept a typed request and echo its exact JSON. |
| `GET /requests/{id}` | Read the original request and pending/completed/failed result. |
| `GET /requests/{id}/status` | Read lightweight progress. |
| `POST /shutdown` | Stop the outgoing relay without deleting requests. |

`dm relay submit --data FILE` submits a `type: request` envelope containing a
`RelayRequest`. Keep its request UUID and operation idempotency key unchanged when
retrying. A local 202 ACK proves disk persistence; it does not prove backend
success, Ting acceptance or recipient delivery. Use `dm relay result REQUEST_ID`
to recover an uncertain outcome. Ordinary message, draft, group and receipt
commands use this same outgoing relay; no auth secret belongs in a queued command.

Work executes in order per profile/environment, with up to 16 concurrent profile
workers and a 128 MiB encoded-payload admission budget. A larger single request
runs alone; this is not a total process-memory limit. Retryable mutations retain
their original key; failed reads return structured errors for explicit retry.
Presence uses HTTP leases and expires across relay restart.

Before a sandbox mutation, HTTP `/iam` must confirm its saved generation. The
original generation accompanies each retry, preventing old commands from writing
to a cleaned environment. No socket handshake is involved.

## Upgrading existing state

New runtime startup stops a recognized old DM relay before launching the new
outgoing-only host, preserving state. An unrelated local service is not stopped.
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
