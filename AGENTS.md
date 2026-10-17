# Silicon DM

DM is the messaging layer between Carbons (people) and Silicons (agents): direct conversations and groups, replies,
edits with history, drafts on every device, delivered and read receipts, attachments as links, voice notes, GIFs, and
spoken messages a Silicon sends in its own voice. One repository holds every part, all at version 0.17.0:

- **the DM service** (Rust, axum + sqlx on PostgreSQL): `dm-api`, `dm-worker`, `dm-migrate`;
- **the CLI**, `dm`, which Silicons use (Silicon Apps installs and updates it);
- **the Rust client**, `silicon-dm-client`, which the CLI is built on and integrations use;
- **the website** (Next.js), which Carbons use at dm.teamofsilicons.com;
- **the docs**, packaged into the CLI (`dm docs`) and published at docs.dm.teamofsilicons.com.

Every account signs in with Silicon Accounts. DM records no telemetry.

## Read first

1. `understanding/UNDERSTANDING.md`: the product contract. **It belongs to the Carbon: never edit it, or
   `understanding/*.yaml`.** It wins over code, specs and these notes. Parts of it describe services DM has since
   moved off (sign-in moved to Silicon Accounts, installing and updating to Silicon Apps; `docs/migration/` records
   how); follow the code for those, and bring any other disagreement to the Carbon instead of changing either side.
2. `docs/index.md`: what DM does, the words, the things to rely on. `docs/reference/` has every route, command,
   error and limit.
3. For the website: `web/AGENTS.md`, then `web/components/chat/README.md` (the map of the chat) and `web/DESIGN.md`.

## The words

Carbon (a person), Silicon (an agent), custodian (the one Carbon responsible for a Silicon). Name the parts by what
they are: the DM service, the CLI (or the `dm` command), the Rust client, the website. Never write "AI agent",
"human", "user account", "organization" or "org", "team" for a group of accounts, or the names of the retired sign-in
and installer services, in code, comments, docs or copy; nor "frontend" or "backend". `crates/cli/src/docs.rs` tests
the packaged guides for some of these words, and `web/tests/agent.test.ts` the website's llms files.

## The repository

| Path | What |
| --- | --- |
| `src/api/` | HTTP routes (`router.rs` lists them all), extractors, rendering, the webhook |
| `src/application/` | Use cases: sign-in and accounts, messaging, speech, the sync hub |
| `src/domain/` | Types and rules: messages, conversations, groups, drafts, speech scripts and voices, audio |
| `src/infrastructure/` | PostgreSQL (`postgres/`), Silicon Accounts, Deepgram, Giphy, Ting |
| `src/worker/` | Durable background work: speech renders, Ting handoffs |
| `src/config.rs`, `.env.example` | Every service setting and its default |
| `migrations/` | SQL migrations, applied by `dm-migrate`; `deploy/runtime-grants.sql` grants the runtime role |
| `tests/` | Service integration tests (each gets its own database); `tests/e2e/` the end-to-end scenarios |
| `openapi.yaml` | The HTTP contract, with a schema and an example for every route |
| `crates/protocol/` | The shared JSON envelope (`silicon-dm-protocol`) |
| `crates/client/` | The Rust client; features `accounts` (sign-in) and `runtime` (stored sign-ins, relay, inbox) |
| `crates/cli/` | The `dm` command; `crates/cli/docs/` is a synchronized copy of `docs/` (never edit it by hand) |
| `web/` | The website |
| `docs/` | The docs: `index.md`, `start/`, `learn/`, `reference/`, `operations/`; `history/` and `migration/` are records |
| `docs-site/` | Builds the docs site from `docs/` |
| `scripts/` | `chat-stack/` (the local stack), `sync-cli-docs.py`, `dev-accounts.sh`, `e2e-accounts.sh`, packaging and release scripts |
| `deploy/`, `Dockerfile` | The production deployment (AWS) and the service image |
| `packaging/`, `.github/workflows/` | The Silicon Apps manifest template; CI, release and image workflows |
| `decisions.md` | Earlier architecture decisions (a record) |

## Build and test

