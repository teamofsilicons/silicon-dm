# silicon-dm-client

Stateless Rust client for Silicon DM's public API and WebSocket protocol, with
an optional `runtime` feature for the durable local relay used by the CLI.
The default client owns no local files or background processes and does not
depend on the backend crate. Optional runtimes use explicit caller-owned state
directories and can run in-process or launch the packaged `dm-relay` binary.

See the repository's [client guide](https://github.com/teamofsilicons/silicon-dm/blob/main/docs/client/README.md),
[realtime guide](https://github.com/teamofsilicons/silicon-dm/blob/main/docs/client/realtime.md),
and [optional runtime guide](https://github.com/teamofsilicons/silicon-dm/blob/main/docs/client/runtime.md).

Add the published library to a Rust application:

```toml
[dependencies]
silicon-dm-client = "0.4"
```

Enable `features = ["runtime"]` when the application needs durable local queues,
credential refresh, callback delivery, or the localhost relay. Pass an explicit
private state directory to `LocalRuntime`; the default client remains stateless.
The optional relay executable can be installed with
`cargo install silicon-dm-client --features runtime --bin dm-relay --locked`.

The runtime's caller-owned `UpdatePolicy` enables hourly checks by default.
Call `updates::after_command` with the consuming application's manifest to update
its compatible dependency and rebuild; a running process keeps its linked
version until restarted. The policy can be disabled, and the stateless client
does not invoke Cargo automatically. See the runtime guide for integration and
shutdown examples.

Version 0.2.2 reduces copies of large queued payloads, admits runtime work by
encoded byte size, and adds `RelayClient::request_status` for progress reads
without transferring the original request. Full relay acknowledgements and
results retain their existing JSON contract. Waiting for a testing environment's
realtime generation now retries after one second without counting a failed
backend attempt. The queue's 128 MiB admission budget is not a total memory
limit: one larger payload proceeds alone, and parsing and full responses need
additional memory. See the
[manual verification record](https://github.com/teamofsilicons/silicon-dm/blob/main/docs/cli/manual-verification.md)
and [deployment guide](https://github.com/teamofsilicons/silicon-dm/blob/main/docs/deployment.md)
for observed results and deployment prerequisites. Build from this checkout with
`cargo check -p silicon-dm-client`.
