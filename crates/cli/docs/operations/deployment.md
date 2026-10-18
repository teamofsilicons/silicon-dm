---
title: Deploy the DM service
description: The runbook for production as it runs (the DM service, its worker and the website as containers on one host behind the load balancer) and the scripts that upgrade it, with database migrations and runtime grants, speech, ingress, the website and its uploads into Silicon Briefcase, rolling back, the decisions only the Carbon makes, the checks after every deployment, and the Fargate templates as the alternative.
kind: instructive
order: 80
related:
  - operations/releasing.md
  - reference/configuration.md
  - reference/limits.md
---

# Deploy the DM service

Production runs on one ARM64 host: the DM service, its worker and the website are containers there, behind the load
balancer, and the scripts in [`deploy/parallel/`](../../deploy/parallel/) upgrade them. This runbook covers that path
first, then what every deployment needs whatever it runs on (configuration, ingress, the website's uploads), the
decisions the Carbon makes, and the checks after a deployment. The Fargate templates in `deploy/aws/` are the
alternative ([below](#the-alternative-fargate)).

A deployment changes what every Carbon and Silicon uses at once, so each step is a deliberate command run by the Carbon
responsible for production, or by someone they asked; nothing here runs from CI by itself.

## Production

| Part | Container (command) | Port on the host | Public name |
| --- | --- | --- | --- |
| The DM service | `dm-accounts-api` (`dm-api`) | 8081 | `https://api.dm.teamofsilicons.com` |
| Its worker | `dm-accounts-worker` (`dm-worker`) | none | |
| The website | `dm-accounts-web` (`node server.js`) | 4316 | `https://dm.teamofsilicons.com` |

- **The host's network.** The containers use it directly (`--network host`), read-only, without capabilities, with a
  64 MB tmpfs at `/tmp`. The website has another for Next's cache (`/app/.next/cache`); an upload touches neither.
- **Memory.** 768 MB each for the service and the worker (a spoken message's render holds its audio in memory), and
  512 MB for the website, which uploads of any size never fill (it holds none of a file's bytes).
- **The load balancer** (`silicon-dm-production`) ends TLS, sends each public name to its port, speaks HTTP/2 to
  browsers, sets no body limit, and closes a connection on which nothing moved for 180 seconds, either way.
- **The database** is RDS PostgreSQL `silicon_dm_accounts`. The migrator role `dm_accounts_migrator` owns its objects;
  the service and the worker run as `dm_accounts_runtime`.
- **Settings.** Each container's environment is a 0600 file in `/opt/silicon-accounts-apps/dm/config/` (`api.env`,
  `worker.env`, `migration.env`, `web.env`), which the scripts write from the Secrets Manager secret
  `silicon-dm/accounts-production/runtime`. Never source those files as shell code, print them, or copy a secret out of
  them.
- **The pre-Accounts deployment** keeps its own gateway on this host (port 4315) and its own service and database
  elsewhere, unchanged ([parallel production](../migration/parallel-production.md)).

## Upgrading production

Run these as root on the host (through SSM), in this order:

1. **Build the images.** Dispatch [`deployment-images.yml`](../../.github/workflows/deployment-images.yml) on the
   reviewed commit. It builds two linux/arm64 images labelled with the commit and the version: the service's (`dm-api`,
   `dm-worker` and `dm-migrate`) and the website's (`web/Dockerfile.gateway`), and exports each as an archive with its
   metadata and `SHA256SUMS`. Stage both on the host and check the sums.
2. **Back up the database** with an RDS snapshot.
3. **Upgrade the service and the worker.** `docker load` the service's archive, then:

   ```sh
   python3 deploy/parallel/upgrade_service.py --image <image id> --revision <full commit> \
     --grants deploy/runtime-grants.sql
   ```

   It checks the image's platform, revision and version, and saves the running containers' configuration and
   environment files in `/opt/silicon-accounts-apps/dm/best-chat-upgrade-<time>/`. Then it stops the service and the
   worker, so no older process writes beside a newer one, and runs `dm-migrate` with `migration.env`. It applies
   `deploy/runtime-grants.sql` as the migrator (a new table is unusable by the runtime role until it is granted). It
   writes `api.env` and `worker.env`: the running settings, plus `DM_DEEPGRAM_API_KEY` from the secret's
   `deepgram_api_key` field, written only into those two files and never printed. It starts both containers with 768 MB
   (`--memory`) and waits for `/ready` (204) on `127.0.0.1:8081` and for `GET /api/v1/voices` to say `enabled: true`.
   If any of that fails, it puts the old containers and their files back.
4. **Upgrade the website.**

   ```sh
   python3 deploy/parallel/upgrade_web.py --stage <folder with gateway.tar, gateway.metadata.json, SHA256SUMS> \
     --source <full commit>
   ```

   It checks the sums and the revision, loads the image, stops the running website and keeps it as
   `dm-accounts-web-before-<time>`. Then [`install_web.py`](../../deploy/parallel/install_web.py) writes `web.env`
   ([the website's settings](#the-websites-settings-in-production)), starts the new container and waits until `/`
   answers 200 with the name "Silicon DM". If it does not, the previous container comes back.
5. Run the [acceptance checks](#acceptance-after-a-deployment).

Upgrading stops each part for a moment. The CLI's relay retries what it sent meanwhile, and the browser's outbox does
a few times and then offers Retry, so nothing written is lost. The website stops the uploads under way when it is told
to stop and cancels their reservations at Briefcase, and the page sends each such file again from its first byte; see
[stopping the website](#stopping-the-website).

**Rolling back.** The previous containers keep their names with a suffix: `dm-accounts-api-before-0.17-<time>` and
`dm-accounts-worker-before-0.17-<time>`, with their environment files in the backup folder, and
`dm-accounts-web-before-<time>`. Stop and rename the new container, rename the old one back, and start it. 0.16 runs on
the migrated database ([0.17's migrations](#the-migrations-of-017) say why for each). Its sync feed then meets
positions it does not know, the ones 0.17's conversation events took, and answers 409 `sync_reset_required`: each
client resets its sync once and refreshes its lists, and nothing is lost (0.16 writes no conversation events, so it
happens once).

### The migrations of 0.17

`dm-migrate` applies them in order (the journal is `public._sqlx_migrations`), without the statement timeout the
service uses (`DM_DATABASE_STATEMENT_TIMEOUT_SECONDS` does not apply to it: a backfill on a large table must not be cut
short), and on failure prints the whole chain of causes. Then run `deploy/runtime-grants.sql` (`upgrade_service.py`
does both).

| Migration | What it does | DM 0.16 on the migrated database |
| --- | --- | --- |
| 0043 voices, speech and unread | Voice choices, the speech cache and renders, per-account speech usage, read positions (with a backfill from the receipts already recorded) | Never touches the new tables |
| 0044 conversations of several and large files | Conversations of three or more accounts, attachment sizes up to 5 TiB | Its rows keep fitting |
| 0045 conversation events | Per-account conversation events in the sync stream | Never reads or writes them (its feed resets once: above) |
| 0046 delivery anchors without waiting | Fan-out triggers that no longer wait on another writer's directory refresh | Its writes fire them, with the same results |
| 0047 outbox lookups and retention | Who received each message (`message_recipients`), keyed trigger lookups, 30 days of sync feed, `delivered_through_sequence` | Its writes fire the replaced trigger functions, with the same results through keyed lookups |
| 0048 speech cache scopes | Speech audio cached per conversation, and per account for `POST /speech` | Never touches the speech tables |
| 0049 message recipients recorded on send | The database records a message's recipients from its outbox rows, whichever version sends | Its sends record them too, so ticks keep moving |
| 0050 presence activity conversation | The conversation a device's activity is about (`client_presence_leases.activity_conversation_id`) | Writes leases without it; nothing breaks |
| 0051 read events | `read` conversation events with the reader's unread facts | Never reads or writes conversation events |

All of them are additive: nothing 0.16 reads changes meaning, and no data is removed except the sync feed's events
older than 30 days, which no client can still ask for.

### The website's settings in production

`install_web.py` writes these into `web.env`:

| Variable | Value |
| --- | --- |
| `APP_ID` | `dm` |
| `APP_SECRET`, `SESSION_SECRET` | From the secret (changing `SESSION_SECRET` signs every browser out) |
| `ACCOUNTS_URL` | `https://accounts.teamofsilicons.com` |
| `APP_API_URL` | `http://127.0.0.1:8081`, the DM service on the same host |
| `PUBLIC_URL` | `https://dm.teamofsilicons.com` |
| `HOSTNAME`, `PORT` | `0.0.0.0`, `4316` |
| `BRIEFCASE_API_URL` | `https://api.briefcase.teamofsilicons.com`: uploads are on |
| `NEXT_TELEMETRY_DISABLED` | `1` |

There is no `UPLOADS_MAX_BYTES`, so the website has no limit of its own and Briefcase's apply. There is no
`KEEP_ALIVE_TIMEOUT` either: the website keeps idle connections longer than the load balancer by itself
([The website](#the-website)).

### Stopping the website

The website stops the uploads under way when it is told to stop (`docker stop` sends SIGTERM). It takes no new one
(503 `website_restarting`), stops each transfer and each save that was still going after its last byte, and cancels
their reservations at Briefcase with fresh proof tokens before the process exits. Its log says so, with how many it
stopped (`dm: stopping; one upload was under way…`). Without that, each reservation would stay pending at Briefcase
for up to a day, counted against the Carbon's daily allowance and their 32 unfinished uploads. The page sends each
stopped file again from its first byte once the website is back.

`docker stop` waits only 10 seconds by default before it kills the process, so the scripts give every part 30:
`docker stop --time 30` for the website, the service and the worker, and `--stop-timeout 30` on the website's
`docker run`. The website's cancels then have room even when Briefcase answers slowly, and the service finishes its
requests (a sync long-poll holds up to 30 seconds; `DM_SHUTDOWN_TIMEOUT_SECONDS` is 30 by default). Stopping a part by
hand, do the same. A deployment during a large upload costs that upload its progress, so check the website's log, or
wait, when that matters.

## The DM service's settings

DM's app at Silicon Accounts (`dm`) needs its webhook at `https://api.dm.teamofsilicons.com/webhook/` (every update,
including custodian changes) and, for the command, `device_flow` and `public_client` turned on.

| Setting | Migration process | API and worker |
| --- | --- | --- |
| `DM_ENVIRONMENT` | `production` | `production` |
| `DM_DATABASE_URL` | The migrator role (`dm_accounts_migrator`) | Same database, the runtime role (`dm_accounts_runtime`) |
| `DM_BIND_ADDR` | not needed | `0.0.0.0:8081` in production (the load balancer reaches the host's port) |
| `DM_PUBLIC_BASE_URL` | not needed | `https://api.dm.teamofsilicons.com/api/v1/` |
| `ACCOUNTS_URL` | not needed (`dm-migrate link-identities` uses it) | `https://accounts.teamofsilicons.com` |
| `ACCOUNTS_API_URL` | optional | Optional server-to-server origin |
| `DM_APP_ID` | not needed | `dm` |
| `DM_APP_SECRET` | not needed | DM's app secret |
| `DM_ACCOUNTS_WEBHOOK_SECRET` | not needed | The app webhook secret (`whsec_…`) |
| `DM_DATA_KEY` | not needed | 32 random bytes, base64; stable and backed up |
| `DM_PROOF_ISSUERS` | not needed | Apps allowed per proof scope |
| `DM_TING_URL` | not needed | Ting's address when Ting delivers to Silicons ([`enable_ting.py`](../../deploy/parallel/enable_ting.py) set it in production); unset, clients sync with `GET /api/v1/sync` |
| `DM_GIPHY_API_KEY` | not needed | Giphy key |
| `DM_DEEPGRAM_API_KEY` | not needed | Deepgram key for voices, spoken messages, listening and transcription; `upgrade_service.py` writes it from the secret |
| `DM_REPORTS_PER_HOUR` | not needed | Bug reports one account may send an hour (default 10) |

Without `DM_DEEPGRAM_API_KEY` the service runs, says `speech.enabled: false` in `GET /api/v1/accounts`, and answers
the speech routes with 503 `speech_not_configured`; everything else works. Both the API and the worker render speech
(they share the work through the database), so both need the key. The speech limits (`DM_SPEECH_RENDERS_PER_MINUTE`,
`DM_SPEECH_DAILY_CHARACTERS`, the cache size) are in [configuration](../reference/configuration.md#speech).

**Database connections.** Each process keeps a pool of `DM_DATABASE_MAX_CONNECTIONS` for its queries, and up to three
more connections of its own, outside that pool, for `LISTEN` (the wake-ups that let a sync long-poll, the Ting worker
and the speech worker react at once): `dm-api` opens three, `dm-worker` two. Count them when sizing the database's
connection limit: with a pool of 4, the service and the worker hold up to 13 together (4 + 3 + 4 + 2). A deployment
with a pool of one connection opens no listener and relies on polling.

Production database URLs must include exactly one `sslmode=verify-full` and a host matching the certificate; never
disable verification. Other production upstream URLs use HTTPS. [`.env.example`](../../.env.example) lists pool,
timeout, delivery and provider settings.

Migrations: `dm-migrate` only applies them; `dm-migrate link-identities` and `draft-identity-mapping` re-key
identities from before Silicon Accounts at cutover. The journal is `public._sqlx_migrations`; a mismatch is an error to
investigate, never permission to rewrite history. Run `deploy/runtime-grants.sql` after every migration. Back up the
database before upgrading; replacing an image never deletes data.

## Ingress for the DM service

| Path | Required handling |
| --- | --- |
| `/api/v1/*` | HTTPS requests; preserve `Authorization`, `Idempotency-Key`, `If-Match`, `Range` and `x-dm-contract-version` |
| `/api/v1/sync` | Long-poll: allow responses up to 45 s without buffering |
| `/api/v1/conversations/*/speech`, `/api/v1/speech` | Rendering takes 2 to 6 seconds and at most about 3 minutes: allow responses up to 200 s |
| `/api/v1/speech/transcriptions` | Audio uploads up to `DM_TRANSCRIBE_MAX_BYTES`: allow the body and up to 10 minutes |
| `/webhook/` | POST with the exact original body bytes and the `X-Accounts-*` headers; keep the trailing slash |
| `/live`, `/ready` | Probes; success is 204 |

Do not parse and re-encode webhook JSON at the proxy, cache authenticated responses or strip
`Cache-Control: no-store`. Redact credentials from logs. Align body limits with `DM_MAX_HTTP_BODY_BYTES` (128 MiB by
default).

## The website

The website is a Next.js server ([`web/Dockerfile.gateway`](../../web/Dockerfile.gateway), `node server.js`).
Production runs it as `dm-accounts-web` on port 4316; the Fargate alternative is
[`deploy/aws/web.yaml`](../../deploy/aws/web.yaml) ([its notes](../../deploy/aws/README.web.md)). Browsers talk only to
it. It signs Carbons in, forwards their calls to the DM service through its same-origin proxy (`/api/…`), and saves
their files and voice notes in Silicon Briefcase. Its settings are in
[configuration](../reference/configuration.md#the-website).

| Path | Required handling |
| --- | --- |
| `POST /api/uploads` | The request body is the file itself, of any size up to Briefcase's 5 TiB: no body limit, no request buffering (the website pipes the bytes on to Briefcase as they arrive; a proxy that buffers first holds the whole file and delays every byte), and no deadline for the whole request. An ordinary idle timeout is enough (60 seconds or more): bytes flow the whole time, and the website answers within about 20 seconds of the last one (see below). With nginx: `client_max_body_size 0; proxy_request_buffering off; proxy_http_version 1.1;` on that location, with `proxy_read_timeout` and `proxy_send_timeout` of at least 60 s |
| `GET`, `DELETE /api/uploads/saving` | A file the website is still saving into Briefcase: each `GET` waits up to 25 seconds for the outcome (allow 60), `DELETE` cancels it |
| `GET`, `HEAD /api/uploads/config` | Small JSON; whether uploads are on, and the largest file |
| `GET`, `HEAD /api/uploads/file` | Streams the files of messages (photos, audio, video) from Briefcase: keep `Range`, `If-Range` and `If-None-Match` on the way in and `206`, `Content-Range`, `Accept-Ranges` and `ETag` on the way out, never buffer or cache them (they are `Cache-Control: private`), and keep the website's `Content-Security-Policy: sandbox` and `nosniff` |
| every page | Keep the website's own headers, in particular `Permissions-Policy: camera=(), microphone=(self), …`: voice notes and dictation need the microphone, and an edge that sends `microphone=()` instead turns them off |
| `/api/…` (the proxy) | As the service's routes above: the sync long-poll holds 45 s, speech answers within 200 s, transcription uploads up to `DM_TRANSCRIBE_MAX_BYTES` within 10 minutes |
| `/auth/callback` | Registered at Silicon Accounts as `PUBLIC_URL/auth/callback` before traffic moves |

The website runs as one process, in one replica. Its session refresh uses an in-process cache, and a file it is still
saving into Briefcase after its 202 lives in that process's memory (the page's `GET /api/uploads/saving` must reach
it), so do not run it in cluster mode or with several replicas unless a Carbon's requests always reach the same one.

**Its own server.** The website sets up its server when it starts, before it answers anything (`instrumentation.ts`,
`prepareServer` in `web/lib/server/briefcase.ts`), so these hold from the first request after a start, an upload
included:

- **The deadline for a request's body.** Node ends a request whose body is still arriving after five minutes. The
  website raises that to a day for its whole server, which no upload reaches (Briefcase's own deadline is far
  shorter). Headers must still arrive within 60 seconds.
- **Idle connections.** Node closes an idle connection after 5 seconds. A load balancer that keeps its own connections
  to the website longer then sends a request now and then on one the website is just closing, and answers it 502.
  AWS documents this cause, and it can hit an upload at its first byte. So the website keeps idle connections 185
  seconds, longer than production's 180. An operator's `KEEP_ALIVE_TIMEOUT` (milliseconds, Next's own setting) takes
  its place. Behind a balancer whose idle timeout is longer than 180 seconds, set it above that timeout.

**Uploads and the website's server.** An upload holds two connections for as long as its bytes take: the browser's, and
the website's own to Briefcase. Nothing is written to the website's disk and its memory stays bounded. Measured on a
production build: a 6 GiB upload at 2 GB/s took a fresh server from 107 to 291 MB, three more at once added nothing, and
at 100 MB/s it does not move. A request to a page can make it hold at most 1 MB. So the task's size does not limit a
file: 512 MB is plenty. The limits that remain are the ingress's and Briefcase's:

- **Idle timeouts.** Bytes flow the whole time a file is sent. After the last byte, Briefcase checks the file's digest
  and stores it before it answers, and for a large file that takes minutes with nothing moving. A load balancer would
  cut the browser's request after its idle timeout (production's: 180 seconds) and the file would be lost. So the
  website answers the browser within 20 seconds of the last byte instead (`202`, with an id) and keeps saving, and the
  page asks `GET /api/uploads/saving` until it has the link, each ask waiting up to 25 seconds. No timeout in front of
  the website needs raising. The website's own request to Briefcase does wait in silence, up to 45 minutes. What
  Briefcase's ingress allows after the last byte (its nginx: 900 seconds on the transfer route) bounds the largest
  file, with Briefcase's own limits below. The website stops a transfer where nothing moves for 5 minutes.
- **Briefcase's own limits** ([the Briefcase side](#uploads-the-briefcase-side)).

### Uploads: the Briefcase side

The DM service never handles files. The website's server saves a Carbon's file into the Carbon's own Silicon
Briefcase, under `apps/dm/`, as DM acting for the Carbon. Silicon Accounts issues it a User verification proof for the
receiving app `briefcase` with the scopes `briefcase.uploads.reserve`, `briefcase.uploads.commit`,
`briefcase.uploads.status`, `briefcase.uploads.cancel` and `briefcase.link_access.update`, and Briefcase checks it.
Production Briefcase accepts those proofs only from the apps its `BRIEFCASE_PROOF_ISSUERS` lists for each scope, so `dm`
must be listed for all five:

- **Without `briefcase.uploads.reserve` or `briefcase.uploads.commit`**, every upload answers 503
  `briefcase_not_allowed` and nothing is saved; the website offers links instead.
- **Without `briefcase.link_access.update`**, every file is saved but its link never opens. The Carbon reads "Silicon
  Briefcase saved the file but does not let DM open links to files yet", with where the copy is.
- **Without `briefcase.uploads.cancel` or `briefcase.uploads.status`**, a stopped upload is not let go and stays
  pending for a day, and one whose answer was lost on the way cannot be checked, so it fails.

The website's log names Briefcase's own code for each, never a file's name: `upload failed: briefcase_not_allowed
(proof_scope_missing)` for a refused upload, and `upload: Silicon Briefcase did not cancel an unfinished upload
(proof_scope_missing)…` for a refused cancel. Look for both after turning uploads on.

Briefcase's own limits bound what a Carbon can send. Its operators set them:

- **Its upload deadline** (`BRIEFCASE_UPLOAD_TIMEOUT_SECONDS`, 900 seconds by default) bounds one file from its first
  byte to its answer. At 100 Mbit/s that carries about 10 GB, so raise it for large files (21600, six hours, is a
  sensible value).
- **The daily allowance:** 100 GiB of uploads into one Briefcase a UTC day unless its operators gave that Briefcase
  more. Unfinished uploads count toward it, so a larger file is refused on any day (429 `briefcase_limit`). The
  composer warns when such a file is added.
- **Its temporary disk** holds each file while it is checked (production: about 90 GB free on the Briefcase host
  today). It bounds the largest single file until it grows.

Set `BRIEFCASE_API_URL` on the website only once Briefcase lists `dm` (`UPLOADS_MAX_BYTES` only if you want a limit
below Briefcase's 5 TiB). Then check an upload, a voice note and a photo in a message end to end, and one large file
(a few gigabytes) to see the timeouts above hold.

## Decisions for the Carbon

These change what DM may do for every Carbon and Silicon, and what it costs, so the Carbon responsible for production
makes them; nothing in this repository makes them by itself.

- **Speech in production.** A Deepgram key in DM's production secret (`deepgram_api_key`), which `upgrade_service.py`
  passes to both the API and the worker as `DM_DEEPGRAM_API_KEY`. Deepgram then renders and transcribes, billed to that
  key, within the per-account limits in [configuration](../reference/configuration.md#speech).
- **Uploads into Briefcase.** `dm` in production Briefcase's `BRIEFCASE_PROOF_ISSUERS` for the five upload scopes
  above, and `BRIEFCASE_API_URL` on the website (`install_web.py` sets it). DM can then write into every Carbon's
  Briefcase, only under `apps/dm/` and only with that Carbon's own sign-in, files of any size up to Briefcase's 5 TiB.
- **How large a file can be in practice.** Briefcase's daily allowance (100 GiB a Briefcase by default), its upload
  deadline and its temporary disk, above.

The Carbon turned speech and uploads on for DM 0.17 (18 October 2026): speech with a Deepgram key, and uploads with
`dm` on Briefcase's issuer list. Turning either off again is the Carbon's decision too.

## Acceptance after a deployment

1. `/live` and `/ready` through the public origin answer 204.
2. `GET /api/v1/accounts` names the expected Silicon Accounts and app id, and `speech.enabled` is what you configured.
3. A Carbon signs in on the website or with `dm login`; a Silicon with
   `silicon-accounts login --app dm -q | dm login --slt-stdin`; both pass `dm login status --json`.
4. The Carbon sends to a Silicon it looks after; the Silicon reads it with `dm inbox` and marks it read; the Carbon
   sees `read_at`.
5. With speech on: `dm voices list` lists the five voices, the Silicon sends `dm messages send c:… --speak 'Testing.'
   --wait`, its `speech.status` becomes `ready` within seconds, and `dm messages listen` saves the audio.
6. `POST /v1/apps/dm/webhook/test` at Silicon Accounts arrives (`dm.accounts_webhook_receipts`).
7. GIF trending and search work with the configured key.
8. With uploads on: on the website, attach a photo and record a voice note; both arrive, play and open from the
   other Carbon's browser (`GET /api/uploads/config` answers `enabled: true`, `capped: false` unless you set
   `UPLOADS_MAX_BYTES`). Then send a file of a few gigabytes: it prepares, shows its speed and time left, arrives,
   and opens through the website (its download is streamed with `Range`). A file large enough that Briefcase takes
   more than 20 seconds to store it (tens of gigabytes) says "Saving… this can take a few minutes" and then arrives.
   The website's log has no `upload failed: briefcase_not_allowed` line.
9. On the host, `curl -sI http://127.0.0.1:4316/robots.txt` says `Keep-Alive: timeout=185`. The first answer after a
   start still says 5, because Node writes it before the website changes the setting.
10. Record the image digests, the migration version and the observed outcomes (no secrets).

## The alternative: Fargate

[`deploy/aws/fargate.yaml`](../../deploy/aws/fargate.yaml) runs private ARM64 API and worker tasks behind a dedicated
Application Load Balancer, with an encrypted RDS PostgreSQL instance; the stack owns its security groups, task roles,
CloudWatch logs and rate-limiting WAF ([the Fargate guide](../../deploy/aws/README.fargate.md)). The
[EC2 template](../../deploy/aws/production.yaml) ([its guide](../../deploy/aws/README.md)) remains available too, and
[`deploy/aws/web.yaml`](../../deploy/aws/web.yaml) runs the website. Production does not use them today, and they lag
behind it:

- They pass neither `DM_DEEPGRAM_API_KEY` to the tasks nor `BRIEFCASE_API_URL` to the website. Add the secret and the
  variable to both service task definitions, and the variable to the website's, before relying on them.
- They default to the names of the pre-Accounts deployment: check every host name before applying one.

Build and push reviewed ARM64 runtime and bootstrap images to `silicon-dm-production` in ECR, pass their immutable
digests to a CloudFormation change set, and inspect it before execution. Start with `RuntimeDesiredCount=0`, run the
bootstrap (migration) task, check it exited 0, then raise the count. Credentials belong in Secrets Manager, never in
image layers, template parameters or command arguments. Fargate allows a 120-second stop timeout; DM's shutdown
deadline is 110 s.

A rolling deployment runs an older task beside a newer one for a while. A 0.16 task then meets sync positions it does
not know (0.17's conversation events take some) and answers 409 `sync_reset_required`, so each affected client resets
its sync once; nothing is lost. The upgrade on the host has no such overlap: it stops the old service first.

By hand, on a host of your own:

```sh
docker build --tag silicon-dm:0.17.1 .
docker run --rm --env-file /secure/silicon-dm/migration.env silicon-dm:0.17.1 dm-migrate
psql 'service=dm_migrator' --set ON_ERROR_STOP=1 \
  --set runtime_role=dm_runtime --file deploy/runtime-grants.sql
docker run --detach --name silicon-dm-api --restart unless-stopped \
  --env-file /secure/silicon-dm/runtime.env --publish 127.0.0.1:8080:8080 silicon-dm:0.17.1 dm-api
docker run --detach --name silicon-dm-worker --restart unless-stopped \
  --env-file /secure/silicon-dm/runtime.env silicon-dm:0.17.1 dm-worker
```

The image contains `dm-api`, `dm-worker` and `dm-migrate` and runs as UID/GID 10001. Use private `--env-file`s; never
source them as shell code. The move of an earlier deployment to Silicon Accounts (DM 0.15) was a coordinated change,
recorded in [the cutover runbook](../migration/cutover.md).

## Client release

The `dm` command is distributed only through Silicon Apps: the release workflow builds one archive per target with
`apps.yaml`, the archives are uploaded with `silicon-apps upload`, and Silicon Apps installs and updates the command;
DM never replaces itself. `silicon-dm-protocol` and `silicon-dm-client` go to crates.io for Rust integrations.
[Release DM](releasing.md) has every step.
