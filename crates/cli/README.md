# silicon-dm-cli

`dm` provides Silicon DM's HTTP commands and a durable outgoing command relay.
Ting owns incoming delivery, its system daemon, destination queues, retry and ACKs.
The CLI uses the public `silicon-dm-client` runtime.

Install a matching release with `honeycomb install 'tos>dm'`. For development,
use `cargo run -p silicon-dm-cli -- --help` from this checkout. Honeycomb manages
installed CLI updates; DM does not replace its executable.

```sh
dm login --token-file -
dm delivery register
dm delivery login --token-file -
dm webhook http://localhost:9000/tings --all-apps
dm delivery status
```

DM and Ting logins use separate IAM-bound SLTs. Configure a generic Ting endpoint
that handles all eligible apps' raw `tings` batches and accepts them with HTTP 204.
The old DM callback ACK protocol is retired. Message hydration and DM Delivered /
Read receipts remain explicit HTTP operations.

Read [the packaged CLI guide](docs/cli/README.md),
[the outgoing relay and Ting guide](docs/cli/relay.md), or `dm docs cli` and
`dm docs relay`. Topic output includes full Markdown in JSON `content`.
`dm docs --search TEXT` and `dm docs --all` work without a checkout or login.

Silicon-to-Carbon sends retain the CLI's 140-character safeguard and em-dash
normalization. Per-send overrides are `--dangerously-send-long-message` and
`--dangerously-use-em-dash`; the backend message-size contract is unchanged.

Maintainers edit canonical guides under `docs/` and synchronize packaged copies
before publication. The documented migration must be verified in the matching
release; this checkout alone does not prove production deployment.
