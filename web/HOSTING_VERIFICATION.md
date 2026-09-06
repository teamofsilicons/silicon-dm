# Hosted frontend verification — 6 September 2026

This record covers deployment checks performed individually against the hosted
frontend and gateway. It supplements `MANUAL_VERIFICATION.md`, which records the
larger local frontend exercise against live DM/IAM. No automated browser scenario
suite is used.

## Deployment inventory

- Frontend: `https://dm.teamofsilicons.com`, Vercel project
  `silicon-dm-frontend` in `saketdev12-5675s-projects`.
- Vercel deployment:
  `https://silicon-dm-frontend-pworx9oyh-saketdev12-5675s-projects.vercel.app`.
- Gateway: `https://gateway.dm.teamofsilicons.com`.
- AWS stack: `silicon-dm-web-production`, account `234951665042`, `us-east-1`.
- Private gateway instance: `i-06b6670e5b2c53a3d`.
- Encrypted retained state volume: `vol-0fb520477e7c45cf6`.
- Gateway image: `234951665042.dkr.ecr.us-east-1.amazonaws.com/silicon-dm-production@sha256:518e8ba38c2de3e384b37ad77556643472d249d0222ce95a54e533ef69d56905`.
- Existing backend remains at `https://backend.dm.teamofsilicons.com`.

The Vercel artifact contains static public assets only. Its configured CSP permits
HTTPS/WSS to the exact gateway host. The public build environment variable is
stored on the Vercel production project. The gateway contains no copied local
test sessions; authentication will create fresh hosted browser sessions.

## Observed checks

- TypeScript check and production builds passed.
- Gateway container started as UID 10001 with read-only root and dropped
  capabilities; its local `/healthz` returned 200 and config returned the intended
  backend and origins with exact credentialed CORS headers.
- AWS validated the rendered template. The reviewed change set contained only
  additions for the gateway and ALB routing, with no backend/database replacements.
- ACM issued the gateway certificate after adding its DNS validation CNAME.
- The frontend custom domain returned HTTPS 200, the correct HTML, no-store
  caching, and the configured gateway CSP.
- Backend `/ready` remained HTTP 204 during gateway provisioning.

## Hosted gateway results

- Stack reached `UPDATE_COMPLETE`; the instance and state volume were preserved
  during a bootstrap recovery. The initial full-device zero check rejected an
  unformatted encrypted EBS volume. It was replaced with exact volume/instance
  identity, stack ownership, snapshot absence, filesystem-signature, and creation
  age checks. The corrected bootstrap completed with a real successful health
  check. No production application data existed on that new volume.
- ALB gateway target reached `healthy`; public TLS validation and `/healthz`
  returned 200. HTTP redirects to the gateway's own HTTPS hostname.
- The frontend's credentialed CORS preflight returned 204. An unrelated Origin
  returned 403. Public config contained the expected exact origins and backend.
- A fresh production SLT obtained with the installed IAM CLI established a hosted
  gateway session for the existing owner. No tokens were printed or placed into
  frontend assets.
- Restarted only `silicon-dm-gateway` through SSM. The service returned active,
  the EBS filesystem remained mounted with directory mode 0700, and the same
  cookie still loaded the authenticated owner profile after restart.
- An individually opened authenticated WSS connection through the ALB received
  `ready`, then closed. No automated scenario suite was run.
- `/auth/login` returned 303 to the IAM login page and set an opaque `__Host-`
  cookie with Secure, HttpOnly, and SameSite=Lax flags. The complete browser
  callback has not yet been exercised on the hosted domains.
- The stack policy rejects replacement/deletion of the instance, state volume,
  and attachment. Stack/instance termination protection remains enabled.
- Logged out the temporary API-verification owner session; the response showed
  no authenticated profiles. The private local sign-in helper was stopped.

## Remaining browser check: workstation DNS

Both Namecheap authoritative nameservers and public resolver 1.1.1.1 return the
correct gateway CNAME. The workstation's local Unbound resolver instead retains
an earlier NXDOMAIN response with about 50 minutes remaining at diagnosis. The
browser therefore cannot reach the gateway yet. The user was asked to reload the
verified root-owned Unbound process; sudo requires their password. Neither DNS
configuration nor certificate verification was bypassed in the browser.

Server-side HTTPS checks used curl's `--connect-to` with the existing ALB hostname
while retaining the gateway Host/SNI and normal TLS certificate validation.
This distinguishes validated hosting from completion of the browser sign-in,
callback, and live messaging checks, which remain pending DNS cache clearance.