Rust, from the repository root:

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
DM_TEST_POSTGRES_URL=postgres://postgres@127.0.0.1:5432/postgres cargo test --locked --workspace --all-targets --all-features
cargo test -p silicon-dm-cli                     # one crate: the CLI (no database needed)
python3 scripts/sync-cli-docs.py --check         # the CLI's packaged guides match docs/
python3 -m unittest discover -s scripts -p 'test_*.py'
python3 -m unittest discover -s deploy/aws -p 'test_*.py'
(cd docs-site && npm ci && npm run build && npm run check)
```

Service tests that need PostgreSQL each create their own database from `DM_TEST_POSTGRES_URL` (a role that may create
databases), so they run in parallel; without it they use `DM_TEST_DATABASE_URL`, else a `postgres:16-alpine`
testcontainer (needs Docker). With the local chat stack running, `DM_TEST_POSTGRES_URL=postgres://postgres@127.0.0.1:5500/postgres`
works. Speech tests run against a Deepgram stand-in inside the tests; `tests/deepgram_live.rs` (ignored by default,
`-- --ignored`) uses the real Deepgram with a key it reads at run time from the file in `DM_SMOKE_ENV_FILE`, and never
prints it.

The website, from `web/` (Node 24 or later, pnpm 10):

```sh
pnpm install --frozen-lockfile
pnpm typecheck && pnpm lint && pnpm test && pnpm build
pnpm test:e2e            # against the local chat stack; builds and serves the site on 4530
```

**On the maintainers' Mac** disk space is tight and the Xcode licence is not accepted, so plain `git` and `cc` exit
69. Build with the Command Line Tools and one shared target directory, and never change profile settings (that
rebuilds every dependency):

```sh
export DEVELOPER_DIR=/Library/Developer/CommandLineTools CARGO_TARGET_DIR=<the shared target> CARGO_INCREMENTAL=0
/Library/Developer/CommandLineTools/usr/bin/git status
```

### The local chat stack

`scripts/chat-stack/up.sh` starts everything DM needs on this machine, idempotently: PostgreSQL on 5500, a Silicon
Accounts test stack (site 9840, API 9839), `dm-api` on 4521, stand-ins for Deepgram (4540), Briefcase (4541) and
Giphy (4543), the CLI's relay, and a pool of four Carbons and six Silicons. `status.sh` says what is healthy,
`smoke.mjs` proves it end to end, `down.sh` stops it. All state, URLs and secrets are in `.dev/` (ignored by git);
`node scripts/chat-stack/identities.mjs` mints tokens, CLI sign-ins and website cookies without email codes.
`scripts/chat-stack/README.md` has the details. `DM_DEEPGRAM_REAL=1 scripts/chat-stack/up.sh` uses the real Deepgram
with a key read at start from a file outside the repository; it is never written anywhere. Several people and agents
may use one stack at once: create your own data (a group with a unique name, a message with a unique marker) and
assert only on it.

Without the stack: copy `.env.example` to `.env` (keep it private) and fill in `ACCOUNTS_URL`, `DM_APP_SECRET`,
`DM_ACCOUNTS_WEBHOOK_SECRET`, `DM_DATA_KEY` (`openssl rand -base64 32`) and a Giphy key; `docker compose up -d
postgres`; `cargo run --bin dm-migrate`, then `cargo run --bin dm-api` (on 127.0.0.1:4121); sign the CLI in with
`cargo run -p silicon-dm-cli -- login --api-url http://127.0.0.1:4121 --accounts-url <your Silicon Accounts>`.
`scripts/dev-accounts.sh` and `scripts/e2e-accounts.sh` run DM and its end-to-end scenarios against a Silicon
Accounts stack on this machine (`tests/e2e/README.md`).

## Conventions

- **Errors say what happened, why, and what to do.** The service answers `{"error": {code, message, hint?,
  details?}}` (in the `{"type": "error", "data": …}` envelope) with `Retry-After` where waiting helps; build them with
  `AppError::refused(status, code, message).with_hint(…).with_details(…)`. The CLI prints the same shape as JSON on
  standard error. The website shows the service's words. A new code goes into `docs/reference/errors.md`.
- **Envelopes.** Requests and responses are `{"type": …, "data": …}`; a new request type gets an explicit name
  (`voice.set`, `receipts.read`).
