# DM 0.13 instant send

Client and CLI 0.13.0 make `dm messages send` return as soon as the message is
durably queued on this machine, typically in 20 to 40 ms, instead of waiting for
DM's backend. The backend, HTTP contract and protocol crate 0.11.0 are unchanged.

## Why

A send used to take about 1.5 s, and about 2.3 s after 30 idle minutes, from a
network 240 ms away from the backend. The command waited for a diagnostic upload
before printing, the relay opened a new TLS connection for every request and
token refresh, refreshed an expired 30-minute access token in the middle of the
send, looked for new work only every 300 ms, and held every later request behind
one send that was retrying.

## What changes

- `dm messages send` returns once the request is in the home's durable queue.
  The output has `"message_state": "waiting"` and the relay `request_id`; the
  relay delivers it in the background with its original idempotency key, across
  crashes and restarts. `dm relay result REQUEST_ID` shows the outcome.
- `--wait` (or an explicit `--wait-seconds N`) keeps the previous behaviour:
  wait for the backend and print the stored message with its conversation-local
  ID. Use it when you need that ID immediately.
- The CLI no longer needs the relay to be ready. If the relay is not running or
  does not serve this home yet, the CLI writes the request into the durable
  queue, launches `dm daemon start` detached, and returns.
- CLI diagnostics are recorded locally and uploaded by the relay; no command
  waits on the network for telemetry.
- The relay shares one HTTP connection pool (HTTP/2 when offered) across every
  home and profile, keeps it warm with `GET /live` every 20 seconds, replaces it
  after the machine sleeps, refreshes access tokens at 80% of their lifetime,
  starts a submitted request at once, and retries a request or refresh that
  fails in transit (or meets a momentary 5xx/429) once immediately.
- Requests are ordered per conversation: a send retrying in one conversation no
  longer delays messages to anyone else. Requests that are not about one
  conversation keep order with everything queued before them.
- `--wait` and other waiting commands poll the relay every 10 ms instead of 100 ms.

## Upgrading

Nothing to run. The first 0.13 command replaces an older shared relay. Requests
queued by an older release keep whole-profile ordering until they finish.
Scripts that read the stored message from `dm messages send` output should add
`--wait`. The long-message warning ("Message sent but it was above...") is
printed only when the send is waited for, because it reports a delivered message.
