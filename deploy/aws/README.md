# AWS production deployment

The Fargate deployment is described in [README.fargate.md](README.fargate.md)
and uses `fargate.yaml`. The EC2 template and bootstrap below remain available
for environments with sufficient EC2 capacity.

`production.yaml` gives DM its own public ALB, HTTPS certificate attachment,
private EC2 Auto Scaling group, production RDS PostgreSQL instance, separate
RDS testing instance, private security groups, CloudWatch log groups, and
instance role. It reuses only the existing VPC and public/private subnets.
It does not change IAM, Briefcase, or the shared platform ALB/WAF.

## Inputs

Supply these CloudFormation parameters after inspecting the target AWS account:

| Parameter | Meaning |
| --- | --- |
| `VpcId` | Existing VPC with DNS resolution enabled |
| `PublicSubnetA`, `PublicSubnetB` | Different availability zones with internet-gateway routes for the ALB |
| `PrivateSubnetA`, `PrivateSubnetB` | Different availability zones with outbound access for package installation, ECR, Secrets Manager, IAM, Giphy, SSM, and CloudWatch |
| `CertificateArn` | Issued ACM certificate in the same region covering `backend.dm.teamofsilicons.com` |
| `AppSecretArn` | DM-specific Secrets Manager secret described below |
| `BackendImageUri` | Reviewed ARM64 ECR image, preferably pinned with `@sha256:...` |

Defaults use `t4g.medium` (4 GiB) for the application, `db.t4g.small` for
production, and `db.t4g.micro` for testing. PostgreSQL defaults to 17.9; inspect
regional engine availability before creating the change set. These are initially
single-instance/single-AZ databases, with seven-day production backups and
one-day testing backups. The ASG runs one host and allows a second temporarily
for rolling updates. This is a cost-conscious starting deployment, not a
multi-AZ availability guarantee. Size memory and database connections for
measured concurrency, especially when receiving several very large messages.

Create the ECR repository `silicon-dm-production` separately before deploying.
The role can read images only from that repository in the stack account/region.
Use `CAPABILITY_NAMED_IAM` for the named, DM-specific instance role.

## Secret schema

Store a JSON object with these fields in `AppSecretArn`. Do not put secret
values in template parameters, committed files, shell arguments, or outputs.

| Field | Required value |
| --- | --- |
| `DM_IAM_APP_ID` | Canonical IAM app identifier, `tos>dm` |
| `DM_IAM_APP_SECRET` | Registered IAM application credential |
| `DM_IAM_WEBHOOK_SECRET` | Registered webhook signing secret |
| `DM_IAM_WEBHOOK_KEY_VERSION` | Positive **JSON integer** matching the IAM signing version |
| `DM_GIPHY_API_KEY` | Real Giphy application key |
| `DM_TEST_KEY_ENCRYPTION_KEY` | Base64 encoding of 32 random bytes; keep stable and back up |
| `DM_RUNTIME_DATABASE_PASSWORD` | Independently generated production runtime password |
| `DM_TEST_DATABASE_PASSWORD` | Independently generated testing runtime password |

Bootstrap reads secrets through the host instance role. It creates `dm_runtime`
with no DDL/role-management privileges in production and `dm_testing` with
`CREATE ON DATABASE` in the distinct testing database. The latter owns only
schemas it creates for isolated environments. RDS manages each database master
credential. Only bootstrap uses master credentials, applies the embedded
production migrations, and grants the runtime privileges from
[`runtime-grants.sql`](../runtime-grants.sql). It does not migrate the testing
database as production; isolated schemas migrate through DM's lifecycle.

Temporary master/application JSON and migrator environment files are removed
when bootstrap exits. Runtime configuration is root-owned mode 0600 under
`/etc/silicon-dm`, with no master passwords. Its values are parsed as data,
never sourced as shell code. Containers run as UID/GID 10001 with a read-only
root filesystem, no Linux capabilities, and no privilege escalation. EC2
metadata is blocked for bridged containers; the host handles ECR pulls and
CloudWatch logging. No AWS SDK credentials are passed into DM.

