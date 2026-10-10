# DM migration to Silicon Accounts + Silicon Apps: progress log

Branch `migrate/accounts-apps-20261010` (based on `origin/main` e175aa4, DM 0.14.1). Each stage appends a dated
section: what it did, commits, test commands and results, what is left, gotchas. Decisions live in
[decisions.md](decisions.md).

## 2026-10-10 — Stage 1 (service): baseline before any change

Environment: Rust 1.98.0 (rust-toolchain.toml), `CARGO_TARGET_DIR=target/mig`, Postgres 16.11 on 127.0.0.1:5460 (trust),
no Docker (the daemon hangs). 96 GB free on /System/Volumes/Data.

| command | result |
|---|---|
| `cargo test --locked --workspace --all-features --no-run` | builds (vendored `silicon-iam-client` is a workspace member through its path dependency) |
| `cargo test --locked --workspace --all-features --lib --bins` | pass: silicon_dm 52, dm (CLI main) 4, silicon_dm_client 46, silicon_dm_protocol 3, vendored silicon_iam_client 47 |
| `cargo test --locked -p silicon-dm-cli -p silicon-dm-client -p silicon-dm-protocol --all-features --all-targets` | pass: CLI 4 + delivery 3 + discovery 1 + em_dash 1 + home 2 + message_length 1; client 46 + ting_http 4; protocol 3 |
| `cargo test --locked -p silicon-dm --all-features --test wire_contract --test ting_transport` | pass: 7 + 7 |
| DB suites, one fresh database per test function (`DM_TEST_DATABASE_URL=postgres://postgres@127.0.0.1:5460/dm_t_…`, runner script in the stage scratchpad) | 13/13 pass: http_delivery_routes (1), http_sync_presence (1), postgres_durable_flow (4), ting_authorization (1), ting_credentials (1), ting_handoffs (1), ting_publisher (1), ting_worker (1), understanding_contract::{bundle_commands_and_history_are_atomic_and_retry_safe, managed_generation_is_independent_of_credential_and_lifecycle_revisions} |
| Docker-only (call testcontainers directly, ignore `DM_TEST_DATABASE_URL`) | 10 fail without Docker after testcontainers' 120 s start timeout (pre-existing, environmental): group_id_migration::upgrade_keeps_existing_groups_and_assigns_unique_stable_addresses, identifier_schema_migration::cutover_preserves_conversation_and_exact_replay_receipt, message_history_migration::upgrade_preserves_edits_and_keeps_deletion_out_of_history, ting_cutover_hold::held_handoffs_preserve_exact_rows_and_fail_closed, understanding_contract::{app_secret_discovery_is_isolated_revalidated_and_resets_generation, groups_work_through_sdk_and_http_with_admin_and_retry_guards, honeycomb_lifecycle_is_authenticated_fenced_and_retry_safe, recipient_addressed_messages_use_stable_local_codes_and_server_owned_replies, reports_retry_postmark_and_deduplicate_without_sending_test_email, retired_sockets_leave_http_actor_scope_and_contract_lifecycle_intact} |

So every regression after this point is a test that passed above and fails later.

## 2026-10-10 — Stage 1 (service): done

The service authenticates and authorises with Silicon Accounts only, has no Teams, keeps every byte of existing data
and passes its tests. A first session did the whole stage but stopped before committing (the work was found
uncommitted and intact); this second session reviewed it against the stage checklist, re-ran every gate, fixed what it
found, checked it live against the shared local Accounts stack and committed it in logical steps.

What the service is now (details and every judgement call: [decisions.md](decisions.md)):

- **Dependencies:** `silicon-accounts-client = "0.4.0"`; `vendor/silicon-iam-client`, its Docker `COPY` and
  `.dockerignore` entry are gone. `cargo deny` ignores RUSTSEC-2023-0071 (`rsa`, via jsonwebtoken's rust_crypto
  backend; DM only verifies EdDSA with public keys). Workspace 0.15.0, protocol 0.13.0, HTTP contract 4.
- **Configuration:** `ACCOUNTS_URL` (issuer), `ACCOUNTS_API_URL` (optional), `DM_APP_ID` (default `dm`),
  `DM_APP_SECRET`, `DM_ACCOUNTS_WEBHOOK_SECRET` (`whsec_…`), `DM_ACCOUNTS_REQUEST_TIMEOUT_SECONDS`, `DM_DATA_KEY`
  (base64 32 bytes: sync-cursor MAC and stored-proof encryption), `DM_PROOF_ISSUERS`, `DM_TING_URL` (unset = Ting
  off). Boot validation with exact messages; plain http only for loopback hosts; obsolete `DM_IAM_*`, `DM_TEST_*`,
  `DM_HONEYCOMB_*` and `DM_TING_BASE_URL` are ignored and logged once.
- **Authentication:** one extractor for every API route: Bearer JWT verified locally (JWKS cached, refetched on an
  unknown `kid` at most every 30 s), `VerifyOptions::for_app("dm").with_issuer(ACCOUNTS_URL)`; per-account
  `revoked_before` cutoff from webhooks; introspection (cached 30 s) on group owner/settings changes, member removal,
  message deletion and allow-list changes; `Authorization: Proof sap_…` on the conversation, message, receipt, draft
  and sync routes for the scopes and issuers in `DM_PROOF_ISSUERS`. IAM routes, testing environments, the Honeycomb
  control plane and the retired WebSocket code are deleted.
