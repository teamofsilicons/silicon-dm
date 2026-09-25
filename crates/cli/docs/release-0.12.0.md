# DM 0.12 shared outgoing relay

Client and CLI 0.12.0 run one outgoing relay per operating-system user instead
of one per DM home. The backend, HTTP contract and protocol crate 0.11.0 are
unchanged.

## What changes

- Every DM home attaches to the same relay process: each Silicon's
  `SILICON_HOME`, a Carbon's `~/.silicon-dm`, and any `SILICON_DM_HOME`.
  Before 0.12, the first home's relay held port 19780 and every other home's
  relay failed to start.
- A busy preferred port falls back to the next free port, then to any free
  loopback port. The relay writes the port it bound to every attached home's
  `relay_port`.
- Homes keep separate profiles, queues and bearers. A request reaches only its
  own home's queue.
- `dm daemon stop` stops the relay for every home. Queued requests stay on disk,
  and the next DM command from any home starts the relay again.
- The 16 concurrent outgoing requests and the 128 MiB payload budget are shared
  by all homes.
- New Rust API: `LocalRuntime::with_relay_home`, `Store::with_relay_home`,
  `store::relay_home_directory` and `RelayClient::attach`.

## Upgrading

Nothing to run. The first 0.12 command in a home starts or joins the shared
relay. When the shared relay attaches a home that an older per-home relay still
serves, it stops that relay and then serves the home's existing queue. A 0.12 CLI
that finds an older shared relay replaces it. An unrelated service on a home's
old port is left running.

Shared state lives in `~/.silicon-dm/relay` of the real user account; set an
absolute `SILICON_DM_RELAY_HOME` to override it. See
[the relay guide](cli/relay.md#outgoing-loopback-api).
