# DM end to end against Silicon Accounts

`scripts/e2e-accounts.sh` runs DM's eight end-to-end scenarios against a Silicon Accounts stack on
this machine, with real Carbons and Silicons, real tokens and the real `dm-api`, `dm-migrate` and
`dm` binaries. It never talks to a deployed Silicon Accounts: every URL it is given must be
`localhost` or `127.0.0.1`.

## What you need

- A local Silicon Accounts stack: the `silicon-accounts` checkout's testkit (the API, the account
  site's hosted pages, the mock email sender) and its stack file, the JSON file with the stack's
  URLs and each app's development secrets (`accounts_public_url`, `accounts_api_url`,
  `mock_messaging_url`, `apps.dm.app_secret`, `apps.dm.webhook_secret_seeded`,
  `apps.interface.app_secret`). DM's sign-in setup there must list
  `http://localhost:4120/auth/callback`, with the device flow and public clients on and no
  required fields.
- The testkit's dependencies installed (`testkit/node_modules/.bin/tsx`) and the stack's
  `silicon-accounts` CLI built in the checkout (`target/debug/silicon-accounts`, or set
  `SILICON_ACCOUNTS_CLI`).
- PostgreSQL 16 with a superuser that needs no password (default
  `postgres://postgres@127.0.0.1:5460`; set `DM_DEV_POSTGRES_URL` for another).
- The debug binaries: `scripts/dev-accounts.sh --build` (or `cargo build -p silicon-dm --bins -p
  silicon-dm-cli`). Scenario 7 also builds the release CLI and, when `silicon-apps` is installed,
  validates the archive with it (run with an empty home and no server, so it changes nothing).

## Run

```sh
export DM_TEST_STACK=/path/to/test-stack.json SILICON_ACCOUNTS_DIR=/path/to/silicon-accounts
scripts/dev-accounts.sh --build          # optional: start DM by hand (idempotent)
scripts/e2e-accounts.sh                  # every scenario
scripts/e2e-accounts.sh --only 5,8       # some (their prerequisites run too)
scripts/dev-accounts-stop.sh [--drop]    # stop it, put DM's webhook back, optionally drop the database
```

`scripts/dev-accounts.sh` migrates the database (`dm_e2e`), starts `dm-api` on `127.0.0.1:4121` as a
role that holds only `deploy/runtime-grants.sql`, points DM's webhook at the stack to
`http://127.0.0.1:4121/webhook/` (remembering the setup it replaced) and proves a test delivery
arrives and verifies. `python3 scripts/dev_accounts.py --help` lists its settings; `status` and
`env` print what runs and the exports that point the `dm` CLI at it. The data key and any webhook
secret the stack generates live in `.mig/dev/` (mode 0600, ignored by git) and reach `dm-api` only
through its environment.

When `dm-api` was not running, the runner starts the stack and stops it afterwards (keep it with
`--keep-running`). Each run uses new identities (`dm-e2e-<who>-<run>@example.test`,
`si:dm-e2e-<who>-<run>`) and signs each Carbon in once, because the stack limits email codes per
address and per network. Results: `.mig/e2e/run-<run>/result.json` (every check with its detail)
and `transcript.txt`, with every token, short-lived token, STK, proof and secret redacted. The
runner exits 1 when a check failed.

## The scenarios

| # | What it proves |
| --- | --- |
| 1 | **A Carbon on the API.** Signed in on the hosted pages (code exchange with DM's secret), the Carbon is admitted as itself; tokens of another audience, tampered tokens and contract 3 are refused. Messages are created (idempotently), listed, read, edited (history kept) and deleted (tombstone); drafts follow their versions; a group is created, renamed at its version, and the invitee sees it only after accepting. |
| 2 | **A Silicon on the CLI.** `dm login --slt-stdin` in an empty home (the token appears in no file or output, the sign-in file is 0600), `dm login status --json`, a journey of sends, edits, drafts and a group, `dm logout` (revoked at Silicon Accounts), then exactly `{"authenticated":false}`, and the positional `dm login <slt>` form. |
| 3 | **The device flow.** `dm login` prints the code and page, the Carbon approves it, the CLI is signed in as that Carbon and reads its Silicon's messages; `login status` lists the Silicon it looks after. |
| 4 | **Custodians and sharing.** The custodian lists, reads and manages (as an owner) its Silicon's group but cannot write as it; an unrelated Carbon gets 404/403 until the Silicon invites it by id, and loses access when removed. Nobody outside the Silicon's custodian, the custodian's other Silicons and its allow-list can start a conversation with it or invite it; `dm allow add/remove` and the custodian's `?for=` change that, and existing conversations keep working. |
| 5 | **Webhooks.** An id change made by the custodian reaches DM once and re-addresses the conversation; a replayed delivery (same `event_id`) is acknowledged and ignored; deliveries signed with another secret, unsigned, ten minutes old or not an event are refused; a name change shows; a Silicon that removes DM (`silicon-accounts apps remove dm`) has its earlier token, its CLI sign-in and its other homes refused; a website sign-out ends only that sign-in, and sensitive routes see it at once. |
| 6 | **Proofs.** DM accepts User verification proofs from `interface` on the scopes `DM_PROOF_ISSUERS` grants (and records `via_app` on what it sends), refuses missing scopes, routes that need the account's own sign-in, proofs for another receiving app, App verification proofs, made-up and revoked proofs, and accepts a refreshed one. With a local Ting stand-in (`scripts/dev_ting_stub.py`), DM enrols recipients with User verification proofs, sends with its App verification proof, re-enrols a recipient Ting does not know and replaces an expired proof; the stand-in checks every proof with Silicon Accounts. |
| 7 | **Silicon Apps discovery.** The release CLI is packed with `scripts/package-apps.sh` (validate, pack, discovery required); the archive holds exactly `apps.yaml` and `bin/dm`; from the extracted archive in an empty home `dm --help`, `dm accounts --json` and `dm login status --json` answer as Silicon Apps requires and nothing is written. |
| 8 | **Restart safety.** After `dm-api` restarts, earlier access tokens, the CLI's saved sign-in and sync cursors still work, the Silicon that removed DM stays refused, a replayed event is still ignored and forged deliveries are still refused. |

## Ting stand-in

The stack has no `ting` app, so scenario 6 runs `dm-api` with `DM_TING_APP_ID=interface` and the
stand-in verifies DM's proofs with `interface`'s credentials (`DM_DEV_TING_APP` picks another app).
In production `DM_TING_APP_ID` stays at its default, `ting`, and Ting is off unless `DM_TING_URL`
is set.

## What the runner changes at the stack

It creates the run's Carbons and Silicons, changes one Silicon's id and one Carbon's name, removes
DM from one Silicon's apps, issues and revokes proofs, and points DM's webhook at the local
`dm-api` while it runs (`scripts/dev-accounts-stop.sh` puts the previous setup back). It changes no
other app's sign-in setup or webhook.
