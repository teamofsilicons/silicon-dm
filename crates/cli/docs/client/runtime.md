# Optional Rust runtime

Enable `silicon-dm-client`'s `runtime` feature for private login storage, direct
Ting destination setup, and the durable **outgoing** command relay used by the CLI.
DM has no incoming socket supervisor, delivery queue worker or webhook forwarder.
Ting's installed system daemon owns those responsibilities.

For this checkout, use a path dependency with `features = ["runtime"]`. Build the
matching relay with `cargo build -p silicon-dm-client --features runtime --bin
dm-relay`; the executable is `target/debug/dm-relay`. A published version must
contain the same Ting migration before it can serve this runtime.

## DM login and outgoing relay

```rust
use silicon_dm_client::runtime::{DaemonCommand, LocalRuntime, LoginOptions};

let runtime = LocalRuntime::new(private_state_directory)?;
let options = LoginOptions {
    profile: "assistant",
    base_url: "https://backend.dm.teamofsilicons.com",
    short_lived_token: &dm_slt,
    webhook_url: None,
    testing_environment_id: None,
    idempotency_key: &saved_dm_login_key,
};
runtime.login(&options, &DaemonCommand::default()).await?;
let identity = runtime.login_status("assistant", None).await?;
let relay = runtime.client()?;
let outgoing_status = relay.status().await?;
```

Create a fresh operation key before the first attempt; retain it with the original
SLT for an uncertain retry. A profile cannot switch actor, organization or backend.
Login saves its tokens before starting the outgoing relay, so a launch failure can
be recovered with `start()` without consuming another SLT. Use `start_with()` and
an explicit `DaemonCommand` executable path when `dm-relay` is not on PATH.

`LoginOptions.webhook_url` must be `None`. The old `runtime.webhook(...)` method
returns migration guidance. Do not configure a legacy DM callback and assume it
understands Ting batches.

The absolute state directory and its files are private to the local OS user.
`from_environment()` checks `SILICON_DM_HOME`, configured home,
`$SILICON_HOME/.silicon-dm`, then `$HOME/.silicon-dm`. Different state directories
require different loopback ports. Set `config.relay_port` through
`runtime.store().update(...)` before starting if needed.

## Explicit grant, Ting login, and direct attachment

First call `Client::register_delivery(saved_registration_key)` with this
recipient's authenticated DM client as shown in the [client guide](README.md).
That explicit IAM-consented grant allows DM to send the recipient tings. DM login,
Ting login and reconnect do not create or restore that grant automatically.

Obtain a separate **Ting-bound SLT** for the same member and organization. DM's
access token, refresh token and DM-bound SLT are not Ting receiver credentials.
Install/start Ting's official shared system daemon before attaching a destination.
The runtime reports `daemon_unavailable` if it is missing; it does not start a DM
receive daemon as a fallback.

```rust
use silicon_dm_client::runtime::{DeliveryLoginOptions, DeliveryAttachOptions};

runtime.delivery_login(&DeliveryLoginOptions {
    profile: "assistant",
    testing_environment_id: None,
    ting_api_url: "https://backend.ting.teamofsilicons.com",
    short_lived_token: &ting_slt,
    idempotency_key: &saved_ting_login_key,
    testing: None,
}).await?;

let destination = "http://127.0.0.1:9000/tings".parse()?;
runtime.delivery_attach(&DeliveryAttachOptions {
    profile: "assistant",
    testing_environment_id: None,
    webhook_url: &destination,
    webhook_id: None, // First attachment; later calls reuse the saved stable ID.
    secret: Some(&callback_secret),
    health_url: None,
    takeover: false,
    accept_all_apps: true,
}).await?;
```