## Transport and lifecycle

RDS requires TLS, and every PostgreSQL URL uses `sslmode=verify-full` plus the
mounted AWS RDS CA bundle. Production and testing run on separate RDS hosts.
The HTTPS ALB preserves request paths and bytes, supports native WebSocket
upgrades, and forwards only the configured DM hostname. Port 80 redirects to
HTTPS. The dedicated WAF rule limits each source IP to approximately 2,000
requests per five minutes. It has no body-content rules and disables sampled
requests so arbitrary messages, webhook payloads, and credentials are not
captured in WAF samples. Adjust the IP limit for shared NAT workloads if needed.

The app's HTTP/frame limit remains 128 MiB (`134217728` bytes), with a
120-second REST deadline and 60-second database statement deadline. The logical
text maximum is 100 million Unicode characters; UTF-8 and JSON still must fit
the configured byte limit. ALB idle timeout is 180 seconds, above the 120-second
WebSocket heartbeat policy. Target deregistration permits 180 seconds to drain;
DM receives SIGTERM and has a 130-second graceful-shutdown deadline, while
Docker waits 135 seconds and systemd allows 145 seconds. Clients reconnect and
replay their durable queues when a socket closes during replacement.

CloudFormation waits for the instance's authenticated database setup and local
`/ready` response of 204 before accepting its creation signal. ALB health checks
also use `/ready` and expect 204. The API and worker are managed by systemd with
CloudWatch logs under `/silicon-dm/production/api` and `/worker`. Inspect both
processes through SSM after deployment; the readiness probe does not prove a
successful IAM or Giphy request.

RDS and ALB deletion protection are enabled. Database deletion/replacement takes
snapshots and logs are retained. A failed stack creation may need operator
recovery because protected resources cannot be blindly rolled back. Never drop
production databases or disable protection merely to make a deployment retry.
Back up before an upgrade; CloudFormation code rollback does not undo schema
migrations. Changing the encryption key breaks existing encrypted test secrets.
Update Secrets Manager and roll the ASG deliberately when rotating credentials;
running containers do not automatically reread the secret.

## Editing and validation

`bootstrap.sh` is the source of the embedded user data. It deliberately contains
CloudFormation placeholders and is **not** a workstation deployment command.
After changing it or the canonical grants, run:

```sh
python3 deploy/aws/render.py
bash -n deploy/aws/bootstrap.sh
cfn-lint deploy/aws/production.yaml
aws cloudformation validate-template --template-body file://deploy/aws/production.yaml
```

The renderer uses only the Python standard library and limits unexpanded user
data to leave room below EC2's 16 KiB limit after parameter substitution.
It rewrites only the template's user-data block. Keep the rendered template
reviewed alongside its source. These checks validate packaging/infrastructure
syntax; they are not automated application test scenarios.

Create and review a CloudFormation change set, then execute it. When the stack
is healthy, create the Namecheap CNAME for `backend.dm` using the stack's
`LoadBalancerDnsName` output. Preserve unrelated DNS records. Keep ACM's DNS
validation CNAME for renewal. Finish the individually performed deployment
checks in [`docs/deployment.md`](../../docs/deployment.md), including CLI login,
large-message delivery/replay, Giphy results, and the actual signed IAM webhook
at `https://backend.dm.teamofsilicons.com/webhook/`. There are no OBO endpoints.

AWS references: [ALB attributes](https://docs.aws.amazon.com/elasticloadbalancing/latest/application/edit-load-balancer-attributes.html),
[Auto Scaling creation/update signals](https://docs.aws.amazon.com/AWSCloudFormation/latest/UserGuide/quickref-ec2-auto-scaling.html),
and [RDS PostgreSQL TLS](https://docs.aws.amazon.com/AmazonRDS/latest/UserGuide/PostgreSQL.Concepts.General.SSL.html).
