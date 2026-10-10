# DM Next.js website

`web/Dockerfile.gateway` builds the Next standalone website at port 4315. Its artifact retains the gateway name so `deployment-images.yml` exports it with the existing backend artifacts. It no longer runs the Solid gateway or reads its state volume.

Create a **separate** stack from `web.yaml` with the existing ECS cluster ARN, VPC/private subnets, ALB security group, HTTPS listener, certificate for `dm.teamofsilicons.com`, digest-pinned website image and a Secrets Manager JSON secret containing `APP_SECRET` and a stable random `SESSION_SECRET` of at least 32 bytes. The execution role can read that one secret. Subnets need HTTPS access to ECR, Logs, Secrets Manager, Accounts and DM. A customer-managed encryption key on the secret requires the corresponding decrypt permission on the execution role.

The template defaults to zero tasks. Register `https://dm.teamofsilicons.com/auth/callback` with Accounts, start one task during the approved cutover, wait for healthy targets, then route the DM website hostname to this ALB. Its HTTP listener must already redirect to HTTPS. Keep the Vercel deployment and retired gateway stack/state for rollback; do not run `gateway-bootstrap.sh` with this new image. No template here deletes their retained data.

One replica is deliberate: session refresh rotation uses a short in-process replay cache. The replacement deployment stops the old task before starting the new one; expect a brief website interruption. Scaling horizontally requires shared refresh coordination. Changing the session secret invalidates browser sessions.

Validate the template with CloudFormation before creating the change set, review all resource additions, then check hosted sign-in, two-account messaging, draft reload, invitation acceptance, refresh and sign-out on the deployed hostname. The local verification report records build and browser evidence; CloudFormation/Docker execution and public-domain verification are separate release gates.
