# Fargate deployment and CloudFormation recovery

Use `fargate.yaml` for private ARM64 Fargate API/worker tasks behind DM's own
ALB. It retains the database, ALB, subnet, security-group, WAF, and log-group
properties from the EC2 template wherever those resources can be reused. It
adds isolated ECS execution/bootstrap roles, an ECS cluster, a restricted
runtime secret, and an IP target group. Runtime services initially have zero
desired tasks so no application runs before migrations and grants complete.

The API uses 2 vCPU and 8 GiB; the worker uses 0.5 vCPU and 4 GiB. Rolling
deployments temporarily permit two tasks per service. Validate regional Fargate
quota and current usage before provisioning. Both services use private subnets
with public IP assignment disabled. They require the existing outbound NAT or
appropriate VPC endpoints plus outbound connectivity to Silicon Accounts and Giphy.

## Images

The manual [deployment image workflow](../../.github/workflows/deployment-images.yml) builds the backend runtime
(including the RDS CA wrapper) and the website's server image on native ARM64 runners. It
exports `dm-deployment-backend` and `dm-deployment-gateway` artifacts containing
Docker image archives, revision/version/platform metadata, and `SHA256SUMS`.
Verify those checksums and the expected commit, authenticate `crane` to ECR, then
run `crane push backend.tar <ecr-repository>:<release-tag>` to copy the archive;
no local Docker daemon is needed. The matching `dm-deployment-bootstrap`
artifact contains the migration/bootstrap image built from that exact runtime.
The workflow has no AWS credentials and performs no registry push or deployment.

Build both images from the reviewed checkout. `BACKEND_IMAGE` must identify
the immutable ARM64 backend image containing the reviewed binaries.

```sh
docker build --platform linux/arm64 \
  --file deploy/aws/Dockerfile.runtime \
  --build-arg BACKEND_IMAGE="$BACKEND_IMAGE" \
  --tag "$RUNTIME_IMAGE" .

docker build --platform linux/arm64 \
  --file deploy/aws/Dockerfile.bootstrap \
  --build-arg BACKEND_IMAGE="$RUNTIME_IMAGE" \
  --tag "$BOOTSTRAP_IMAGE" .
```

The runtime image only adds the AWS RDS CA bundle to the existing nonroot image.
The bootstrap image additionally contains Python, the AWS SDK, PostgreSQL
client tools, the migration binary, and canonical runtime grants. Push both to
the DM ECR repository and supply digest-pinned URIs as `BackendImageUri` and
`BootstrapImageUri`. Neither image contains credentials. The Dockerfile-specific
`Dockerfile.bootstrap.dockerignore` permits only `bootstrap-task.py` and the
canonical `runtime-grants.sql` through the repository's restrictive build context.

## Silicon Accounts settings

DM signs every account in with Silicon Accounts. The non-secret settings are stack parameters,
passed to the API and worker as environment variables:

| Parameter | Variable | Default |
| --- | --- | --- |
| `AccountsUrl` | `ACCOUNTS_URL` | `https://accounts.teamofsilicons.com` (exactly the `iss` of its tokens) |
| `AccountsApiUrl` | `ACCOUNTS_API_URL` | empty: DM calls `AccountsUrl` |
| `AppId` | `DM_APP_ID` | `dm` |
| `ProofIssuers` | `DM_PROOF_ISSUERS` | Silicon Interface (`interface`) for the five DM proof scopes |
| `TingUrl` | `DM_TING_URL` | empty: Ting delivery off; clients read the sync feed |

The application secret (`AppSecretArn`) is a JSON object with these fields:

| Field | Value |
| --- | --- |
| `DM_APP_SECRET` | DM's app secret at Silicon Accounts |
| `DM_ACCOUNTS_WEBHOOK_SECRET` | The app webhook signing secret (`whsec_…`) |
| `DM_DATA_KEY` | `openssl rand -base64 32`; keep it stable and backed up (sync cursors and stored proofs depend on it) |
| `DM_GIPHY_API_KEY` | Giphy application key |
| `DM_RUNTIME_DATABASE_PASSWORD` | Independently generated production runtime password |
| `DM_SPACE_STATION_TABLE_KEY`, `DM_POSTMARK_SERVER_TOKEN` | Optional, see below |

Bootstrap validates these fields (the webhook secret must start with `whsec_`, the data key must
decode to 32 bytes) and publishes the runtime secret the tasks read: the app secret, webhook
secret, data key, provider keys and the restricted database URL. DM's webhook at Silicon Accounts
is the stack output `WebhookUrl` (`https://backend.dm.teamofsilicons.com/webhook/`).

