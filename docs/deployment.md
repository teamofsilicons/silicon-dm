# Deploying Silicon DM

This runbook covers the backend image, databases, IAM callback, and client
release. The Fargate deployment is defined in
[`deploy/aws/fargate.yaml`](../deploy/aws/fargate.yaml), with the migration task
and operating details in [the Fargate guide](../deploy/aws/README.fargate.md).
The general Docker commands below explain the underlying migration/runtime
steps. Fargate uses an explicitly invoked one-off bootstrap task before the
services are activated. See [manual verification](manual-backend-verification.md)
for observed results and remaining checks.

## AWS deployment

The Fargate template runs private ARM64 API and worker tasks behind a dedicated
Application Load Balancer in the existing production VPC. Separate encrypted
RDS PostgreSQL instances hold production and testing data. The stack owns its
security groups, task roles, CloudWatch logs, and rate-limiting WAF. Namecheap
points `backend.dm` at the load balancer, and an ACM DNS-validation record
permits certificate renewal.

The API has 2 vCPU and 8 GiB; the worker has 0.5 vCPU and 4 GiB. Both initially
run one task. The databases are single-AZ, with seven-day production backups
and one-day testing backups. Check regional Fargate capacity before deploying,
including the extra tasks needed during rolling replacement. The alternate
[EC2 template](../deploy/aws/production.yaml) and [EC2 guide](../deploy/aws/README.md)
remain available; EC2 uses a separate regional vCPU quota.

Build and push reviewed ARM64 runtime and bootstrap images to
`silicon-dm-production` in ECR. The runtime image includes the AWS RDS CA bundle;
the bootstrap image adds the migration tools. Pass their immutable digests,
issued certificate ARN, app-secret ARN, VPC, and public/private subnets to a
CloudFormation change set for `silicon-dm-production` in `us-east-1`. Inspect the
proposed resources before execution. Initially set `RuntimeDesiredCount=0`, run
the bootstrap task, verify exit code 0 and its restricted runtime-secret output,
then set the count to 1. The migration task rejects changes to existing database
credentials or the testing encryption key; credential rotation requires a
separate coordinated procedure.

Use ordinary rollback for updates that replace ECS task definitions;
`--disable-rollback` rejects replacement resources.

Later task replacements drain existing targets while clients reconnect and
replay. Fargate allows a 120-second container stop timeout; DM's shutdown deadline
is 110 seconds. An interrupted request remains subject to durable idempotent
retry. Credentials belong in Secrets Manager, never image layers, template
parameters, DNS records, or command arguments. Runtime tasks receive individual
restricted secret values through their execution role and have no AWS task role.

## Required configuration

The public origin is `https://backend.dm.teamofsilicons.com`. Register the exact
callback `https://backend.dm.teamofsilicons.com/webhook/` in IAM. The canonical
application ID is `tos>dm`; there is no OBO endpoint registration.

Keep runtime configuration in Secrets Manager. Fargate task definitions refer to
individual secret keys. For a host-managed Docker deployment, use private
environment files and do not source them as shell code: canonical IAM IDs contain
`>`, and secrets can contain shell metacharacters. Docker's `--env-file` reads
values without executing them.

Use separate configuration for the migrator and runtime:

| Setting | Migration process | API and worker |
| --- | --- | --- |
| `DM_ENVIRONMENT` | `production` | `production` |
| `DM_DATABASE_URL` | Production database, object-owning migration role | Same database, restricted runtime role |
| `DM_BIND_ADDR` | Not needed | `0.0.0.0:8080` inside the container |
| `DM_PUBLIC_BASE_URL` | Not needed | `https://backend.dm.teamofsilicons.com/api/v1` |
| `DM_IAM_BASE_URL` | Not needed | `https://backend.iam.teamofsilicons.com` |
| `DM_IAM_APP_ID` | Not needed | `tos>dm` |
| `DM_IAM_APP_SECRET` | Not needed | Registered application secret |
| `DM_IAM_WEBHOOK_SECRET` | Not needed | Registered callback signing secret |
| `DM_IAM_WEBHOOK_KEY_VERSION` | Not needed | Exact signing version returned by IAM |
| `DM_GIPHY_API_KEY` | Not needed | Valid application Giphy key |
| `DM_TEST_DATABASE_URL` | Not used | Separate testing database and schema-owning role, if enabled |
| `DM_TEST_KEY_ENCRYPTION_KEY` | Not used | Stable base64-encoded 32-byte key, if testing is enabled |

Production database URLs must include exactly one `sslmode=verify-full` query
parameter and use a host matching the database certificate. Install any required
trusted CA in the image or configure the database driver's certificate options;
do not disable verification to work around certificate errors. Other production
upstream URLs must use HTTPS. [`.env.example`](../.env.example) lists pool,
timeout, delivery, and provider settings with their defaults.

The testing database must be distinct from production. Its role needs
`CREATE ON DATABASE` and owns the schemas created for individual environments.
Test schemas migrate lazily when initialized through DM; do not run the
production migrator against that database. Keep the encryption key stable
across every API and worker replica and back it up with the databases.
Replacing it does not re-encrypt existing environment credentials.

## Build and migrate

Build the release image from the reviewed checkout:

```sh
docker build --tag silicon-dm:0.2.0 .
docker image inspect silicon-dm:0.2.0 --format '{{.Id}}'
```

Record the resulting image ID or registry digest for the deployment. The image
contains `dm-api`, `dm-worker`, and `dm-migrate`; it runs as UID/GID 10001.
The container filesystem holds no message database or local CLI state.

