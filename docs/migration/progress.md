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
