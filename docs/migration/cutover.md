# DM cutover to Silicon Accounts (runbook)

DM 0.15.0 (contract 4) signs everyone in with Silicon Accounts and has no organizations. The switch is one
coordinated change: the database migrations, the re-key of existing identities, the new service, and Silicon
Interface's move to proofs happen together. Nothing here has been run against production; every step needs a
Carbon's go-ahead.

## 1. Before the window

1. **Silicon Accounts, app `dm`** (owner console or `silicon-apps`/`silicon-accounts` CLIs, as the app owner):
   - sign-in: redirect URIs for the DM website (`https://dm.teamofsilicons.com/auth/callback`) and the CLI device
     flow; public client on (Silicons sign in with an SLT);
   - the local test stack's `dm` sign-in setup requires a phone number; decide whether production should (DM does not
     need one: drop `phone` from `required_fields` if not);
   - webhook URL `https://backend.dm.teamofsilicons.com/webhook/` (every update; DM handles `account.id_changed`,
     `account.updated`, `account.deleted`, `membership.signed_out`, `membership.access_removed`,
     `silicon.custodian_changed`). Keep the `whsec_…` secret for `DM_ACCOUNTS_WEBHOOK_SECRET`.
2. **Silicon Interface must switch at the same time.** It calls DM with User verification proofs
   (`Authorization: Proof sap_…`, receiving app `dm`) carrying the scopes it needs: `dm.conversations.read`,
   `dm.messages.read`, `dm.messages.write`, `dm.receipts.write`, `dm.drafts.write`. DM accepts them only from the
   apps listed in `DM_PROOF_ISSUERS`. Old Interface builds that send IAM tokens and `X-Org-ID` stop working at the
   switch.
3. **Secrets** (new runtime secret keys):

   | Variable | Value |
   | --- | --- |
   | `ACCOUNTS_URL` | `https://accounts.teamofsilicons.com` |
   | `ACCOUNTS_API_URL` | optional server-to-server origin |
   | `DM_APP_ID` | `dm` |
   | `DM_APP_SECRET` | the `dm` app secret |
   | `DM_ACCOUNTS_WEBHOOK_SECRET` | the app webhook secret (`whsec_…`) |
   | `DM_DATA_KEY` | `openssl rand -base64 32`, stored once and backed up (sync cursors and stored proofs depend on it) |
   | `DM_PROOF_ISSUERS` | `dm.conversations.read=interface,dm.messages.read=interface,dm.messages.write=interface,dm.receipts.write=interface,dm.drafts.write=interface` |
   | `DM_TING_URL` | leave unset until Ting accepts Silicon Accounts proofs |

   Remove `DM_IAM_BASE_URL`, `DM_IAM_APP_ID`, `DM_IAM_APP_SECRET`, `DM_IAM_WEBHOOK_SECRET`,
   `DM_IAM_WEBHOOK_KEY_VERSION`, `DM_IAM_REQUEST_TIMEOUT_SECONDS` and the Ting test variables (the service logs a
   warning while any obsolete variable is set). `deploy/aws/fargate.yaml` and `production.yaml` still pass the old
   names: switch them in the ship stage.
4. **Mapping file.** With the migrator credentials and the Accounts settings above:

   ```sh
   dm-migrate draft-identity-mapping --out mapping.csv
   ```

   It looks every IAM-era id in the data up by its current id and writes the rows it could resolve. Review it; add a
   row for every account whose id changed or that was never resolved (`iam_principal_id` may be a principal UUID or the
   public id DM stored, e.g. `c:saket,zQo`). Ids nobody maps stay as they are and show as legacy identities.

## 2. The window