- **Accounts and the circle:** `dm.accounts` cache (uuid text `COLLATE "C"`, kind, current id, name, photo,
  custodian, cutoff) filled from tokens, lookups (budget 550/min), DM's user base and webhooks; ids resolved through
  Accounts with exact errors (`account_not_found`, `account_deleted`); custodian powers bounded (read, tombstone the
  Silicon's messages, manage its groups and allow-list, never act as it).
- **No Teams:** see decisions.md (`organization_id` kept as a storage partition `@accounts`; one canonical direct
  conversation per participant set; co-equal group owners and accepted invitations; tag/public groups materialised).
- **Data:** migrations 0039 (additive tables and columns) and 0040 (behaviour) only; `dm-migrate link-identities
  --file mapping.csv [--dry-run] [--offline] [--force-relink]` re-keys in one transaction, records
  `dm.identity_links` / `dm.identity_link_runs`, reports unmatched ids; `dm-migrate draft-identity-mapping --out F`.
- **Webhook:** `POST /webhook/`, raw-body verification (5-minute tolerance), dedupe in
  `dm.accounts_webhook_receipts`, ordering by `occurred_at` / account version, all six events plus ping; unknown types
  2xx; 401 bad signature, 400 bad body.
- **Ting:** adapter off unless `DM_TING_URL`; handoffs kept and marked discarded; `GET /api/v1/sync?wait=≤30`
  long-polls on `LISTEN dm_delivery`. With Ting: User verification enrolment (`tings.subscribe`, refresh token
  encrypted with `DM_DATA_KEY`), cached App verification proof for sends (`tings.send`).

### Commits

| commit | subject |
|---|---|
| 9d37f94 | Record service baseline and Accounts migration decisions |
| db012a8 | Add Silicon Accounts identity schema without dropping IAM-era data |
| 025952c | Move the DM service to Silicon Accounts with personal accounts |
| ac6e84c | Document HTTP contract 4: Silicon Accounts sign-in and no organizations |
| da1594c | Grant the runtime role the per-group roster function |
| 8239fa1 | Accept a sign-in made in the same second as a sign-out |
| bbafe25 | Let read proofs follow the sync feed |
| 180770a | Test sign-in on every route family, GIFs, reports and diagnostics |
| d901936 | Describe who can reach a Silicon without account-grouping words |
| ce6e283 | Test the Ting worker's retry backoff |
| (last) | Record stage 1 results, decisions and the cutover runbook |

Every commit type-checks (`025952c` was checked from an export of its index before committing).

### Tests (run from the worktree with the brief's Rust env)

| command | result |
|---|---|
| `cargo fmt --all -- --check` | ok |
| `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` | ok |
| `cargo deny --all-features check` | advisories ok, bans ok, licenses ok, sources ok |
| `npx --yes @redocly/cli@2.49.0 lint openapi.yaml` | valid (4 problems explicitly ignored in `.redocly.lint-ignore.yaml`) |
| `DM_TEST_POSTGRES_URL=postgres://postgres@127.0.0.1:5460/postgres cargo test --locked --workspace --all-features` | 160 passed, 0 failed (24 binaries): silicon_dm lib 44, account_routes 3, accounts_auth 6, accounts_authorization 5, accounts_schema_migration 2, accounts_webhook 6, link_identities 6, messaging_flow 6, ting_delivery 4, ting_transport 7, wire_contract 6, dm CLI 4 + delivery 3 + discovery 1 + em_dash 1 + home 2 + message_length 1, client 46 + ting_http 4, protocol 3 |

One run of the full suite (while other agents were building) failed the unchanged client test
`runtime::daemon::tests::outgoing_relay_retries_http_without_connecting_or_forwarding_incoming_delivery` on its 8-second
deadline; it passed 5/5 alone, 3/3 in the client suite and in the next full run. It is timing-sensitive under load,
not a regression.

**Against the baseline:** every test that passed at baseline still passes, or was removed together with the feature it
tested: the vendored IAM client's 47 tests; the IAM session, directory and OBO tests; testing environments and
sandboxes; the retired WebSocket hub, protocol and session (and `wire_contract`'s WebSocket round trip); the legacy
delivery worker; IAM-era Ting credentials and authorisation. Renamed or replaced: `bearer_scheme_is_exact_and_nonempty`
→ `credentials_are_one_exact_bearer_or_proof`; `giphy_api_key_is_required` and the IAM app-id test →
`configuration_scenarios_load_and_fail_exactly` / `app_ids_follow_the_published_contract`; the Ting enrolment unit test
→ `ting_delivery::recipients_are_enrolled_with_their_own_proof_and_reenrolled_from_the_stored_one`; the client's Ting
sandbox test → a test that the sandbox login is refused; `ting_transport`'s test-credential case →
`invalid_local_requests_are_not_transmitted`. The 13 DB tests and the 10 Docker-only tests of the baseline are covered
by `messaging_flow`, `accounts_*`, `link_identities`, `ting_delivery` and the upgrade test, which loads a dataset the
0.14.1 code wrote (`tests/fixtures/legacy-0038.sql`) into a database migrated to 0038 and upgrades it.

**Migrations:** `accounts_schema_migration::empty_database_applies_every_migration` and
`upgrade_from_0038_keeps_every_legacy_row` (the original columns of every row in 21 legacy tables keep their values,
checksummed; `group_invitations` only gains the materialised public/tag memberships; supersession, owners, addresses
and contract versions as decided);
`link_identities` covers the dry run, the re-key of every identity column with all invariants restored, idempotent
re-runs and corrected mappings, the `dm-migrate` binary, and the service serving re-keyed legacy data.

**Live, against the shared local Silicon Accounts stack** (`dm-api` on 127.0.0.1:4121, fresh `dm_e2e_svc` database;
scripts in the session scratchpad `dm-svc/`: `e2e-start.sh`, `e2e-start-runtime.sh`, `e2e_svc.py`, `e2e_svc_2.py`,
`e2e_lib.py`; identities minted with `mint.mts`):

- `e2e_svc.py` 35/35: discovery; a Carbon (code flow) and a Silicon (SLT) admitted with real tokens; name from the
  user base; custodian learned by lookup; the Silicon ↔ custodian conversation exists; sync cursor and a long-poll
  that returns the new message; sender rendered by current id + uuid; outsider refused (`contact_not_allowed`), a
  Carbon reachable by anyone; custodian `?for=`; allow-list change through live introspection; a token for
  `interface` refused; Interface's real User verification proof accepted for reads and sync, refused for writes
  (`proof_scope_missing`) and other routes (`proof_not_accepted`); real signed webhooks: ping, `account.updated`
  (new name shown), `account.id_changed` (conversation address and history follow the new id; an older token does not
  move it back), `membership.signed_out` `app_revoked` (recorded as ignored, nothing else ends).
- `e2e_svc_2.py` 24/24: `membership.access_removed` (older token refused, a new sign-in works); STK rotation
  (`stk_rotated`: older token refused, the immediate new sign-in, 1 s later, works); a custody transfer
  (`silicon.custodian_changed`: new custodian reads, gets the conversation, former custodian refused, existing
  conversation keeps working); the Silicon's deletion (`account.deleted`: sign-ins refused, conversation kept with
  the Silicon shown as deleted, its messages kept, new messages 409 `recipient_deleted`).
- Both suites again with `dm-api` connected as a role holding only `deploy/runtime-grants.sql`: 35/35 and 24/24.
- dm's webhook on the stack was moved to the local service for these runs and restored (verified).

### Left for later stages

- **CLI / client stage:** `crates/client` and `crates/cli` still speak the 0.14 flows against a service that no longer
  serves them: `Client::with_auth(token, org)` sends `X-Org-ID` (ignored now), login/refresh/logout go through DM's
  removed `/api/v1/auth/*`, discovery calls the removed `/api/v1/iam`, the Ting receiver login still reads `/v1/iam`,
  and models carry `org_id`, tag/public group fields and testing types. Move them to D5 (device flow, `--slt`,
  `--slt-stdin`, positional SLT and `--token-file` aliases, hidden `dm iam --json`, `dm accounts --json`,
  `dm login status --json`, `dm allow …`, `dm sync [--wait]`, `dm inbox --watch`), actor refs with `uuid`, contract 4
  (`GET /me`, `GET /accounts`, owners/invitations/leave, `/allow-list`, `/sync?wait=`). The protocol crate keeps the
  contract-3 route type names only for that client; drop them with it. Sync `crates/cli/docs/openapi.yaml` (and the
  other bundled topics) from the root `openapi.yaml` / docs with `scripts/sync-cli-docs.py`.
- **Web stage:** the Next.js BFF calls the service with `Authorization: Bearer` (Carbons only); custodian views use
  `?for=si:…`; updates come from `GET /api/v1/sync?wait=N`; the allow-list is `/api/v1/allow-list?for=si:…`.
- **Ship / packaging stage:** `deploy/aws/fargate.yaml`, `production.yaml`, `bootstrap.sh`, `bootstrap-task.py` and
  the deploy READMEs still pass `DM_IAM_*`, `DM_TEST_*` and `IamBaseUrl`; switch them to the variables in
  [cutover.md](cutover.md) (`deploy/aws/test_bootstrap_grants.py` checks the bootstrap). `honeycomb.yaml`,
  `scripts/build-honeycomb-release.py`, the Honeycomb release workflow, `docs-site` (IAM banner, install.sh) and the
  IAM-era docs still exist.

### Blocked on

- Ting accepting Silicon Accounts proofs: the adapter is ready and tested against a recording publisher and a stub;
  `DM_TING_URL` stays unset, so handoffs are kept and marked discarded and clients sync over HTTP.
- Silicon Interface switching to User verification proofs (receiving app `dm`, the five scopes); DM accepts them from
  the issuers in `DM_PROOF_ISSUERS`. Recorded in cutover.md: it must ship in the same window.
- Production registration of the two-character app id `dm` in Silicon Accounts (handled by the orchestrator).

### Gotchas

- Accounts uuids are short, case-sensitive text: never lowercase or cast them (unit test
  `uuids_differing_only_in_case_are_different_accounts`). Legacy ids always contain `:`, uuids never do.
- Test databases: `DM_TEST_POSTGRES_URL` (a server URL) gives each test its own `dm_t_*` database, dropped when the
  test passes; `DM_TEST_DATABASE_URL` is one fresh database for the whole process; with neither, tests start a
  testcontainer (needs Docker, which hangs here).
- A token's `iat` has whole seconds and webhook times have milliseconds (see decisions.md).
- In zsh, never name a shell variable `path`: it is `PATH`, and every later command in the line is "not found".
- When moving the shared stack's `dm` webhook for a test, read it first and put it back (`PUT` keeps the secret;
  never rotate it, other runs use the seeded secret from `test-stack.json`).