## Initial deployment

Supply the same VPC, subnet, certificate, application-secret, database-engine,
and public-host parameters as the EC2 deployment. Keep `RuntimeDesiredCount=0`
until the one-off bootstrap succeeds. Execute the reviewed change set with
rollback disabled so successful resources survive any provisioning failure.
This option must be selected before execution; it is not a switch for a stack
operation that is already running.

Once the cluster and task definitions exist, use the stack outputs to run one
`BootstrapTaskDefinitionArn` task in `ClusterName`, with the two private
subnets, `TaskSecurityGroupId`, public IPs disabled, Fargate launch type, and
platform version `1.4.0`. Inspect its specific task ARN until it stops. Read its
exit code and `/silicon-dm/production/bootstrap` CloudWatch log stream.
A zero exit and the final completion message mean that it has:

1. Read only the two approved secret ARNs: app configuration and production
   master.
2. Connected to the production RDS host with authenticated `sslmode=verify-full`
   and the baked-in RDS CA; configured `dm_runtime` without superuser,
   role-management, database-creation, replication, or RLS-bypass privileges.
3. Applied production migrations as the production master and the canonical
   restricted grants.
4. Written a separate Secrets Manager JSON document containing the restricted
   database URL and the runtime Silicon Accounts and provider settings. It does
   not include the RDS master password.

The testing RDS instance from the test-environment era is retained with its data;
neither bootstrap nor the tasks use it.

Role creation uses explicit restricted attributes. On an existing role, bootstrap
checks those attributes and changes only the password inside a transaction with
the database grants. It does not issue `ALTER ROLE ... NOSUPERUSER`,
`NOREPLICATION`, or `NOBYPASSRLS`: an RDS master is not a PostgreSQL superuser,
and PostgreSQL rejects those explicit attribute alterations even when the
requested value is already false.

Bootstrap sends no CloudFormation success signal and never pretends an EC2
instance exists. Application task definitions contain secret references, not
secret values. The runtime execution role can read only the runtime secret;
there is no AWS task role on API/worker containers. Bootstrap's separate task
role can read the source/master secrets and read/write the runtime secret, while its
execution role only pulls images and writes logs.

After bootstrap succeeds, execute a reviewed update with
`RuntimeDesiredCount=1`. Verify both services reach steady state and the API
IP targets pass `/ready` with 204. Perform public HTTPS, sign-in, sync feed,
Giphy, and real signed Silicon Accounts webhook checks individually as described in
[`docs/deployment.md`](../../docs/deployment.md). A service reaching steady
state is infrastructure evidence, not end-to-end application verification.

Before any database mutations, bootstrap verifies that an already initialized
runtime secret still matches the database URL (and so the runtime password) and
`DM_DATA_KEY`. It rejects changes to those values; ordinary bootstrap is not
a password-rotation mechanism. Use a separately reviewed rotation/re-encryption
procedure rather than overwriting credentials or clearing the runtime secret.
The keys a DM 0.14 bootstrap published for its earlier sign-in, test
environments and installer (`RETIRED` in `bootstrap-task.py`) are carried over
unchanged and unused, so the 0.14 task definitions can still start during a
rollback; remove them once the rollback window has closed
([the cutover runbook](../../docs/migration/cutover.md)).

Use ordinary rollback for updates that replace task definitions. CloudFormation
rejects replacement resources when an update uses `--disable-rollback`; reserve
that preservation option for initial provisioning or a reviewed recovery that
does not replace resources.

The move to Silicon Accounts (DM 0.15) is a coordinated cutover: stop both old
services, migrate, re-key identities, then start the new API and worker together.
Follow [the cutover runbook](../../docs/migration/cutover.md).

For compatible upgrades, run the new bootstrap/migrator task and inspect its
successful completion before deploying new API/worker task definitions. Verify
migration compatibility with the old running version beforehand. Secret rotations
need new tasks to consume the new values; running tasks do not reread Secrets Manager.

## Shutdown and large requests

The ALB has a 180-second idle timeout and a 180-second target drain period.
The app accepts 128 MiB request/frame bodies and has a 120-second REST deadline.
The logical content maximum remains 100 million Unicode characters subject to
the encoded byte limit. WAF uses IP-rate protection with no message-content
inspection rules or sampled request capture.

