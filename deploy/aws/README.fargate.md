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
appropriate VPC endpoints plus outbound connectivity to IAM and Giphy.

## Images

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

1. Read only the three approved secret ARNs: app configuration, production
   master, and testing master.
2. Connected to both RDS hosts with authenticated `sslmode=verify-full` and the
   baked-in RDS CA; configured `dm_runtime` and `dm_testing` without superuser,
   role-management, database-creation, replication, or RLS-bypass privileges.
3. Applied production migrations as the production master and the canonical
   restricted grants. Test schemas are still created/migrated only through DM's
   isolated environment lifecycle, with `CREATE ON DATABASE` for `dm_testing`.
4. Written a separate Secrets Manager JSON document containing restricted
   database URLs and the runtime app/provider settings. It does not include
   either RDS master password.

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
IP targets pass `/ready` with 204. Perform public HTTPS, login, message replay,
Giphy, and real signed IAM callback checks individually as described in
[`docs/deployment.md`](../../docs/deployment.md). A service reaching steady
state is infrastructure evidence, not end-to-end application verification.

Before any database mutations, bootstrap verifies that an already initialized
runtime secret still matches both database URLs/passwords and the testing
encryption key. It rejects changes to those values; ordinary bootstrap is not
a password-rotation mechanism. Use a separately reviewed rotation/re-encryption
procedure rather than overwriting credentials or clearing the runtime secret.

Use ordinary rollback for updates that replace task definitions. CloudFormation
rejects replacement resources when an update uses `--disable-rollback`; reserve
that preservation option for initial provisioning or a reviewed recovery that
does not replace resources.

For upgrades, run the new bootstrap/migrator task and inspect its successful
completion before deploying new API/worker task definitions. Verify migration
compatibility with the old running version beforehand. Secret rotations need
new tasks to consume the new values; running tasks do not reread Secrets Manager.

## Shutdown and large requests

The ALB has a 180-second idle timeout and a 180-second target drain period.
The app accepts 128 MiB request/frame bodies and has a 120-second REST deadline.
The logical content maximum remains 100 million Unicode characters subject to
the encoded byte limit. WAF uses IP-rate protection with no message-content
inspection rules or sampled request capture.

Fargate allows at most 120 seconds for `StopTimeout`, so DM's graceful-shutdown
deadline is 110 seconds and the task stop timeout is 120 seconds. A request
that uses the entire REST allowance can still be interrupted during shutdown;
clients must retry using the same durable idempotency key. WebSockets reconnect
and replay committed deliveries. These containers do not need a writable root
filesystem or a Fargate-unsupported `tmpfs` option.

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
