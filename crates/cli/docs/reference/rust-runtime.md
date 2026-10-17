---
title: Rust client runtime
description: The runtime feature of silicon-dm-client, which the dm command uses. Signing a profile in, where sign-ins are stored and how they refresh, the durable outgoing relay and the sync inbox.
kind: reference
order: 63
related:
  - reference/rust-client.md
  - learn/relay.md
  - start/receiving.md
  - start/sign-in.md
---

# Rust client runtime

Enable `silicon-dm-client`'s `runtime` feature for what the `dm` CLI does: Silicon Accounts
sign-ins kept per profile, the durable **outgoing** relay, and the sync inbox. Build the
standalone relay with `cargo build -p silicon-dm-client --features runtime --bin dm-relay`.

## Signing a profile in

```rust,no_run
use silicon_dm_client::runtime::{DaemonCommand, LocalRuntime, ServiceCheck, SignInTarget};
# async fn example(slt: &str) -> anyhow::Result<()> {
let runtime = LocalRuntime::from_environment()?;           // or LocalRuntime::new(private_dir)
let target = SignInTarget::new(
    "assistant",                                           // profile
    "https://api.dm.teamofsilicons.com",               // DM API
    "https://accounts.teamofsilicons.com",                 // Silicon Accounts
    "dm",                                                  // app id
)?;
if let ServiceCheck::Unreachable(why) = LocalRuntime::check_service(&target).await? {
    eprintln!("could not check the DM service first: {why}");
}
let session = runtime.public_client(&target)?.exchange_slt(slt).await?;  // or the device flow
let status = runtime.save_sign_in(&target, &session, "slt", &DaemonCommand::default()).await?;
let verified = runtime.login_status("assistant").await?;
# let _ = (status, verified);
# Ok(()) }
```

- `check_service` asks the DM service (`GET /api/v1/accounts`) which Silicon Accounts and app id
  it trusts and refuses a mismatch before a single-use code or token is spent.
- `save_sign_in` stores the session in the profile and starts the relay (a relay that cannot
  start is reported in the result; the sign-in is kept). A profile with queued requests from
  another account is never taken over (`profile_in_use`); the replaced sign-in of the same profile
  is revoked. A profile from before Silicon Accounts releases its queued requests only to the same
  public id and fails them (`account_changed`) otherwise.
- `login_status` refreshes when needed and confirms with `GET /api/v1/me`;
  `runtime::offline_status(profile)` reads only the file, creates nothing and needs no home.
- `logout(profile)` revokes the refresh token at Silicon Accounts and forgets the tokens;
  `refresh(profile)` refreshes now; `profiles()` lists every profile without secrets.

Errors that concern the sign-in are `store::SessionError { code, message, hint }`
(`not_signed_in`, `sign_in_ended`, `sign_in_required`, `profile_in_use`, `accounts_mismatch`,
`app_id_mismatch`, `service_not_accounts`) or `accounts::SignInError`.

## Storage and refresh

`from_environment()` uses `SILICON_DM_HOME`, else the directory set with `dm config home`, else
`$SILICON_HOME/.silicon-dm`, else `$HOME/.silicon-dm`. The directory is 0700, `config.json` is 0600
and replaced atomically under `config.lock`; symbolic links are refused. A file that cannot be
read is kept as `config.json.unreadable-<time>` and replaced by defaults.

`store().fresh_profile(key)` returns the profile with at least a minute of access token left.
When it must refresh, it takes `refresh-<profile>.lock`, re-reads the file (another process may
have refreshed already), rotates the refresh token with Silicon Accounts and stores the new pair
before returning it, so concurrent processes rotate once. A refused refresh token signs the
profile out with the reason; a passing Silicon Accounts failure keeps using a token that still has
more than 10 seconds left. `with_client(profile, |client| …)` runs a call with a fresh client and
repeats it once after a 401 (nothing ran).

## The outgoing relay

```rust,no_run
use silicon_dm_client::relay::{Operation, RelayRequest};
use silicon_dm_client::runtime::{DaemonCommand, LocalRuntime};
# async fn example(runtime: LocalRuntime) -> anyhow::Result<()> {
let request = RelayRequest::new("assistant", Operation::Me);
let starter = DaemonCommand { executable: "dm".into(), arguments: vec!["daemon".into(), "start".into()] };
let ack = runtime.submit_or_queue(&request, &starter).await?;   // durable before it returns
let relay = runtime.client()?;
let result = relay.result(ack.request_id).await?;
# let _ = result;
# Ok(()) }
```

`run()` hosts the relay in this process (one per operating-system user; it refuses to start
twice), `start()`/`start_with()` attaches this home to the running relay or launches one. The
[relay guide](../learn/relay.md) describes lanes, retries, the loopback API and upgrades.

## The inbox

```rust,no_run
use silicon_dm_client::runtime::{InboxOptions, InboxStart, LocalRuntime};
# async fn example(runtime: LocalRuntime) -> anyhow::Result<()> {
let options = InboxOptions {
    profile: "assistant".into(),
    watch: true,
    wait: 30,
    start: InboxStart::Saved,
    hydrate: true,
    mark_delivered: false,
    limit: None,
};
runtime
    .inbox(&options, |line| { println!("{line}"); Ok(()) }, |notice| eprintln!("{notice}"))
    .await?;
# Ok(()) }
```

`inbox` long-polls `GET /api/v1/sync`, emits one JSON value per event (with the current message
when `hydrate`), a `cursor` value after each batch, and saves the cursor per profile after every
page. Without `watch` it returns a `done` value once nothing is pending. Passing failures are
retried with a growing pause and reported through the second callback; a `sync_reset_required`
answer emits a `reset` value and continues from a fresh cursor. `sync_page(profile, &request)`
reads one page instead. [Receive messages](../start/receiving.md) documents every line.