Fargate allows at most 120 seconds for `StopTimeout`, so DM's graceful-shutdown
deadline is 110 seconds and the task stop timeout is 120 seconds. A request
that uses the entire REST allowance can still be interrupted during shutdown;
clients must retry using the same durable idempotency key. Sync long-polls last
at most 30 seconds and clients resume from their cursor. These containers do not
need a writable root filesystem or a Fargate-unsupported `tmpfs` option.

## Recovering an incomplete EC2 stack without recreating RDS

Do not modify or retire a stack solely because an observation timed out. First
inspect its authoritative status, events, and the exact failed scaling activity.
A pending EC2 quota request is not approval. The ASG resource-signal timeout
starts after resource stabilization, so its timestamp cannot be inferred simply
from the first `CREATE_IN_PROGRESS` event.

If the EC2 creation must be retired, inspect the actual protection settings and
inventory every DM-owned resource first. RDS and ALB deletion protection remain
enabled. Do not disable those protections, delete production data, or touch other
services to clear capacity. A default-rollback creation cannot be converted to
preserve-resources mode while it is running. Stack termination protection does
not disable creation rollback.

A reviewed recovery should proceed from actual resulting states:

1. Preserve the resource inventory and original template/parameters before any
   stack operation. Verify that the selected stack is the new DM stack.
2. If stack deletion reaches `DELETE_FAILED`, use an explicit reviewed list of
   surviving logical resource IDs with `RetainResources`. Include dependencies
   and retained log groups, not just databases and ALB. The API documents this
   option for `DELETE_FAILED`; do not assume it applies to an arbitrary state.
3. Build a foundation-only import template describing the survivors exactly,
   including immutable security-group descriptions, names, DB subnet/parameter
   groups, and every required reference. Give imported resources a
   `DeletionPolicy`. Resolve resource identifier fields with
   `get-template-summary`; do not guess them.
4. Import the survivors into the final DM stack with an `IMPORT` change set.
   Import must not create, delete, or change properties of other resources.
   Inspect the actual survivors after completion and verify their settings.
5. Only after `IMPORT_COMPLETE`, update to the full `fargate.yaml` with desired
   count zero, bootstrap, then activate the services. The final IP target group's
   name is `silicon-dm-prod-ip-api`, allowing replacement of a surviving
   instance-target group without a fixed-name collision.

This is a recovery procedure to adapt to verified resource states, not an
instruction to delete a live stack automatically. The final CloudFormation
state must describe the resources that really serve DM; never send a fabricated
ASG readiness signal to bypass the failed EC2 resource.

## Validation

`cfn-lint deploy/aws/fargate.yaml` and AWS `validate-template` validate the
infrastructure structure. Also compile-check `bootstrap-task.py` without running
it. Runtime verification is performed manually against the real selected task,
service, databases, and public endpoint. No automated application test scenarios
are required by this deployment workflow.

References: [Fargate IP target groups](https://docs.aws.amazon.com/AmazonECS/latest/APIReference/API_LoadBalancer.html),
[resource import restrictions](https://docs.aws.amazon.com/AWSCloudFormation/latest/UserGuide/import-resources-manually.html),
[preserving resources on failure](https://docs.aws.amazon.com/AWSCloudFormation/latest/UserGuide/stack-failure-options.html),
[DeleteStack retention](https://docs.aws.amazon.com/AWSCloudFormation/latest/APIReference/API_DeleteStack.html),
[CreationPolicy timing](https://docs.aws.amazon.com/AWSCloudFormation/latest/TemplateReference/aws-attribute-creationpolicy.html).


## DM 0.5 diagnostics and report configuration

The application secret may include `DM_SPACE_STATION_TABLE_KEY` and
`DM_POSTMARK_SERVER_TOKEN`. Bootstrap validates and copies these optional fields
into the restricted runtime secret without rotating database credentials. Missing
values become empty strings: telemetry export stays inactive without its key,
and production report submission returns unavailable without a Postmark token.
After changing either value, rerun bootstrap and replace the API/worker tasks so
ECS injects the updated secret version.

The runtime image creates `/var/lib/silicon-dm/telemetry` as UID 10001, mode 0700,
and declares it as a Docker volume. Each Fargate task mounts its own writable
`telemetry` volume at that exact path, preserving image ownership while keeping
the root filesystem read-only. The spool survives a process restart within the
task; unsent diagnostics can be lost when the task is replaced. It contains no
message payloads or authentication credentials. Message durability is independent
of this best-effort telemetry spool.