- Old clients' sync cursors fail once with `sync_reset_required` (the cursor key is now `DM_DATA_KEY`).

## 2026-10-10 — Stage 2 (client crate and CLI): done

`silicon-dm-client` 0.15 and the `dm` CLI speak Silicon Accounts only, follow the Silicon Apps CLI contract and work
against the migrated service. Every judgement call is C1–C28 in [decisions.md](decisions.md); the CLI part of the
runbook is "CLI and Silicon runtimes" in [cutover.md](cutover.md); contract wording is in
[understanding-proposal.md](understanding-proposal.md).

Baseline before any change (same commands as below): CLI 12 tests, client 46 + 4, protocol 3, all passing; one client
lane test (`profile_wide_requests_keep_order_with_everything_before_them`) failed once and passed 3/3 on rerun: it
crossed a one-second backoff boundary. It is fixed (the test now pushes the retry well into the future).

What changed:

- **Client (`crates/client`)**: `Client::with_token` / `with_proof` (Bearer or `Proof sap_…`), contract 4 header, no
  organization header; `accounts`, `me`, `delivery`, allow-list, owners, invitations, accept/decline/leave,
  `conversations_for` (custodian `?for=`), `send_to_recipient`, `sync` with `wait`. Models: `Actor` with `uuid`
  (and name, photo, deleted, legacy), `Me`, `Delivery`, `SignInDiscovery`, `AllowList`/`AllowListEntry`,
  `Invitation`, owners/members/pending in `GroupDetails`, supersession fields. Errors keep `hint`; `code()`,
  `message()`, `hint()`, `status()`, `is_code()`. Removed: IAM sign-in, test environments, delivery
  registration/authorization, `check_update`, WebSocket shims and frame types. The `ting` module validates schema-2
  references (recipient `{uuid,id}`); schema-1 references are `LegacyReference`.