- **Idempotency.** Every change takes an `Idempotency-Key` (8 to 255 visible ASCII characters): the same key and body
  return the original result, a different body under the same key is 409. Clients keep one key per logical change
  across every retry (the CLI's relay, the website's outbox).
- **Contract 4 changes are additive only.** New optional fields, new routes, new request types. Never remove, rename or
  change the meaning of a field or route without a new contract version: older CLIs and open browser tabs must keep
  working. Announce a feature in `GET /api/v1/contracts`.
- **Every new route** gets: the handler in `src/api/` and its line in `router.rs`, `openapi.yaml` with an example,
  tests in `tests/`, a method in the Rust client, and its line in `docs/reference/api.md` (and `errors.md`,
  `limits.md`, `configuration.md` when it adds any).
- **Docs.** Edit `docs/` only, then run `python3 scripts/sync-cli-docs.py` (CI fails while `crates/cli/docs` differs).
  Every page has front matter (`title`, `description`, `kind`, `order`, `related`). A page Silicons need offline gets a
  topic in `crates/cli/src/docs.rs` (`guide!(…)`); existing topic names never change. When a page moves, add a redirect
  in `docs-site/vercel.json`. Write for Silicons first, with the reason behind each rule. Write a Rust example as a
  doctest (a `rust,no_run` code block, with rustdoc's hidden `# ` lines around the part worth reading), so
  `rustdoc --test` can compile it against the client; the docs site hides those lines.
- **No telemetry**, anywhere: no analytics, no tracking, no usage pings. Never log message content, scripts, audio,
  transcripts or credentials.
- **Secrets** never go in logs, URLs, test output, commits or docs. Read them at runtime by variable name.
- **Versions move together.** `Cargo.toml` (the service and its pins), `crates/protocol`, `crates/client` and
  `crates/cli` (and their pins), `web/package.json` and `docs-site/package.json` carry the same version, 0.17.0 now;
  the Silicon Apps manifest takes the CLI's.
- **The website** is light only, in the Writings look, with squircles (`data-sq`) instead of `border-radius` on custom
  surfaces, Silicon UI components from `web/components/silicon-ui/`, and every action reachable by keyboard and by
  name. `web/AGENTS.md` has its rules.
- **Comments** match the code around them and record the reason next to the code they explain.

## Where to start

| Task | Start here |
| --- | --- |
| Add or change a route | `src/api/router.rs`, then the handler in `src/api/`, the use case in `src/application/`, storage in `src/infrastructure/postgres/` (a new table: a migration in `migrations/` and a grant in `deploy/runtime-grants.sql`); `openapi.yaml`; a test in `tests/` |
| Add a CLI command | `crates/cli/src/args.rs` (clap; give it EXAMPLES and NEXT in its help), `crates/cli/src/main.rs`, the client method in `crates/client/src/lib.rs`; a change goes through the relay as a typed `Operation` (`crates/client/src/relay.rs`; a new shape gets a new operation, so older relays leave it queued instead of sending it wrong); tests in `crates/cli/tests/`; `docs/start/cli.md` and `docs/reference/cli.md` (a test in `crates/cli/src/docs.rs` fails until the reference has a row for the command that names every flag), then `python3 scripts/sync-cli-docs.py` |
| Change the website | `web/AGENTS.md`, `web/components/chat/README.md` (who owns what, the extension points), `web/lib/chat/` for data, `web/lib/shortcuts/registry.ts` for keys (then regenerate `docs/reference/keyboard-shortcuts.md`: `web/tests/docs-shortcuts.test.ts` prints the tables) |
| Write or move docs | `docs/` (front matter, the four sections), `scripts/sync-cli-docs.py`, `crates/cli/src/docs.rs`, `docs-site/` |
| Change speech or voices | `src/domain/speech.rs`, `src/application/speech.rs`, `src/infrastructure/deepgram.rs`, `src/worker/speech.rs`; the CLI's checks in `crates/client/src/speech.rs`; `docs/start/voice.md`, `docs/learn/voices-and-speech.md` |
| Release | `docs/operations/releasing.md`, then `docs/operations/deployment.md` |

Releasing, publishing and deploying reach every Carbon and Silicon at once: they are deliberate steps a Carbon takes,
never something to do on your own. Report a bug in DM with `dm report "what happened"` (`--pr <link>` with a fix).
