# Optional Rust relay runtime and updates

Current 0.5 guidance: [start using DM](../getting-started.md), [sandbox entry](../testing-environments.md), and [shared transport / contracts](../contracts.md). These replace older manual-pairing and per-profile connection instructions below; the standalone protocol remains compatible.

Enable `silicon-dm-client`'s `runtime` feature to use the same relay implementation
as the CLI. The normal `Client` remains stateless; choosing `LocalRuntime`
explicitly opts into local files, background work, and a loopback HTTP listener.
No IAM application secret belongs in either component.

```toml
[dependencies]
silicon-dm-client = { version = "0.4", features = ["runtime"] }
```

For development against this checkout, use a path dependency.
The `runtime` feature also builds the `dm-relay` executable. Install it with
`cargo install silicon-dm-client --features runtime --bin dm-relay`.
For local development, `cargo build -p silicon-dm-client --features runtime
--bin dm-relay` produces the executable in `target/debug`.

## Login and launch

```rust
use silicon_dm_client::runtime::{DaemonCommand, LocalRuntime, LoginOptions};

let runtime = LocalRuntime::new(private_state_directory)?;
let callback = "http://127.0.0.1:9000/events".parse()?;
let options = LoginOptions {
    profile: "assistant",
    base_url: "https://backend.dm.teamofsilicons.com",
    short_lived_token: &slt,
    webhook_url: None,
    testing_environment_id: None,
    idempotency_key: &stable_login_key,
};
let status = runtime.login(&options, &DaemonCommand::default()).await?;
runtime.webhook("assistant", None, Some(&callback))?;
let identity = runtime.login_status("assistant", None).await?;
// Later, detach callbacks while retaining the login and queues:
// runtime.webhook("assistant", None, None)?;
let relay = runtime.client()?;
let daemon_status = relay.status().await?;
```

`private_state_directory` must be an absolute directory path. The runtime
creates it privately (0700 on Unix), with credential, lock, database, and log
files restricted to the owner. Its state is compatible with the CLI's
`SILICON_DM_HOME` layout. `LocalRuntime::from_environment()` uses this exact
state override first, then the configured home, then `$SILICON_HOME/.silicon-dm`
or `$HOME/.silicon-dm`. `SILICON_HOME` must be an existing absolute directory. Different runtime directories are independent; their
listeners must use different ports. Configure a port before first launch:

```rust
runtime.store().update(|config| {
    config.relay_port = 19782;
    Ok(())
})?;
```

The callback URL is optional during login. Configure it afterward through
`runtime.webhook(profile, test, Some(&url))`; `None` removes it. Configured URLs
are validated locally and saved with the actor's tokens and stable device ID. Only the SLT goes to the backend. A profile cannot
be reassigned to a different actor, organization, or backend, which protects
its existing queues. Profile names accept 1–64 ASCII letters, digits,
underscores, or hyphens. Reuse the original login key and SLT after an uncertain
response. If process launch fails after login, the saved profile remains;
retry `start` without obtaining another SLT.

`DaemonCommand::default()` launches `dm-relay run` from PATH. Supply an absolute
executable path in `DaemonCommand` when it is not installed on PATH. `start_with`
passes the selected directory as `SILICON_DM_HOME`; it does not change the
embedding application's process environment. The CLI uses this API with its own
`dm daemon run` entry point. A detached process survives the launching command.

For a testing profile, first insert a `runtime::store::TestKey` into the
store's `testing_keys` under the DM environment UUID, then use that UUID in
`LoginOptions`. Its backend URL must match the login URL. Obtain the SLT from
the paired IAM test environment; a production token is not a test credential.

## Hosting in an existing application

An application that manages its own service lifecycle can await `runtime.run()`
instead of starting another executable. Configure its profiles first through
the store or a previous login. Run one host per state directory. The runtime
enforces its daemon lock and binds only numeric loopback.

Stop the selected host with `runtime.client()?.stop().await?`. Ctrl-C also stops
it. Dropping or cancelling the `run` future cancels its workers and account
connections; durable inboxes and outboxes remain on disk. Multiple hosts with
different directories and ports can run within one Tokio application.

`runtime.client()` returns the stateless `RelayClient`. Its `submit_value`
preserves the complete original JSON, including extension fields, in the
durable ACK and result. Its `submit` accepts a typed `RelayRequest`. Use a
stable request UUID and the original operation idempotency key when retrying.
An ACK confirms local persistence; inspect `result(request_id)` for the backend
outcome. See [relay protocol](../cli/relay.md) for request shapes.

Each incoming delivery is persisted before the backend transport ACK. With no
configured webhook, callbacks stay pending without being marked delivered;
configuring one resumes them. Unhooking retains authentication and queued work. The
runtime posts it to the profile's callback until HTTP success and an exact
`{"type":"ack","data":{"acknowledged":true,"delivery_id":"received UUID"}}` response. Recipient
message acceptance queues Delivered; Read remains explicit. A logged-out
profile keeps its pending work but no longer dispatches it. Sandbox generation
changes prevent old queued mutations from reaching a cleaned environment.
An in-flight callback cannot revive an archived delivery or enqueue its receipt
after the runtime adopts a new generation. A callback already transmitted before
the reset may still reach its recipient; the local archive prevents further retries.

Use `runtime.logout(profile, testing_environment_id, stable_key).await?` to
revoke the selected family and disable its mapping. Login, refresh, and logout
serialize per profile across processes. A refresh preserves concurrently changed
callback settings. Direct store writers that replace a family during logout
are protected by a token comparison; the result reports `newer_login_retained`.

Relay status reports `authentication_required` when loading a profile fails with
401 or 403. Log in again with a fresh IAM SLT for that profile; existing queues
and its device ID are retained. Connection diagnostics expose the failing stage
and error code without tokens or raw transport URLs.
An initial WebSocket handshake 401 first triggers token refresh, allowing a
still-valid family to recover from upstream access-token revocation without
waiting for its recorded expiry or requiring another login.

## Updates

The Rust package is an ordinary project dependency. Update it through your
Cargo manifest and lockfile, then rebuild your application. DM never runs Cargo
or changes the consuming project at runtime. Legacy `after_command` and
`automatic` hooks are no-ops. Honeycomb manages released CLI updates.