- **`accounts` feature**: `PublicClient` (device flow, short-lived token exchange, refresh, revoke) with classified
  `SignInError`s.
- **Runtime**: profile format 2 in `config.json` (sign-in, account, DM and Accounts URLs), single-flight refresh under
  the profile lock, legacy profiles signed out with their old id, unreadable state kept aside, `save_sign_in` (account
  switch and legacy-queue rules, replaced sign-in revoked), `login_status`, `offline_status`, `logout`, `refresh`,
  `profiles`, `with_client`, `sync_page`, `inbox`; relay without sandbox generations, `sync_feed` status marker,
  retired test-environment requests failed; `ting_delivery` and `updates` removed with `silicon-ting-client`.
- **CLI (`crates/cli`)**: new command tree (`args.rs`), `auth.rs` (login/accounts), `receive.rs` (sync/inbox),
  `output.rs` (JSON errors with hints); discovery, docs and signed-out status before any runtime; retired commands
  answer `command_retired`; `publish = false`.
- **Docs**: every packaged guide rewritten (new `accounts.md`, `cli/receiving.md`); IAM/Honeycomb-era records in
  `docs/history/`; sync script with `--check` (CI step); docs site skips history/migration; READMEs, deployment and
  configuration guides; OpenAPI corrections (allow-list entries, draft `message_content`, owners `Idempotency-Key`).
- **Service test** `tests/wire_contract.rs` follows the client API (Bearer, contract 4; the retired WebSocket frame
  test became a rendered-account round trip).

### Commits

| commit | subject |
| --- | --- |
| e2b38fe | Move the DM client and CLI to Silicon Accounts |
| 9ba6733 | Read allow-list entries as the service sends them |
| 5140793 | Rewrite the DM guides for Silicon Accounts and Silicon Apps |
| e390858 | Record stage 2 results, decisions and the CLI cutover |
| (last) | Test that the stored sign-in stays owner-only and atomic |

### Tests (from the worktree with the brief's Rust env)

| command | result |
| --- | --- |
| `cargo fmt --all -- --check` | ok |
| `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` | ok |
| `cargo deny --all-features check` | advisories ok, bans ok, licenses ok, sources ok |
| `DM_TEST_POSTGRES_URL=postgres://postgres@127.0.0.1:5460/postgres cargo test --locked --workspace --all-features` | **163 passed, 0 failed** (service 44 + 3 + 6 + 5 + 2 + 6 + 6 + 6 + 4 + 7 + 6; CLI unit 6, accounts 8, discovery 1, em_dash 1, home 2, message_length 1, receive 3, state 4; client 35 + http_contract 4; protocol 3); afterwards a 36th client test (file modes, atomic replace, symbolic link refused) passes too |
| `python3 scripts/sync-cli-docs.py --check` | `crates/cli/docs matches 18 canonical files` |
| `npx --yes @redocly/cli@2.49.0 lint openapi.yaml` | valid (4 problems explicitly ignored, as before) |
| `cd docs-site && npm ci && npm run build && npm run check` | 17 pages built and verified |
| `cargo package --list --allow-dirty --offline -p silicon-dm-client` / `-p silicon-dm-protocol` | file lists as expected (sources, README, tests) |

New tests: `crates/cli/tests/accounts.rs` (real binary against an axum stand-in for Silicon Accounts and DM: discovery
golden JSON in an empty home with no files written; device flow pending → slow_down → approved with the poll
intervals measured; denied; expired (answer and deadline); Silicon `--slt-stdin` with the token in no file and no
output, 0600/0700; positional, `--slt` and `--token-file` forms; every refusal code without echo; service trusting
another Accounts refused before spending; three concurrent commands rotating once; a refused refresh signing the
profile out), `tests/state.rs` (0.14 profile + queued send released to the same id, failed for another id, retired
test key not written back; damaged state kept aside; retired commands and flags), `tests/receive.rs` (sync pages,
inbox lines with hydration, `--mark-delivered`, inaccessible, saved-cursor resume, `--watch` until SIGTERM exit 0),
`src/command_tests.rs` (documented forms parse, removed ones do not, every visible help page has a description and no
retired words), client `runtime_tests.rs` (legacy profile, damaged file, eight concurrent refreshes rotating once,
refused refresh, unreachable Accounts, account takeover refused, revoke forms, 0700/0600 modes with no temporary file left and a symbolic link refused), `accounts.rs` unit tests (refusal
codes, URLs, non-`slt_` input never sent), `tests/http_contract.rs` (Bearer/Proof/contract header, custodian and
allow-list routes, owners idempotency, sync long-poll query, errors with hints, Ting hydration without ACKs).

### Live, against the shared local Silicon Accounts stack and the migrated service

Service: `dm-api` on 127.0.0.1:4121 with a fresh `dm_e2e_cli` database (`<session scratch>/dm-cli/start-api.sh`);
CLI env per home: `HOME=SILICON_HOME=<fresh dir>`, `SILICON_DM_RELAY_HOME=<dir>/relay`, `DM_API_URL=http://127.0.0.1:4121`,
`ACCOUNTS_URL=http://localhost:9590`. Identities from `mint.mts` (`dm-cli-c1-<run>`, `dm-cli-s1-<run>`,
`dm-cli-c2-<run>`). Scripts: `<session scratch>/dm-cli/e2e_cli.py`, `e2e_cli_2.py` (never print secrets).

