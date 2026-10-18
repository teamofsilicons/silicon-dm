---
title: Deploy the DM service
description: The runbook for the DM service image, its database migrations and runtime grants, the Silicon Accounts webhook, speech, ingress, the website and its uploads, the decisions only the Carbon makes, and the checks to run after every deployment.
kind: instructive
order: 80
related:
  - operations/releasing.md
  - reference/configuration.md
  - reference/limits.md
---

# Deploy the DM service

This runbook covers the service image, the database, the Silicon Accounts webhook and the client release. The Fargate
deployment is defined in [`deploy/aws/fargate.yaml`](../../deploy/aws/fargate.yaml), with the migration task and
operating details in [the Fargate guide](../../deploy/aws/README.fargate.md). The move of an existing deployment to
Silicon Accounts (DM 0.15) was a coordinated change, recorded in [the cutover runbook](../migration/cutover.md).

A deployment changes what every Carbon and Silicon uses at once, so each step below is a deliberate command run by
the Carbon responsible for production, and nothing here runs from CI by itself.

## AWS deployment

The Fargate template runs private ARM64 API and worker tasks behind a dedicated Application Load Balancer in the
production VPC, with an encrypted RDS PostgreSQL instance. The stack owns its security groups, task roles, CloudWatch
logs and rate-limiting WAF; the service's DNS name points at the load balancer. The alternate
[EC2 template](../../deploy/aws/production.yaml) and [EC2 guide](../../deploy/aws/README.md) remain available.

Build and push reviewed ARM64 runtime and bootstrap images to `silicon-dm-production` in ECR and pass their immutable
digests to a CloudFormation change set. Inspect it before execution. Start with `RuntimeDesiredCount=0`, run the
bootstrap (migration) task, check it exited 0, then raise the count. Credentials belong in Secrets Manager, never in
image layers, template parameters or command arguments. Fargate allows a 120-second stop timeout; DM's shutdown
deadline is 110 s.

## Required configuration

The public origin is `https://api.dm.teamofsilicons.com`. DM's app at Silicon Accounts (`dm`) needs its webhook at
`https://api.dm.teamofsilicons.com/webhook/` (every update, including custodian changes) and, for the command,
`device_flow` and `public_client` turned on.

