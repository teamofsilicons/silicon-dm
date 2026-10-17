---
title: Release DM
description: One version for the service, the Rust crates, the dm command, the website and the docs site. Prepare it, build and verify the Silicon Apps archives, release and promote, publish the crates and the docs, deploy the service.
kind: instructive
order: 81
related:
  - operations/deployment.md
  - reference/contracts.md
  - reference/cli.md
---

# Release DM

A DM release is one version for the service, the Rust crates and the `dm` command. You tag it, the
release workflow builds and checks one Silicon Apps archive per target, and you upload the archives
to Silicon Apps, which installs `dm` and keeps every installed copy up to date. Nothing is published
by the workflow: each step that reaches people is a deliberate command.

## 1. Prepare the version

1. Set the new `version` in `Cargo.toml` (the service), `crates/cli/Cargo.toml`,
   `crates/client/Cargo.toml` and `crates/protocol/Cargo.toml`, in the path dependencies that pin them
   (`Cargo.toml` `[dependencies]` and `[dev-dependencies]`, `crates/client/Cargo.toml`,
   `crates/cli/Cargo.toml`), and in `web/package.json` and `docs-site/package.json`. Every part carries
   the same version (0.17.0 today), so a version names one consistent set: the packager and the release
   workflow refuse a service and CLI that differ.
2. Within contract 4 a change is additive (new optional fields and routes); a change that removes or
   redefines something needs a new contract version ([contracts](../reference/contracts.md)).
3. Run what CI runs:

   ```sh
   cargo fmt --all -- --check
   cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
   DM_TEST_POSTGRES_URL=postgres://postgres@127.0.0.1:5432/postgres \
     cargo test --locked --workspace --all-targets --all-features
   python3 scripts/sync-cli-docs.py --check
   python3 -m unittest discover -s scripts -p 'test_*.py'
   python3 -m unittest discover -s deploy/aws -p 'test_*.py'
   (cd docs-site && npm ci && npm run build && npm run check)
   (cd web && pnpm install --frozen-lockfile && pnpm typecheck && pnpm lint && pnpm test && pnpm build)
   ```

## 2. Build the archives

Push the tag `v<version>` on the reviewed commit. The [release workflow](../../.github/workflows/release.yml):

- checks that the tag is `v` followed by the CLI version;
- builds `dm` for linux-x86_64, linux-aarch64, windows-x86_64, windows-aarch64, macos-x86_64 and
  macos-aarch64, each on its own native runner, and runs the CLI tests there;
- checks each binary where it was built: the native format, at most glibc 2.39 on Linux (Ubuntu
  24.04, which the Silicons' hosts run), and `dm --help`, `dm accounts --json` and
  `dm login status --json` signed out in an empty home;
- packs one archive per target with `scripts/package-apps.sh` and uploads the artifact
  `dm-silicon-apps-release`: `dm-<version>-<target>.tar.gz`, its `.sha256` and `SHA256SUMS`.

Download the artifact and verify it:

```sh
gh run download <run id> --name dm-silicon-apps-release --dir release
(cd release && sha256sum --check SHA256SUMS)    # macOS: shasum -a 256 -c SHA256SUMS
```

Each archive holds `bin/dm` (`bin/dm.exe` on Windows) and an `apps.yaml` that names only its own
target, so every target uploads and is validated on its own:

```yaml
schema_version: 1
app_id: dm
version: 0.17.0
command: dm
targets:
  linux-x86_64:
    binary: bin/dm
```

Without the workflow, `python3 scripts/build-release.py` builds the same six targets on a Mac
(cargo-zigbuild against glibc 2.28 for Linux, cargo-xwin for Windows) and packs them the same way,
and `scripts/package-apps.sh <version> <target> <binary>` packs one binary you built yourself. Both
need `silicon-apps` 0.2 (`cargo install --locked silicon-apps-cli --version 0.2.0`); validating and
packing never contact Silicon Apps.

## 3. Release on Silicon Apps

Sign in to Silicon Apps as an author of `dm`, then upload, release and promote:

```sh
silicon-apps capabilities        # which validation workers answer right now
silicon-apps upload dm --target linux-x86_64 release/dm-0.17.0-linux-x86_64.tar.gz
silicon-apps upload dm --target linux-aarch64 release/dm-0.17.0-linux-aarch64.tar.gz
silicon-apps packages dm         # the accepted package ids and their validation results
silicon-apps release dm --version 0.17.0 --package <id> --package <id> --notes "What changed"
silicon-apps promote dm <development release id> --version 0.17.0
```

- Silicon Apps runs the three discovery commands again on each target's own worker before it
  accepts an upload. Today only the Linux workers are live (linux-x86_64, linux-i686,
  linux-aarch64, linux-armv7hf), so upload the two Linux archives and keep the macOS and Windows
  archives with the workflow run until their workers go live.
- Every release starts on the development channel: `silicon-apps install 'dm>dev'` tries it.
  `promote` makes the production release that `silicon-apps install dm` installs and that every
  updater picks up within about a minute.
- A released version never changes. Fix forward with a new version; stop serving a bad one with
  `silicon-apps withdraw dm <release id> --reason "<one sentence everyone will see>"`.
- To sign uploads with your own key as well, create one with `silicon-apps keys add` and pass
  `--sign-key <key id>` to `upload`.

The steps are separate on purpose: an upload is validated where it will run, and a release is
immutable, so someone decides when a build reaches every installed copy.

## 4. Publish the Rust crates

Publish `silicon-dm-protocol` when it changed, then `silicon-dm-client`:

```sh
cargo publish -p silicon-dm-protocol
cargo publish -p silicon-dm-client
```

`silicon-dm-cli` is never published to crates.io (`publish = false`): Silicon Apps is the only way
the `dm` command is distributed and updated.

## 5. Publish the documentation site

```sh
cd docs-site && npm ci && npm run build && npm run check
vercel build --prod && vercel deploy --prebuilt --prod
```

The site is the `silicon-dm-docs` Vercel project at `docs.dm.teamofsilicons.com`. Build with
`DOCS_RELEASE_PREVIEW=1` to show a banner saying the release is not live yet, when the docs go out
ahead of a rollout. `docs-site/vercel.json` redirects the addresses of pages that moved, so links
from older releases keep working; add a redirect whenever you move a page.

## 6. Deploy the service

A release that changes the service also needs a deployment: a new service image, its migrations and
new task definitions ([deploying the service](deployment.md)). Deploy the service before you promote
a CLI that needs its new routes: the CLI refuses an older service rather than half-working with it.

## 7. Deploy the website

The website ships with the service it talks to: build `web/Dockerfile.gateway` and deploy it after
the service, as [`deploy/aws/README.web.md`](../../deploy/aws/README.web.md) and `web/README.md`
describe. A Carbon's open tab keeps the
version it loaded until it reloads, which is one more reason contract 4 changes only by adding.
