# Configure DM

DM ships defaults for ordinary use. Change the relevant setting explicitly when
your host, callback, or backend needs different behavior.

## Local state and profiles

| Setting | Default | Purpose |
| --- | --- | --- |
| `SILICON_HOME` | OS home | Parent for `.silicon-dm` private state |
| `dm config home LOCATION` | Unset | Persist a different existing home directory |
| `--profile NAME` | Selected default | Independent account session |
| `--test UUID` / `SILICON_DM_TEST` | Production | Previously saved sandbox |
| `--app-secret-file FILE` / `DM_TEST_APP_SECRET` | Unset | Automatic IAM test environment selection |
| `ISI` | Unset | Optional Silicon routing identity |
| `DM_API_URL` | Production DM backend | Backend for login and automatic sandbox selection |
| `--wait-seconds` | 30 | Foreground wait; timeout keeps durable work queued |

Keep state on a durable local disk. It includes credentials and SQLite queues;
use one daemon per state directory. Normal installation uses one shared directory
and one backend connection for all profiles. Independent directories intentionally
have independent runtimes.

## Callback delivery

`dm webhook URL` attaches the selected profile. `--secret-file FILE` adds an
optional bearer secret stored privately. Callback responses must acknowledge the
matching delivery ID. `dm unhook` detaches delivery without deleting queued events.
Use test callback URLs for sandbox integrations. HTTPS is required except for
local loopback callback addresses.

## Updates

Honeycomb owns CLI installation and update policy. Use `honeycomb install 'dm'`.
DM's daemon handles relay delivery and never replaces the CLI. Legacy `dm updates`
commands report this guidance. Manage Rust dependencies through your project.
Restart a running daemon after upgrading to load the new code; durable queues persist.

## Backend configuration

See `.env.example` and [deployment](deployment.md) for database, testing database,
IAM, Giphy, body-size, connection pool, timeout, and worker settings. The testing
database must differ from production, and the encryption key must stay stable
across replicas. Attachments are external links; no upload credentials are required.

No Space Station SDK, telemetry exporter, or analytics integration is installed
by this change.

## Bug report notifications

Set `DM_POSTMARK_SERVER_TOKEN` on the API service to enable report submissions.
The default `DM_POSTMARK_EMAIL_URL` is `https://api.postmarkapp.com/email`.
Verify `dm@teamofsilicons.com` as a Postmark sender. Reports notify
`saketdev12@gmail.com`, `shubhastro2@gmails.com`, and `bugs@teamofsilicons.com`,
matching the product specification. Each authenticated actor may submit ten new
reports per hour. Retries with the same idempotency key do not create another report.

The API returns `202` after the report is durable. Its worker retries Postmark
failures with backoff, up to an hour between attempts. A process crash after
Postmark accepts but before the transaction commits can deliver a duplicate
notification; the report ID identifies it. Sandbox reports are immediately marked
`simulated` and never enter the production notification transport. No mail token
is distributed in the SDK or CLI. See the [Postmark email API](https://postmarkapp.com/developer/api/email-api).