Back up the production database before upgrading. For the first deployment,
provision the production database and its distinct migration/runtime roles.
The following file paths are examples to replace with the host's private files:

```sh
docker run --rm \
  --env-file /secure/silicon-dm/migration.env \
  silicon-dm:0.2.0 dm-migrate
```

Run [the runtime grants](../deploy/runtime-grants.sql) as the migration role,
using a private PostgreSQL connection configuration such as a service file:

```sh
psql 'service=dm_migrator' --set ON_ERROR_STOP=1 \
  --set runtime_role=dm_runtime --file deploy/runtime-grants.sql
```

The grants permit DM table operations and migration-journal reads while
excluding historical Hook records. They also establish defaults for later
tables. The production journal is `public._sqlx_migrations`; a mismatch is an
error to investigate, not permission to rewrite existing migration history.

For upgrades from a version without persistent draft counters, discard old
client draft version tokens and reload drafts. Migration 0014 backfills live
drafts under a write lock, but cannot reconstruct the versions of drafts
deleted before the counter table existed.

## Start the runtime and configure ingress

Start API and worker with the runtime configuration. A host-managed deployment
can express these same settings through its service manager:

```sh
docker run --detach --name silicon-dm-api --restart unless-stopped \
  --env-file /secure/silicon-dm/runtime.env \
  --publish 127.0.0.1:8080:8080 \
  silicon-dm:0.2.0 dm-api

docker run --detach --name silicon-dm-worker --restart unless-stopped \
  --env-file /secure/silicon-dm/runtime.env \
  silicon-dm:0.2.0 dm-worker
```

The API also runs its own delivery pump for sockets connected to that process.
The standalone worker performs durable delivery maintenance; it does not own
another API process's WebSocket connections. Multiple replicas share the same
production database and testing configuration. Account for every replica's
connection pools when sizing PostgreSQL.

Configure the ingress to route these paths unchanged:

| Path | Required handling |
| --- | --- |
| `/api/v1/*` | HTTPS REST requests; preserve authorization, organization, idempotency, version, and testing headers |
| `/api/v1/ws` | WebSocket upgrade, query parameters, and negotiated subprotocol; no response buffering |
| `/webhook/` | POST with exact original body bytes and IAM signature headers; preserve the trailing slash |
| `/live`, `/ready` | HTTP probes; successful result is 204 |

Do not parse and re-encode webhook JSON at the proxy. Do not cache authenticated
responses or strip `Cache-Control: no-store`. WebSocket inactivity limits must
allow the application's 30-second pings and 120-second heartbeat policy; use an
ingress timeout above 120 seconds. Redact credentials, WebSocket authentication
subprotocols, testing root headers, and raw IAM test webhook bodies from logs.

Align ingress body limits with `DM_MAX_HTTP_BODY_BYTES` (128 MiB by default).
The logical text limit is 100 million Unicode characters; UTF-8 and JSON encoding
can require more bytes. Increase the explicit body/frame limits only with
adequate process memory and request timeouts. The backend separately limits
auth bodies to 16 KiB and IAM webhooks to 1 MiB.

Allow at least `DM_SHUTDOWN_TIMEOUT_SECONDS` for graceful shutdown. Do not delete
databases, encrypted environment metadata, or CLI queues when replacing an
image. Rollback of code requires checking schema compatibility; forward
migrations are not automatically reversed by starting an older image.

## Manual deployment acceptance

Perform these actions individually after ingress and the runtime are ready:

1. Read `/live` and `/ready` through the public HTTPS origin and verify 204.
   Readiness covers DM database/schema access; it does not prove IAM or Giphy.
2. Use the installed IAM CLI to obtain a fresh SLT with explicit IAM organization selection for the
   registered app. Log in through the DM CLI with a reachable local callback,
   and check `whoami`. Keep production and testing profiles separate.
3. Pair a new DM sandbox with an IAM testing environment. Sign in both intended
   recipients, send a message, observe the callback ACK and Delivered receipt,
   then explicitly mark it Read. Reconnect and inspect durable replay behavior.
4. Request Giphy trending and search with the configured real key. Inspect
   provider results and the null next cursor; discovery currently returns up to
   25 results without pagination.
5. Activate the registered production webhook in IAM. Trigger an authorized
   change on a designated test account, inspect DM receipt and IAM delivery
   status, and verify session revalidation. Exercise the signed test envelope
   separately against the paired sandbox. See [IAM integration](iam.md) for
   signer versions and environment binding.
6. Confirm the tested deployment image, migration version, probe results, and
   manually observed outcomes in a deployment record. Keep secret values and
   raw signed test envelopes out of that record.

The local temporary webhook tunnel used during development is stopped. It is
not a deployment dependency; replace its URL on the IAM test application before
expecting further test callbacks. Production uses the registered public URL.

## Rust package and CLI release

The client and CLI are published on crates.io. For each new release, publish
`silicon-dm-protocol`, then `silicon-dm-client`, then `silicon-dm-cli`, whose manifest depends on that
client version. Use the appropriate crates.io owner account and review the
package contents and release version before publication.

After publication, install the CLI into a separate directory/profile and
manually exercise `updates check`, `updates install`, `updates status`, and
the persisted enable/disable setting. Automatic replacement requires an
installed executable; it deliberately does not overwrite a checkout's debug
binary. The default Rust client reports available updates without state or file
changes. Its optional [SDK runtime](client/runtime.md) supplies a default-on
hourly policy and dependency-update/rebuild execution for an explicitly selected
application manifest; the consuming application owns policy persistence and
restarting with the rebuilt code.
