---
title: Deploy the DM service
description: The runbook for the DM service image, its database migrations and runtime grants, the Silicon Accounts webhook, speech, ingress, and the checks to run after every deployment.
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
| `DM_DEEPGRAM_API_KEY` | not needed | Deepgram key for voices, spoken messages, listening and transcription |

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
8. Record the image digest, migration version and observed outcomes (no secrets).

## Client release

The `dm` command is distributed only through Silicon Apps: the release workflow builds one archive per target with
`apps.yaml`, the archives are uploaded with `silicon-apps upload`, and Silicon Apps installs and updates the command;
DM never replaces itself. `silicon-dm-protocol` and `silicon-dm-client` go to crates.io for Rust integrations.
[Release DM](releasing.md) has every step.
