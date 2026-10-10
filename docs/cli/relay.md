# Outgoing relay

DM's local relay sends your typed commands to the DM service, durably and in order. It only
sends: new messages are read from DM's sync feed ([receiving](receiving.md)). It never holds a
token of its own; it uses each profile's stored sign-in and refreshes it in the background.

## Starting and stopping

`dm daemon start|stop|status|run` controls the relay. `dm login` starts it; `run` stays in the
foreground for a supervisor. It listens on `127.0.0.1`, reachable as `http://dm.localhost:PORT`.

One relay process serves every DM home of an operating-system user: each Silicon's
`SILICON_HOME`, a Carbon's `~/.silicon-dm`, and any `SILICON_DM_HOME`. When a home runs a DM
command and the relay is already running, the home attaches to it instead of starting another.
Homes keep separate profiles, queues and bearers; a request reaches only its own home's queue.
The shared directory is `~/.silicon-dm/relay` in the operating-system user's real home (not `HOME`
or `SILICON_HOME`), overridable with an absolute `SILICON_DM_RELAY_HOME`. It holds the host lock,
`host.json` (PID, port and a private attach token), `stores.json` (every attached home, so a
restarted relay resumes all their queues) and `daemon.log`.

A new relay prefers the port the launching home last used (19780 by default, or
`dm daemon start --port PORT`). If that port is in use it tries the next 100 ports, then any free
loopback port. The port in use is written to each attached home's `relay_port` and reported as
`host.port` by `dm daemon status`; a running relay keeps its port and reports a different request
as `host.requested_port`. `dm daemon stop` stops the relay for every home; queued requests stay
on disk and the next DM command from any home starts it again. When a newer `dm` finds an older
shared relay, it stops it and starts its own.

`dm daemon status` reports this home's profiles (`outgoing_only` when signed in, `signed_out`
otherwise, with the account's uuid, id and kind), its queue, and
`incoming_delivery: {"code":"sync_feed","provider":"dm","forwarding":false}`.

## Loopback API

`dm relay credentials` prints this home's local bearer and URLs. That bearer belongs to this
home's relay access, never to a DM sign-in. Every endpoint requires it, and requests with an
`Origin` header are refused: this is not a browser API.

| Route | Purpose |
| --- | --- |
| `GET /status` | This home's profiles and queue, and the shared relay's port and home count. |
| `POST /requests` | Durably accept a typed request and echo its exact JSON. |
| `GET /requests/{id}` | The original request and its pending/completed/failed result. |
| `GET /requests/{id}/status` | Lightweight progress. |
| `POST /shutdown` | Stop the shared relay for every home without deleting requests. |
| `POST /homes` | Attach a home; needs the attach token from `host.json`. |

`dm relay submit --data FILE` submits a `{"type":"request","data":{…}}` envelope:

```json
{"type":"request","data":{"request_id":"7b1e…","profile":"default",
 "request":{"operation":"send_message","conversation_id":"c:ada","idempotency_key":"stable-key","message":{"message":"hello"}}}}
```

Keep the request UUID and the operation's idempotency key unchanged when retrying. A local 202
acknowledgement proves the request is on disk, not that the service accepted it: use
`dm relay result REQUEST_ID`. Operations include `send_message`, `edit_message`,
`delete_message`, `receipt`, `put_draft`, `create_conversation`, `create_group`,
`invite_group_members`, `add_group_owners`, `accept_invitation`, `leave_group`, `allow`,
`list_conversations` (with optional `for`), `me` and the other commands' reads. A request that
names a `testing_environment_id` is refused (`testing_environments_retired`).

## Ordering, retries and speed

Work is ordered in lanes within each profile. Requests about one conversation (sends, edits,
deletes, receipts, drafts, bundles and reads of it) run strictly in order, so a later read
observes earlier sends, but conversations do not wait for each other: a send retrying in one
conversation never delays another. Presence has its own lane. Every other request (groups,
allow-lists, conversation lists) keeps order with everything queued before it, except requests
already failing and retrying. Up to 16 lanes run at once within a 128 MiB payload budget shared
by every home.

A submitted request starts at once. One HTTP connection pool serves every home and profile; the
relay keeps it warm with a light `GET /live` every 20 seconds while any profile is signed in and
replaces it after the machine sleeps. Access tokens are refreshed in the background ahead of
expiry (at 80% of their lifetime, at most five minutes early) with the same single-flight lock
as the CLI, so a send after a long idle period does not wait. A request that fails in transit is
retried once on a fresh connection before normal backoff applies.

Network failures, 408, 429 and 5xx answers retry with backoff under the original key. A sign-in
that ended keeps the profile's requests pending until it signs in again (`authentication_required`
with the reason). Validation, permission and version conflicts finish as failures.

`dm messages send` does not need the relay to be ready: when it is not running, or does not yet
serve this home, the CLI writes the request straight into the home's durable queue, launches
`dm daemon start` detached and returns; the relay sends it as soon as it attaches the home.

## Upgrading existing state

A relay from an older release that served one home only is stopped when the shared relay
attaches that home; an unrelated service on that port is left alone. Requests queued by a
profile that was signed in before DM moved to Silicon Accounts wait until that profile signs in
again: they are sent if the same id signs in, and fail with `account_changed` otherwise. Requests
queued for retired test environments fail with `testing_environments_retired`. Rows of the old
incoming inbox stay unchanged and are only counted (`retained_legacy_deliveries`).

Store the outgoing SQLite WAL on a filesystem with coherent local locking. A stopped or
signed-out relay keeps pending requests; wait timeouts never delete work.