1. Snapshot the database.
2. Stop `dm-api` and `dm-worker` 0.14.
3. `dm-migrate` (applies 0039 and 0040; additive: nothing is dropped or rewritten).
4. `dm-migrate link-identities --file mapping.csv --dry-run`: check `linked`, `unmatched`, `unused_mapping_rows`,
   `superseded` and `warnings` in the JSON report. Then run it without `--dry-run`. It runs in one transaction,
   suspends and restores every trigger, constraint and index, records each link in `dm.identity_links` and the run in
   `dm.identity_link_runs`. Running it again is a no-op; a corrected mapping re-points links (refused for a uuid that
   already signed in unless `--force-relink`).
5. Re-apply `deploy/runtime-grants.sql` (grants `dm.sync_group_conversation(uuid)` to the runtime role).
6. Deploy `dm-api` and `dm-worker` 0.15.0 with the new secrets.
7. `POST /v1/apps/dm/webhook/test` and check `select * from dm.accounts_webhook_receipts` shows the `ping`.
8. Switch Silicon Interface to proofs.
9. Smoke: `GET /api/v1/accounts` (no sign-in), a Carbon signs in on the website, sends to a Silicon in its circle,
   the Silicon reads it with `GET /api/v1/sync?wait=30`.

## 3. After the window

- Silicons and CLIs sign in again with Silicon Accounts (`dm login`, see "CLI and Silicon runtimes" below). Old CLIs
  get 406 `unsupported_contract`.
- Sync cursors from 0.14 fail once with `sync_reset_required` (the cursor key changed); clients reset and continue.
- Ting stays off: handoffs are kept and marked discarded (`ting_not_configured`). When Ting accepts Silicon Accounts
  proofs, set `DM_TING_URL`; recipients enrol on their next request.

## CLI and Silicon runtimes (dm 0.15)

The CLI moves in the same window; a 0.14 CLI gets 406 `unsupported_contract` from the 0.15 service.

1. **Install** `dm` 0.15 through Silicon Apps on every machine that uses DM (`silicon-apps install dm`). A running
   0.14 relay is replaced automatically by the first 0.15 command; queued requests survive.
2. **DM's sign-in setup** at Silicon Accounts: `device_flow` and `public_client` on (the CLI signs in without a secret),
   no required fields. (The local test stack had `required_fields: ["phone"]`; this stage set it to `[]` there.)
3. **Silicons sign in again**: the runtime runs `silicon-accounts login --app dm -q | dm login --slt-stdin` (the
   positional `dm login <slt>` it uses today still works). Carbons run `dm login` and approve the code. Each Silicon
   needs its Silicon Accounts identity first.
4. **Discovery**: `dm accounts --json` replaces `dm iam --json`; the old command stays as a hidden alias printing the
   same object for one release, so a runtime that still calls it keeps working. `dm login status --json` keeps its
   meaning (`{"authenticated":false}` when signed out).
5. **Receiving**: `dm delivery login`, `dm webhook … --all-apps` and `dm unhook` now answer `command_retired`. While
   `DM_TING_URL` is unset, a Silicon runtime receives by supervising `dm inbox --watch` (JSON lines; resumes from its
   saved cursor) or polling `dm sync --wait 30`, and fetches messages as before (`dm messages show`). Restore Ting
   delivery only when Ting accepts Silicon Accounts proofs.
6. **Local state**: 0.14 profiles load as signed out (their tokens are dropped). Requests they had queued are sent once
   the same id signs in again, and fail with `account_changed` if another id signs in to that profile. Requests queued
   for test environments fail with `testing_environments_retired`. Settings of retired features (test keys, update
   policy) are dropped at the first write.
7. **Interface and other Rust consumers** move to `silicon-dm-client` 0.15 (`with_token` / `with_proof`; no organization
   header).

## Rollback

Before `link-identities` runs: redeploy 0.14 (0039/0040 only add). After it: restore the snapshot; the re-key rewrites
identity columns in place, so the 0.14 binary cannot read the data afterwards.

## Blocked on

- Ting accepting Silicon Accounts User and App verification proofs (DM is ready; `DM_TING_URL` stays unset).
- Silicon Interface sending proofs instead of IAM tokens (must ship in the same window).
