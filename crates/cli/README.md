# silicon-dm-cli

The `dm` executable provides the complete public DM command interface and a
durable local relay for Carbon and Silicon profiles. It uses `silicon-dm-client`
for backend operations. Its private default state directory is `~/.silicon-dm`.

Install the published CLI with `cargo install silicon-dm-cli --locked`.
For local development, install this checkout with
`cargo install --path crates/cli --locked`, or run
`cargo run -p silicon-dm-cli -- --help` from the repository root.

Read [the packaged CLI guide](docs/cli/README.md) and
[the local relay guide](docs/cli/relay.md), or use `dm docs cli` and
`dm docs relay` after installation. `dm docs` indexes every offline topic;
`dm docs --search TEXT` searches the complete guides and `dm docs --all` exports
them. Topic responses are JSON with full Markdown in `content`.

Version 0.2.2 bounds queued payload work, uses lightweight status polling,
avoids redundant large JSON copies, and retries startup prerequisites without
inflating failure backoff. Its packaged guides include the Fargate deployment,
manual 100-million-character recovery checks, real IAM callbacks, and Giphy.
Use `dm docs deployment`, `dm docs verification`, and `dm docs cli-verification`
to read them offline. `dm updates check` shows the installed and published
versions; hourly automatic updates are enabled by default and can be disabled
with `dm updates disable`. A running daemon keeps its version until restarted.

Repository maintainers update the canonical guides in the root `docs/` directory
and run `python3 scripts/sync-cli-docs.py` before packaging. All embedded sources
live inside this crate, so installed packages do not depend on checkout paths.