`accept_all_apps: true` is an explicit acknowledgement of Ting's contract: the
endpoint receives raw `{"tings":[...]}` for **every eligible app** in this
recipient/org, not only DM. Use a generic consumer that authenticates the secret
and `Ting-Webhook-Id`, dispatches all apps, deduplicates durably and returns HTTP
204 only after accepting the whole batch. DM callback ACK JSON and Silicon
result JSON are not translated automatically. The SDK's
`Client::hydrate_ting_batch` only validates and fetches DM messages; see
[consumer and initial-history setup](README.md#receiving-raw-ting-batches).
DM Delivered and Read receipts remain explicit HTTP operations.

The supplied URL goes directly to Ting's daemon and remains local. DM's backend
never receives it. The runtime keeps a private official Ting profile scoped to
the DM profile and environment generation, checks member type/identity and org
access, and remembers the exact hook ID. Ting owns its opaque session and local
delivery state. The DM outgoing relay does not need to stay running to receive
Ting deliveries; the generic consumer and Ting service do.

Do not create a new hook after losing an attachment response. Retry the same
attachment intent, or recover its exact `webhook_id`; uncertain attachment state
is reported instead of blindly creating another destination. `takeover: true`
explicitly transfers that same hook from another live receiver.

| Runtime method | Effect |
| --- | --- |
| `delivery_status(profile, test)` | Verifies the bound identity and asks Ting for status; does not create or reconnect a hook. |
| `delivery_reconnect(profile, test)` | Explicitly rebinds existing Ting destinations; never creates a replacement ID. |
| `delivery_unhook(profile, test)` | Detaches the saved hook while retaining its ID for explicit reattachment. |
| `delivery_logout(profile, test)` | Ends this bound Ting session and reports unconfirmed remote revocation. |
| `logout(profile, test, key)` | Revokes the DM family and disables outgoing commands for that profile. |

DM and Ting login lifecycles are separate. Use the corresponding logout operation
for the session you intend to end. A paused or detached destination requires
explicit reconnect/attachment; status checks do not silently reactivate it.

## Outgoing queue and migration

`RelayClient::submit` accepts a typed `RelayRequest`; `submit_value` preserves the
exact original JSON, including extension fields. A local ACK confirms durable
storage, not backend success. Read `request_status(request_id)` for lightweight
progress and `result(request_id)` for the backend result and original request.
Use the same request UUID and operation idempotency key across retries. These
outgoing APIs remain available to the CLI and embedding applications.

The relay serializes requests per profile and retries eligible HTTP failures.
It handles presence through HTTP device leases, not socket frames. Before each
sandbox mutation it checks `/iam` and refuses to apply an old or unknown generation
to a cleaned environment. Presence commands do not replay after relay restart.

Starting the new runtime replaces a recognized legacy DM relay process while
preserving durable state. It refuses to stop an unrecognized service on the port.
The launched executable must support the new Ting ownership contract.

Status reports `incoming_delivery.code = "delivery_moved_to_ting"` and
`forwarding = false`. New installations create no incoming inbox or cursor
tables. Existing inbox/cursor records remain unchanged for explicit inspection or
migration; `retained_legacy_deliveries` and `retained_legacy_pending_deliveries`
report them. `pending_webhooks` is zero because no DM worker forwards these rows.
Logging in or attaching a Ting hook does not replay or reinterpret them. Initialize
current history using the SDK's boundary/snapshot/sync procedure in the
[client guide](README.md#initial-history-and-recovery).

An embedding application can run `runtime.run().await` rather than launch a
process. Use one host per state directory. `runtime.client()?.stop().await`,
Ctrl-C, or cancelling `run()` stops the outgoing host; durable requests remain.
The generic Ting consumer has its own lifecycle.

## Sandboxes and credentials

Select the DM audience's testing app secret through
`select_testing_application(base_url, secret)` before a testing DM login. Use the
returned environment UUID in `LoginOptions`. Tokens must come from that IAM test
environment; a production token is not a substitute.

For `delivery_login`, supply the same environment UUID and
`DeliveryTestCredentials { app_secret, environment_key }` for **Ting's** imported
audience and the shared IAM environment. The runtime verifies these through IAM
before sending the SLT to Ting. They are not persisted by the DM helper; Ting
retains its validated session context. Never use the DM audience's app secret as
Ting's credential. Clean/restore changes the generation and requires explicit
setup for that generation; old destination state is not silently repurposed.

Rust packages update through the consuming project's Cargo manifest and lockfile.
Legacy runtime updater hooks do not modify projects. Honeycomb manages CLI updates.