- `e2e_cli.py` run 602814: **30/30**. Discovery in an empty `HOME`/`SILICON_HOME`: `--help` exit 0 (58 lines);
  `accounts --json` exit 0 →
  `{"accounts_url":"https://accounts.teamofsilicons.com","api_url":"https://backend.dm.teamofsilicons.com","app_id":"dm","client_id":"dm","contract_version":4,"device_flow":true,…,"version":"0.15.0",…}`;
  `login status --json` → exactly `{"authenticated":false}`; nothing written. Silicon: `mint slt` →
  `dm --json login --slt-stdin` → `{"uuid":"2VO","id":"si:dm-cli-s1-602814","kind":"silicon","method":"slt","custodian":{"uuid":"qB5",…}}`;
  token in no file or output; `config.json` 0600; `login status --json` → `verified: true`; `whoami` through the relay
  → account and custodian; the conversation with its custodian exists (`c:dm-cli-c1-602814::si:dm-cli-s1-602814`);
  `messages send c:… --wait` → completed, code `000`, sender rendered with uuid. Refusals from the real Accounts: reused
  token → `slt_already_used` ("…already used; each one works once…"), a token for `remind` → `slt_wrong_app` naming
  'remind', hints name `silicon-accounts login --app dm -q`. Carbon: `dm --json login` printed
  `{"event":"device_code",…,"verification_uri":"http://localhost:9590/device"}`; `mint approve --email … --code …`;
  the CLI finished as `c:dm-cli-c1-602814` (`method: device`); status lists the Silicon in `custodian_of`;
  `conversations list --for si:…` (custodian view); `inbox --from beginning` shows the Silicon's message. `inbox --watch
  --mark-delivered` started from the saved cursor and printed the Silicon's next message 0.04 s after the send command
  started, with `delivered_receipt: true`; SIGTERM → exit 0. Allow-list: the Silicon allowed `c:dm-cli-c2-…`, its
  custodian read the list with `--for`, the Silicon removed it. Logout (both): `revoked: true`, then
  `{"authenticated":false}`; `whoami` → `not_signed_in` with the sign-in hint.
- `e2e_cli_2.py` run 602878: **16/16**. Positional `dm login <slt>`; hidden `dm iam --json` = `dm accounts --json`;
  with `expires_at` forced to 0, `login status --json` rotated the refresh token (sha256 prefix 2e79fcf65e4b →
  561de7bd0f25); forced again, four concurrent `whoami` all exit 0 after one rotation (→ a7c35483fd87) and the sign-in
  still verifies (a reused refresh token would have ended it); `sync --reset` and `sync --cursor … --wait 1`;
  groups: the custodian created `g:design-602878` with its Silicon (joined at once) and the outside Carbon (pending),
  the outsider saw and accepted the invitation, was made co-owner and back, left; the last owner's `groups leave` →
  409 `last_owner`.
- Earlier runs found two real problems, both fixed before the passing runs: the stack's `dm` sign-in setup required a
  phone number (approval refused with `requirements_missing`; C28 set `required_fields: []`), and the allow-list answer
  has `{account, added_by, added_at}` entries (client model and OpenAPI corrected in 9ba6733).
- Afterwards: `dm-api` stopped, relays stopped (`dm daemon stop` per home), temporary homes removed, `dm_e2e_cli`
  dropped. The only process on 19780 is the Carbon's own relay (started 30 Sep, untouched).

### Left for later stages

- **Ship / packaging**: `apps.yaml`, `scripts/package-apps.sh`, the Silicon Apps release workflow (one archive per
  target; the discovery commands already answer in a clean, empty home), `honeycomb.yaml` and
  `scripts/build-honeycomb-release.py` removal, `scripts/install.sh` and the docs site's revision/preview banner and
  install script, deploy templates (`DM_IAM_*` → the Accounts variables). Cross builds were not tried here (no MSVC
  SDK or zig on this Mac); `reqwest`'s rustls backend pulls `aws-lc-sys`, as before.
- **Web**: the Next.js BFF; the CLI's `GET /api/v1/accounts` check and the `/me`, sync and allow-list routes are the
  same ones it will use.
- **Contract**: paste `understanding-proposal.md` (now with the CLI, Docs and Updates sections) into UNDERSTANDING.md
  (Carbon only).

### Blocked on

- Ting accepting Silicon Accounts proofs (unchanged): until then receiving is `dm inbox --watch` / `dm sync`; the
  Silicon runtime must switch from `dm delivery login` + `dm webhook` to supervising `dm inbox --watch` at cutover.
- Production registration of the app `dm` (orchestrator), with `device_flow` and `public_client` on.

### Gotchas

- crates.io `silicon-accounts-client` 0.4.0 has no `exchange_slt_public_client` even though the repo's 0.4.0 source
  does; the CLI posts the SLT grant itself.
- The relay status marker changed to `sync_feed`; a CLI that only knew `delivery_moved_to_ting` would not recognize a
  0.15 relay (0.15 recognizes both, so upgrades work; downgrades start a second relay on the next free port).
- Tests that run `dm login` start a relay: give each test home its own `SILICON_DM_RELAY_HOME` and run
  `dm daemon stop` afterwards (the CLI test support does this on drop), or test relays outlive the run.
- The stub's token names must not contain the error codes the test looks for (`slt_expired` "echoed" itself).

## 2026-10-10 — Stage 3 (packaging, CI, deployment, documentation): done

Everything around the code now says and does Silicon Accounts and Silicon Apps. Nothing was pushed,
released or deployed: a release is one tag away (`release.yml`) and the deployment is a sequence of
reviewed commands in [cutover.md](cutover.md). Judgement calls are S1–S25 in [decisions.md](decisions.md).

What changed:

- **Packaging**: `packaging/apps.yaml.in` and `scripts/package-apps.sh` (`scripts/package_apps.py`)
  pack one archive per target, `dist/apps/dm-<version>-<target>.tar.gz` + `.sha256`, refusing a
  version that differs from the CLI or workspace version, a binary that is not native for the
  target, a Linux binary needing glibc > 2.39, and a binary that answers the three discovery
  commands wrongly or writes to an empty home. `scripts/build-release.py` replaces the Mac
  cross-build script; `scripts/install.sh` hands over to `silicon-apps install dm`.
  `honeycomb.yaml`, `scripts/package-release.py` and `scripts/build-honeycomb-release.py` are gone.
- **CI**: `release.yml` (was `release.yaml`): six native runners, CLI tests, the per-binary check
  with `--discovery require`, one Linux pack job with `silicon-apps-cli` 0.2.0, artifact
  `dm-silicon-apps-release` with SHA256SUMS; tags must equal the version. `publish-release.yml`
  removed. `ci.yml`: Postgres 16 service container for the database tests, the discovery check on
  the built `dm`, packager and deployment unit tests, `cfn-lint` 1.57.2, docs site build and check.
- **Deployment configuration** (`deploy/aws`): both templates take `AccountsUrl`, `AccountsApiUrl`,
  `AppId`, `ProofIssuers`, `TingUrl` (no `IamBaseUrl`) and read `DM_APP_SECRET`,
  `DM_ACCOUNTS_WEBHOOK_SECRET`, `DM_DATA_KEY` from the runtime secret; no IAM, test-environment or
  Honeycomb variables; the testing RDS instance is kept but unused. The ECS bootstrap validates the
  new fields, never rotates the database URL or data key, keeps the 0.14 runtime keys for a rollback,
  and takes commands (`status`, `migrate`, `draft-identity-mapping`, `link-identities`);
  `run-bootstrap-task.py` runs one as a Fargate task from the stack's outputs and prints its log.
  The deploy guides follow; `README.gateway.md` points at the coming website.
- **Documentation**: `docs/releasing.md` (also `dm docs releasing`); install copy names the website
  for Carbons and `silicon-apps install dm` for the command; the docs site links to `main` and its
  preview banner is generic; dated records moved to `docs/history/` (with a README). The cutover
  runbook was rewritten end to end; the UNDERSTANDING proposal gained three missed passages; the
  root `decisions.md` gained D-060.

### Commits

| commit | subject |
| --- | --- |
| 3a6fe30 | Package dm for Silicon Apps instead of Honeycomb |
| 24e8a1d | Build Silicon Apps release archives in CI |
| 74cd55b | Configure the deployment for Silicon Accounts |
| 43d770a | Move dated IAM-era and Honeycomb-era records to docs/history |
| 15b6ef1 | Document releasing through Silicon Apps and installing from the website |
| b5f732e | Run the cutover steps as one-off bootstrap task commands |
| 3d3f8e3 | Write the production cutover runbook and record the stage 3 decisions |
| 82e7ce4 | Let the one-off task runner name another task definition |
| (last) | Record stage 3 in the migration progress log |

### Tests (from the worktree with the brief's Rust env)

| command | result |
| --- | --- |
| `cargo fmt --all -- --check` | ok |
| `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` | ok |
| `cargo deny --all-features check` | advisories ok, bans ok, licenses ok, sources ok |
| `DM_TEST_POSTGRES_URL=postgres://postgres@127.0.0.1:5460/postgres cargo test --locked --workspace --all-features` | **164 passed, 0 failed** (29 binaries): service 44 + account_routes 3 + accounts_auth 6 + accounts_authorization 5 + accounts_schema_migration 2 + accounts_webhook 6 + link_identities 6 + messaging_flow 6 + ting_delivery 4 + ting_transport 7 + wire_contract 6; CLI 6 + accounts 8 + discovery 1 + em_dash 1 + home 2 + message_length 1 + receive 3 + state 4; client 36 + http_contract 4; protocol 3. Same count as the end of stage 2: nothing regressed |
| `python3 -m unittest discover -s scripts -p 'test_*.py'` | 17 passed (manifest rendering, native-format and glibc checks, version rules, discovery with stand-in binaries, packing with a stand-in packer) |
| `python3 -m unittest discover -s deploy/aws -p 'test_*.py'` | 27 passed (grants for both bootstraps; runtime secret: keys, rollback keys, no rotation, refusals that name fields and never echo values; bootstrap commands against fake Secrets Manager and processes; the task runner against a fake `aws`; both templates and `.env.example` carry every variable `src/config.rs` requires and none it retired). The template tests fail on the stage-2 templates (checked) |
| `python3 scripts/sync-cli-docs.py --check` | `crates/cli/docs matches 19 canonical files` |
| `cd docs-site && npm ci && npm run build && npm run check` | 18 pages built and verified (the new releasing page included) |
| `npx --yes @redocly/cli@2.49.0 lint openapi.yaml` | valid (4 problems explicitly ignored, as before) |
| `actionlint` 1.7.12 on `.github/workflows/*.yml` | clean (it found one unused loop variable in the old `ci.yml`, fixed) |
| `cfn-lint` 1.57.2 on `fargate.yaml`, `production.yaml` | clean (both were clean at baseline too) |
| `shellcheck -S warning` 0.11.0 on `bootstrap.sh`, `install.sh`, `package-apps.sh` | clean (two warnings fixed) |
| `python3 deploy/aws/render.py` twice | `production.yaml` unchanged by a re-render (11,710 user-data bytes, limit 14,500) |