| Setting | Migration process | API and worker |
| --- | --- | --- |
| `DM_ENVIRONMENT` | `production` | `production` |
| `DM_DATABASE_URL` | Object-owning migration role | Same database, restricted runtime role |
| `DM_BIND_ADDR` | not needed | `0.0.0.0:8080` |
| `DM_PUBLIC_BASE_URL` | not needed | `https://api.dm.teamofsilicons.com/api/v1` |
| `ACCOUNTS_URL` | not needed (`dm-migrate link-identities` uses it) | `https://accounts.teamofsilicons.com` |
| `ACCOUNTS_API_URL` | optional | Optional server-to-server origin |
| `DM_APP_ID` | not needed | `dm` |
| `DM_APP_SECRET` | not needed | DM's app secret |
| `DM_ACCOUNTS_WEBHOOK_SECRET` | not needed | The app webhook secret (`whsec_…`) |
| `DM_DATA_KEY` | not needed | 32 random bytes, base64; stable and backed up |
| `DM_PROOF_ISSUERS` | not needed | Apps allowed per proof scope |
| `DM_TING_URL` | not needed | Unset until Ting accepts Silicon Accounts proofs |
| `DM_GIPHY_API_KEY` | not needed | Giphy key |
| `DM_DEEPGRAM_API_KEY` | not needed | Deepgram key for voices, spoken messages, listening and transcription ([a Carbon's decision](#decisions-for-the-carbon)) |
| `DM_REPORTS_PER_HOUR` | not needed | Bug reports one account may send an hour (default 10) |

Without `DM_DEEPGRAM_API_KEY` the service runs, says `speech.enabled: false` in `GET /api/v1/accounts`, and answers
the speech routes with 503 `speech_not_configured`; everything else works. Both the API and the worker render speech
(they share the work through the database), so both need the key. The Fargate template does not pass it yet: adding
the secret and the variable to both task definitions is part of turning speech on. The speech limits
(`DM_SPEECH_RENDERS_PER_MINUTE`, `DM_SPEECH_DAILY_CHARACTERS`, the cache size) are in
[configuration](../reference/configuration.md#speech).

Production database URLs must include exactly one `sslmode=verify-full` and a host matching the certificate; never
disable verification. Other production upstream URLs use HTTPS. [`.env.example`](../../.env.example) lists pool,
timeout, delivery and provider settings.

## Build and migrate

```sh
docker build --tag silicon-dm:0.17.0 .
docker run --rm --env-file /secure/silicon-dm/migration.env silicon-dm:0.17.0 dm-migrate
psql 'service=dm_migrator' --set ON_ERROR_STOP=1 \
  --set runtime_role=dm_runtime --file deploy/runtime-grants.sql
```

The image contains `dm-api`, `dm-worker` and `dm-migrate` and runs as UID/GID 10001. Back up the database before
upgrading. `dm-migrate` only applies migrations; `dm-migrate link-identities` and
`draft-identity-mapping` re-key identities from before Silicon Accounts at cutover. The journal is
`public._sqlx_migrations`; a mismatch is an error to investigate, never permission to rewrite history. Use private
`--env-file`s; never source them as shell code.

Run `deploy/runtime-grants.sql` after every migration: a new table (0.17 adds `account_voices`, `speech_renders`,
`speech_usage`, `speech_audio` and `conversation_read_states`) is unusable by the runtime role until it is granted.

## Start the runtime and configure ingress

```sh
docker run --detach --name silicon-dm-api --restart unless-stopped \
  --env-file /secure/silicon-dm/runtime.env --publish 127.0.0.1:8080:8080 silicon-dm:0.17.0 dm-api
docker run --detach --name silicon-dm-worker --restart unless-stopped \
  --env-file /secure/silicon-dm/runtime.env silicon-dm:0.17.0 dm-worker
```

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
default). Replacing an image never deletes data; rolling back code requires checking schema compatibility.

## The website

The website is a Next.js server (`web/Dockerfile.gateway`, port 4315), deployed by
[`deploy/aws/web.yaml`](../../deploy/aws/web.yaml) behind the shared load balancer at `https://dm.teamofsilicons.com`
([the website's deployment notes](../../deploy/aws/README.web.md)). Browsers talk only to it: it signs Carbons in,
forwards their calls to the DM service through its same-origin proxy (`/api/…`), and saves their files and voice notes
in Silicon Briefcase. Its settings are in [configuration](../reference/configuration.md#the-website).

| Path | Required handling |
| --- | --- |
| `POST /api/uploads` | The request body is the file itself, of any size up to Briefcase's 5 TiB: no body limit and no request buffering (the website pipes the bytes on to Briefcase as they arrive; a proxy that buffers first holds the whole file and delays every byte), and no deadline for the whole request. An ordinary idle timeout is enough (60 seconds or more): bytes flow the whole time, and the website answers within about 20 seconds of the last one (see below). With nginx: `client_max_body_size 0; proxy_request_buffering off; proxy_http_version 1.1;` on that location, with `proxy_read_timeout` and `proxy_send_timeout` of at least 60 s |
| `GET`, `DELETE /api/uploads/saving` | A file the website is still saving into Briefcase: each `GET` waits up to 25 seconds for the outcome (allow 60), `DELETE` cancels it |
| `GET`, `HEAD /api/uploads/config` | Small JSON; whether uploads are on, and the largest file |
| `GET`, `HEAD /api/uploads/file` | Streams the files of messages (photos, audio, video) from Briefcase: keep `Range`, `If-Range` and `If-None-Match` on the way in and `206`, `Content-Range`, `Accept-Ranges` and `ETag` on the way out, never buffer or cache them (they are `Cache-Control: private`), and keep the website's `Content-Security-Policy: sandbox` and `nosniff` |
| every page | Keep the website's own headers, in particular `Permissions-Policy: camera=(), microphone=(self), …`: voice notes and dictation need the microphone, and an edge that sends `microphone=()` instead turns them off |
| `/api/…` (the proxy) | As the service's routes above: the sync long-poll holds 45 s, speech answers within 200 s, transcription uploads up to `DM_TRANSCRIBE_MAX_BYTES` within 10 minutes |
| `/auth/callback` | Registered at Silicon Accounts as `PUBLIC_URL/auth/callback` before traffic moves |

The website keeps one replica (its session refresh uses an in-process cache, and a file it is still saving into
Briefcase lives in its memory: with several, a Carbon's requests must reach the same one), and changing `SESSION_SECRET`
signs every browser out. `web.yaml` does not set `BRIEFCASE_API_URL` yet, so uploads stay off in production (the website
says so and offers links instead) until it is set, which also needs Briefcase's side below.

**Uploads and the website's server.** An upload holds two connections for as long as its bytes take: the browser's, and
the website's own to Briefcase. Nothing is written to the website's disk and its memory stays bounded (measured on a
production build: a 6 GiB upload at 2 GB/s took a fresh server from 107 to 291 MB, three more at once added nothing, and
at 100 MB/s it does not move; a request to a page can make it hold at most 1 MB), so the task's size does not limit a
file: 512 MB is plenty. Node ends a request whose body is still arriving after five minutes; the website lifts that
deadline itself on its own server (headers must still arrive within 60 seconds), so the limits that remain are the
ingress's and Briefcase's:

- **Idle timeouts.** Bytes flow the whole time a file is sent. After the last byte, Briefcase checks the file's digest
  and stores it before it answers, and for a large file that takes minutes with nothing moving. A load balancer would
  cut the browser's request after its idle timeout (production's: 180 seconds) and the file would be lost, so the
  website answers the browser within 20 seconds of the last byte instead (`202`, with an id), keeps saving, and the
  page asks `GET /api/uploads/saving` until it has the link, each ask waiting up to 25 seconds. No timeout in front
  of the website needs raising. The website's own request to Briefcase does wait in silence, up to 45 minutes: what
  Briefcase's ingress allows after the last byte (its nginx: 900 seconds on the transfer route) bounds the largest
  file, with Briefcase's own limits below. The website stops a transfer where nothing moves for 5 minutes.
- **Briefcase's own limits.** Its upload deadline (`BRIEFCASE_UPLOAD_TIMEOUT_SECONDS`, 900 seconds by default) bounds
  how long one file may take from its first byte to its answer, and its temporary disk holds each file while it is
  checked: Briefcase's operators size both for the files they expect ([Limits](../reference/limits.md#the-dm-command-and-the-website)).

### Uploads: the Briefcase side

The DM service never handles files. The website's server saves a Carbon's file into the Carbon's own Silicon
Briefcase, under `apps/dm/`, as DM acting for the Carbon: Silicon Accounts issues it a User verification proof for the
receiving app `briefcase` with the scopes `briefcase.uploads.reserve`, `briefcase.uploads.commit`,
`briefcase.uploads.status`, `briefcase.uploads.cancel` and `briefcase.link_access.update`, and Briefcase checks it.
Production Briefcase accepts those proofs only from the apps its `BRIEFCASE_PROOF_ISSUERS` lists for each scope: until
it lists `dm` for all five, uploads answer 503 `briefcase_not_allowed` and the website offers links instead. Then set
`BRIEFCASE_API_URL` on the website (and `UPLOADS_MAX_BYTES` only if you want a limit below Briefcase's 5 TiB) and
check an upload, a voice note and a photo in a message end to end, then one large file (a few gigabytes) to see the
ingress timeouts above hold.

## Decisions for the Carbon

These change what DM may do for every Carbon and Silicon, and what it costs, so the Carbon responsible for production
makes them; nothing in this repository makes them by itself.

- **Speech in production.** Put a Deepgram key into DM's production secrets and pass it to both the API and the worker
  as `DM_DEEPGRAM_API_KEY` (the Fargate template does not yet). Deepgram then renders and transcribes, billed to that
  key, within the per-account limits in [configuration](../reference/configuration.md#speech).
- **Uploads into Briefcase.** List `dm` in production Briefcase's `BRIEFCASE_PROOF_ISSUERS` for the five upload scopes
  above, then set `BRIEFCASE_API_URL` on the website. DM can then write into every Carbon's Briefcase, only under
  `apps/dm/` and only with that Carbon's own sign-in, files of any size up to Briefcase's 5 TiB.

The Carbon turned both on for DM 0.17 (18 October 2026): speech with a Deepgram key, and uploads with `dm` on
Briefcase's issuer list. Turning either off again is the Carbon's decision too.

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
9. Record the image digest, migration version and observed outcomes (no secrets).

## Client release

The `dm` command is distributed only through Silicon Apps: the release workflow builds one archive per target with
`apps.yaml`, the archives are uploaded with `silicon-apps upload`, and Silicon Apps installs and updates the command;
DM never replaces itself. `silicon-dm-protocol` and `silicon-dm-client` go to crates.io for Rust integrations.
[Release DM](releasing.md) has every step.
