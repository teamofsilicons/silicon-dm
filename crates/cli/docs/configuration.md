# Configure DM

DM ships defaults for ordinary use. Change a setting explicitly when your machine or deployment
needs something else.

## The CLI

| Setting | Default | Purpose |
| --- | --- | --- |
| `DM_API_URL` / `dm login --api-url` | `https://api.dm.teamofsilicons.com` | The DM service a profile signs in to (remembered by the profile) |
| `ACCOUNTS_URL` / `dm login --accounts-url` | `https://accounts.teamofsilicons.com` | The Silicon Accounts that signs DM in (remembered by the profile) |
| `DM_APP_ID` | `dm` | DM's app id at Silicon Accounts; only for a development app |
| `SILICON_DM_HOME` | unset | An explicit, absolute state directory (takes precedence) |
| `dm config home LOCATION` | unset | Keep state under `LOCATION/.silicon-dm` |
| `SILICON_HOME` | the OS home | Parent of `.silicon-dm` when nothing above is set |
| `SILICON_DM_RELAY_HOME` | `~/.silicon-dm/relay` of the OS user | Where the shared relay keeps its files |
| `--profile NAME` / `dm profiles use` | `default` | Which signed-in account a command uses |
| `--wait-seconds N` | 30 | How long a command waits for the relay; on timeout the request stays queued |
| `dm config telemetry false` / `DM_TELEMETRY_ENABLED=false` | on | Diagnostics ([what they contain](telemetry.md)) |

Plain `http` URLs are accepted only for this machine (`localhost`, `*.localhost`, loopback
addresses). Keep the state directory on a durable local disk: it holds the sign-ins and the
outgoing queue. One relay serves every state directory of the operating-system user.

`dm accounts --json` shows the values in effect without signing in. Setting `SILICON_DM_TEST` or
`DM_TEST_APP_SECRET` (test environments, retired) makes every networked command refuse with
`testing_environments_retired`, so a home that still selects a test environment never reaches the
live service.

## Installing and updating

Silicon Apps installs and updates the CLI: `silicon-apps install dm` (development releases:
`silicon-apps install 'dm>dev'`). DM never replaces its own executable. A running relay keeps
serving after an update; the next `dm` command from the new version replaces an older relay, and
queued requests survive. Rust programs manage `silicon-dm-client` as an ordinary dependency.

## The service

See `.env.example` for every setting. Sign-in is Silicon Accounts only:

| Variable | Required | Purpose |
| --- | --- | --- |
| `ACCOUNTS_URL` | yes | Silicon Accounts origin; exactly the `iss` of its tokens |
| `ACCOUNTS_API_URL` | no | Where DM calls Silicon Accounts server to server (default `ACCOUNTS_URL`) |
| `DM_APP_ID` | no | DM's app id in Silicon Accounts (default `dm`) |
| `DM_APP_SECRET` | yes | DM's app secret |
| `DM_ACCOUNTS_WEBHOOK_SECRET` | yes | The app webhook signing secret (`whsec_…`); webhooks arrive at `/webhook/` |
| `DM_DATA_KEY` | yes | 32 random bytes, base64: keys sync cursors and the encryption of stored proofs. Keep it stable and backed up |
| `DM_PROOF_ISSUERS` | no | Apps whose User verification proofs DM accepts, per scope (`dm.messages.write=interface,…`); empty accepts none |
| `DM_TING_URL` | no | Ting delivery; unset means clients sync with `GET /api/v1/sync` |
| `DM_TING_APP_ID` | no | Ting's app id in Silicon Accounts: the receiving app of the proofs DM sends Ting (default `ting`) |

Plain `http` is accepted only for this machine. Database, Giphy, body-size, connection pool,
timeout and worker settings are unchanged. Attachments are external links; no upload credentials
are required. DM's sign-in setup at Silicon Accounts needs `device_flow` and `public_client` turned
on for the CLI, and requires no account details (email and timezone are optional).

## Bug report notifications

Set `DM_POSTMARK_SERVER_TOKEN` on the API service to send report notifications. The default
`DM_POSTMARK_EMAIL_URL` is `https://api.postmarkapp.com/email`. Verify `dm@teamofsilicons.com`
as a Postmark sender. Reports notify `saketdev12@gmail.com`, `shubhastro2@gmails.com` and
`bugs@teamofsilicons.com`, matching the product specification. Each account may submit ten new
reports per hour; retries with the same idempotency key do not create another report.

The API returns `202` once the report is stored. Its worker retries Postmark failures with
backoff, up to an hour between attempts. A process crash after Postmark accepts but before the
transaction commits can deliver a duplicate notification; the report id identifies it. Outside
production, without a Postmark token, reports are stored and marked `simulated`. No mail token is
distributed in the client or CLI.