### Packaging, proven locally

- `cargo build --release --locked -p silicon-dm-cli` (Mach-O arm64, `dm 0.15.0`), then
  `scripts/package-apps.sh 0.15.0 macos-aarch64 target/mig/release/dm --discovery require` with
  `~/.apps/bin/silicon-apps` 0.2.0 run in an empty home against an unreachable server (so the
  Carbon's production sign-in is never used): discovery passed, archive
  `dist/apps/dm-0.15.0-macos-aarch64.tar.gz` (5,041,407 bytes, sha256 `9764d4aa…91d4`) holding
  exactly `apps.yaml` and `bin/dm` (mode 0755). Packing the same binary twice gives the same
  sha256.
- `silicon-apps validate` on the extracted (staged) directory and on the archive:
  `"valid": true, "errors": []`, manifest `{app_id: dm, command: dm, version: 0.15.0, targets:
  {macos-aarch64: {binary: bin/dm}}}`.
- From the extracted archive under `env -i HOME=SILICON_HOME=<empty dir> PATH=/usr/bin:/bin`:
  `dm --help` exit 0 (58 lines); `dm accounts --json` exit 0 with `"app_id":"dm"`,
  `"client_id":"dm"`, `"version":"0.15.0"`, `device_flow` and `public_client` true;
  `dm login status --json` exit 0, exactly `{"authenticated":false}`; `dm --version` → `dm 0.15.0`;
  nothing written to the home.
- `python3 scripts/build-release.py --targets macos-aarch64` built, checked and packed the target
  with SHA256SUMS; `--stage-only` repacked it byte for byte; unknown targets and missing builds are
  refused with the reason.

### Cutover tooling, proven locally

With the real `dm-migrate` against a scratch database (`dm_ship_ops`, dropped afterwards) and the
shared local Silicon Accounts stack (dm's dev credentials, never printed):
`psql -X -A -t -F '|'` with the bootstrap's status query lists migrations up to `40|accounts
partition behaviour|t`; `dm-migrate link-identities --file /dev/stdin --dry-run` reads a mapping
from a pipe (`mapping_rows: 1`); `dm-migrate draft-identity-mapping --out /dev/stdout` with two
seeded IAM-era ids printed `c:shubham,b97` and listed `si:cos` as unresolved, and the bootstrap's
parser split that real output into the mapping and the report; feeding the drafted mapping back
through stdin to a dry run linked `c:shubham -> b97` (verified online) and reported
`dm.bug_reports.actor_id: 1` rewritten, `si:cos` unmatched. The jq pipelines in the runbook (one-off
task definition, change-set parameters for both stack updates, secret merge from files) were run on
realistic documents: the parameter files name exactly the template's parameters.

### Sweep

`git grep -n -i -E 'iam|honeycomb|org_id|organi[sz]ation|\borg\b|tenant'` outside `docs/history/`
(122 files). Every remaining hit is intentional:

| where | why it stays |
| --- | --- |
| `migrations/` (32 files) | Applied migrations are immutable (sqlx checksums them); they describe the IAM-era schema. |
| `src/` (`organization_id`, `OrganizationId`, `ACCOUNTS_PARTITION`, `infrastructure/postgres/*`) | Stage 1's storage partition: the column keeps its name, every new row is in `@accounts` (decisions, stage 1). |
| `src/infrastructure/postgres/link_identities.rs`, `src/operator.rs`, `src/bin/dm_migrate.rs`, `deploy/aws/bootstrap-task.py` (help text) | The cutover tools re-key IAM-era identities and say so; operator-only copy. |
| `iam_principal_id,accounts_uuid` (operator, bootstrap, runner, their tests) | The mapping file format the brief defines (D6). |
| `src/config.rs` `OBSOLETE_VARIABLES`; `deploy/aws/bootstrap-task.py` `RETIRED`; cutover section 5 | Names of retired settings: logged when set, carried over for a rollback, then removed. |
| `src/api/contracts.rs` `retired_routes`, `src/api/{access,render,sync}.rs`, `src/domain/*`, `src/reporting.rs`, `src/infrastructure/postgres/{accounts,group_ids,ting,directory}.rs` | Tell old clients why `/api/v1/iam` is gone, render unlinked IAM-era ids, refuse cursors signed with the old key, read the IAM-era directory tables (stage 1). |
| `tests/` (fixture `legacy-0038.sql`, `link_identities`, `accounts_*`, `account_routes`, `ting_delivery`) | Upgrade and re-key tests load IAM-era data or assert the retired routes are gone. |
| `crates/cli` (`Command::Iam`, `command_tests.rs`, `docs.rs`, `tests/{accounts,state}.rs`) | The hidden `dm iam --json` alias (one release) and the tests that keep retired words out of help and guides. |
| `crates/client/src/ting.rs`, `runtime_tests.rs`, `tests/http_contract.rs`; `crates/protocol/src/lib.rs` | Reject schema-1 Ting references that carry `org_id`; load 0.14 profiles; accept older `g:<org>:<slug>` group ids. |
| `crates/client/src/runtime/daemon.rs` (`Tenant`) | The shared relay's internal name for an attached home; not an account grouping, never shown (S24). |
| `deploy/aws/*.yaml`, `README.md` (`AWS::IAM::Role`, `CAPABILITY_NAMED_IAM`, `IamInstanceProfile`, `arn:aws:iam::`) | AWS's own Identity and Access Management, unrelated to the retired service. |
| `deploy/aws/test_bootstrap_*.py`, `test_run_bootstrap_task.py` | Fixtures of a 0.14 runtime secret and assertions that the templates carry no retired variable. |
| `deploy/aws/gateway.yaml`, `gateway-bootstrap.sh` (`IAM_LOGIN_ORIGIN`), `README.gateway.md` | The SolidJS website's gateway, still what production runs; the web stage replaces it (S19). |
| `web/` (29 files) | The SolidJS website, replaced wholesale by the web stage. |
| `understanding/` | Carbon-only contract files; proposals are in `understanding-proposal.md`. |
| `decisions.md`, `docs/migration/*` | The historical decision log and the migration record. |

### Left for later stages

- **E2E stage**: run `scripts/package-apps.sh` on the e2e build and the discovery commands from the
  archive (scenario 7); the archive from this stage is in `dist/apps/` (ignored by git).
- **Web stage**: the Next.js website must replace `web/` and its hosting files: `deploy/aws/gateway.yaml`,
  `gateway-bootstrap.sh`, `render-gateway.py`, `README.gateway.md` and the `gateway` job of
  `deployment-images.yml`, for a standalone Next.js server behind the DM ALB at
  `dm.teamofsilicons.com` (settings `PUBLIC_URL`, `ACCOUNTS_URL`, `APP_API_URL`, and `APP_SECRET`
  and `SESSION_SECRET` from Secrets Manager; streaming to the backend; DNS moves from Vercel). Add
  the web job to `ci.yml`. The cutover's "The website" section is the plan to make concrete.
- **Contract**: paste `understanding-proposal.md` into UNDERSTANDING.md (Carbon only).

### Blocked on

- Unchanged from stage 2: production registration of `dm` in Silicon Accounts and Silicon Apps;
  Silicon Interface's switch to proofs; the Silicon runtime's switch (Silicon Apps installs, the new
  sign-in, `dm inbox --watch`); Ting accepting proofs (only to turn Ting delivery back on).
- Not verifiable here: the Windows, macOS x86_64 and Linux release builds (no runners for them on
  this Mac; the workflow runs them natively, as before), and anything in AWS (templates are
  cfn-lint clean and the operator tooling is tested against fakes).

### Gotchas

- zsh does not split an unquoted `$args`: `dm $args` with `args="accounts --json"` passes one
  argument and clap answers exit 2. Use arrays or separate words in test loops.
- `dm-migrate` logs to stdout; run operator commands with `DM_LOG_FILTER=error` when their stdout
  is the product (the bootstrap does).
- `cfn-lint` exits non-zero on warnings; `gateway.yaml` has an older W3005, so CI lints only the
  two backend templates.
- Python tests that import the hyphenated scripts leave `__pycache__/` beside them; it is ignored
  now.
- `psql` is not on PATH on this Mac: `/opt/homebrew/Cellar/postgresql@16/16.11_1/bin/psql`.

## Recovery completion — Next.js website (2026-10-10)

The web stage is now implemented and locally verified; see [web-verification.md](web-verification.md) for exact evidence, product coverage and release limits. Real backend/CLI checks151/151, frontend units44, TypeScript/lint/standalone build passed. Generic browser checks24 plus the corrected full product journey2/2 passed. Added a separate AWS website stack with the old gateway/state retained for rollback. No production actions performed.

## UUID128 compatibility and checked backfill — 10 October 2026

Added canonical UUID acceptance, an explicit identity-column mapping consumer with transactional dry-run/apply/reapply and a retired-subject fence. Populated clone verification preserved resource IDs, provider paths, native credentials and signed bodies; 18 accounts and 4 encrypted proof grants migrated. See [uuid128.md](uuid128.md) for the cutover sequence and evidence. Production and original checkouts remain untouched.

## UUID cutover replay and coverage verification — 10 October 2026

The checked CSV consumer now refuses incomplete legacy-account coverage before changing data and parks unaccepted prepared notifications without rewriting historical body/event identities. Dry-run, apply, idempotent reapply, conflicting-map refusal and missing-map refusal passed populated PostgreSQL clones. Explicit pending notification fixtures verified immutable-body preservation and replay exclusion. See [uuid128.md](uuid128.md) for schema-specific preservation rules and local evidence. No production data or original checkout changed.

Verified webhook handling now ignores retired top-level subjects and embedded custodian identities before writing account state, and records an acknowledged delivery without reintroducing the retired account reference. Repeated delayed deliveries remain harmless. Regression covers signed-out subjects, custodian changes and profile updates; the Accounts webhook suite passed 7/7 (`.mig/uuid-event-suite.log`).

## Coherent local cutover fixture — 10 October 2026

A read-only audit against current Accounts UUIDs and current/historical public handles found four DM identities from real E2E run616156 whose Accounts rows were lost during an earlier local stack reset. Their mapping cannot be inferred safely. The whole `dm_e2e` store and original `.mig/dev` secrets were preserved offline and backed up to `.mig/cutover/dm_e2e-pre-uuid-quarantine.dump`; no rows were deleted or reassigned. A fresh isolated `dm_uuid128_cutover` store, state `.mig/uuid-dev`, now uses current Accounts. Its real Accounts/API/CLI suite passed 151/151 (`.mig/e2e/run-622890/result.json`). A valid pre-cutover Carbon token and persistent message artifact were captured for subsequent UUID migration retention checks. This is local fixture hygiene, not a production data migration.

The final local macOS ARM64 CLI was rebuilt after UUID/auth fixes, then repackaged with required native signed-out discovery and extracted-archive validation. Evidence: `.mig/uuid-final-package.log`; artifacts remain local under `dist/apps/`. This is local release preparation, not publication.

## Coordinated local UUID backfill — 10 October 2026

With API/worker writers stopped and full database/private-state backups captured, applied the shared Accounts export (211 rows, SHA256 `750423f3117e11f5eb42025b457ff0f1bf42bd463c31b7e106d9cd10978ca4d9`) to `dm_uuid128_cutover`: 5 cached account rows moved and 2 held proof grants were authenticated/resealed. Dry-run, apply and unchanged replay passed. Exact resource IDs, archive/prepared payloads, Hook secret ciphertext and decrypted DM/Extend proof hashes matched their pre-apply snapshots. The live store had no unaccepted prepared deliveries left to park; separate populated queue regressions cover that case. Evidence: Commit `.mig/cutover/applied.json` and `retention-verified.json`. Production remains untouched.
