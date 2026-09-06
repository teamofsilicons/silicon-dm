# Browser gateway and Vercel frontend

The SolidJS frontend runs on Vercel at `https://dm.teamofsilicons.com`.
Its API and WebSocket traffic connects directly to
`https://gateway.dm.teamofsilicons.com`, behind the existing DM ALB. The gateway
uses the existing backend at `https://backend.dm.teamofsilicons.com`.

`gateway.yaml` defines a separate gateway stack. It adds a private ARM64
`t4g.medium` host, a dedicated security group, an encrypted 16 GiB gp3 state
volume, CloudWatch logs, and the necessary ALB host rules and certificate.
It does not replace the backend ECS services or databases. The ALB security
group receives one egress rule to the gateway; gateway ingress is restricted to
that ALB on port 4315. Administration uses SSM with no SSH or public host IP.

The runtime image is built from `web/Dockerfile.gateway` with the `web` directory
as context. Only gateway source, package manifests, and its build script enter
the image. The image runs as UID 10001 with a read-only root, dropped
capabilities, and no AWS credentials. EC2 IMDSv2 uses hop limit 1; the host handles
ECR authentication and CloudWatch logging. Pin the image by ECR digest.

## Persistent state and replacement

Session files are private JSON, protected by directory mode 0700 and file mode
0600. Encryption at rest comes from EBS, not an application encryption layer.
Never upload the state directory to Vercel or copy local test sessions into
production. Back up the encrypted state volume using access-controlled EBS
snapshots. This is a single-host deployment, not a highly available session store.

Bootstrap matches the state volume's exact Nitro serial. It formats only a
signature-free device whose EBS metadata proves it is a snapshot-free volume
created by this stack within the past hour and attached to this exact instance;
otherwise it requires the expected ext4 label
and volume identity marker. It mounts by filesystem UUID. The service refuses
to start without the mounted volume. A host-level `flock` prevents simultaneous
service containers; after a stopped container is removed, its obsolete PID-based
application lock is cleared under that host lock.

The instance and state volume are retained on stack deletion or replacement.
Install the generated stack policy to reject accidental instance, volume, and
attachment replacement/deletion. Termination protection also applies to the EC2
instance and stack. Recovery requires explicitly stopping the old service,
snapshotting state, and reviewing attachment/replacement before bringing up a
single replacement host. Never format an existing volume or start two gateways
against the same state. A normal gateway restart preserves browser profiles.

Changing `GatewayImageUri` in EC2 user data does not update an already-running
container. For code updates, use SSM to pull the reviewed digest, stop the service,
update the exact image reference in its unit, reload systemd, restart, and verify
health and browser login. Preserve the prior unit and image digest for rollback.
Update the template parameter as the recorded desired configuration separately.

## Deployment

The source template contains a bootstrap marker. Render before validation:

```sh
python3 deploy/aws/render-gateway.py > /private/path/gateway-rendered.yaml
python3 deploy/aws/render-gateway.py --stack-policy > /private/path/gateway-policy.json
bash -n deploy/aws/gateway-bootstrap.sh
aws cloudformation validate-template --template-body file:///private/path/gateway-rendered.yaml
```

Provide the existing VPC, private subnet and its availability zone, ALB security
group, HTTP/HTTPS listener ARNs, issued gateway certificate ARN, and gateway ECR
digest. Inspect priorities first: this stack uses priority 2 on both listeners.
Review a CREATE change set, then execute it with resource preservation for initial
creation. `GatewayReady` waits for bootstrap's real `/healthz` result. Check ALB
target health separately. Do not fake the readiness signal to unblock a failure.

Vercel project `silicon-dm-frontend` uses the `web` directory. Its public build
variable is `VITE_DM_GATEWAY_ORIGIN=https://gateway.dm.teamofsilicons.com`.
Build Output API assets contain no Functions, server session state, or credentials.
The custom frontend and gateway domains must remain same-site for the HttpOnly
SameSite=Lax session cookie. Configure the exact frontend origin at the gateway;
default `vercel.app` preview domains cannot authenticate to this production gateway.

Keep ACM's validation CNAME for renewal. Point `gateway.dm` to the existing DM ALB
and `dm` to the Vercel-recommended CNAME. Add only these records, preserving other
DNS entries. Keep ALB access logs disabled because IAM callback query strings
contain short-lived credentials. No OBO endpoints are added.

Finish with manual HTTPS, CORS, IAM callback, session, conversation, and WebSocket
checks on the custom production domains. A successful infrastructure deployment
does not prove authentication or messaging works.
