# IAM organization consent rollout — 8 September 2026

DM now follows IAM's `docs/ORGANIZATION_CONSENT.md` and client 1.4.0 contract.
Browser sign-in sends only `app_id` and `redirect_uri`. IAM selects the organization
grants. DM resolves the returned unscoped token through official SDK authorization
introspection, exposes the selected organizations in the login/refresh response,
and continues to validate `X-Org-ID` against fresh IAM authorization per request.
The singular `organization_id` remains the initial organization for compatibility
with the existing CLI and Rust client. No OBO endpoints were added.

## Manual checks

These were individual live interactions, not an automated scenario suite.

- The production browser page showed one **Continue with IAM** link and no
  organization or advanced token form.
- Clicking it opened the verified `tos>dm` app in IAM. The URL contained only
  `app_id` and `redirect_uri`; the callback retained its random state.
- IAM displayed its organization picker. Selecting the existing `tos` grant and
  continuing returned to DM, loaded the conversation list and showed **Connected**.
- The browser's **Refresh session** action completed and retained the connected
  account. Browser sign-out returned to the IAM-only sign-in page.
- The installed IAM CLI minted a fresh SLT for the existing `tos` grant. A direct
  production backend exchange returned 200, `organization_ids: ["tos"]`, and the
  expected actor. `/auth/me` accepted `tos` (200) and rejected an unselected
  organization (401). Refresh preserved the grant (200); temporary-family logout
  returned 204.
- A separate live gateway login with an obsolete `org_id` query parameter ignored
  it. A fresh CLI-issued SLT completed the cookie/state-bound hosted callback
  (303). `/api/session` returned the authenticated organization without exposing
  access or refresh tokens. Replaying the callback returned 400. Explicit refresh,
  conversation-list read, and logout each returned 200; logout left no profile.

Only one organization was available on the test identity. Multiple-organization
profile creation and shared-family refresh/logout were reviewed in code but were
not exercised against multiple live grants. These checks cover this auth change,
not a new exhaustive run of every messaging, CLI, or SDK command.

## Deployment and recovery

- Frontend: `https://dm.teamofsilicons.com`; Vercel deployment
  `silicon-dm-frontend-non51kuor-saketdev12-5675s-projects.vercel.app`.
- Gateway image: `silicon-dm-production@sha256:0c4bb5cc1a97627f797452f439bf77d7cf8c3b8bc33df39313ae9f2fb3297a07`.
- Backend image: `silicon-dm-production@sha256:18dc3e3d20edc32640e0d3ca2219b17dbfefe9041dab48b0a9eb585ce08f92d4`.
- Image registry: `234951665042.dkr.ecr.us-east-1.amazonaws.com`.

Updating the gateway image parameter in EC2 user data triggered a stop/start.
AWS repeatedly reported insufficient `t4g.medium` capacity in `us-east-1a`, which
caused a temporary browser outage. Recovery changed the same instance to
`t4g.large`, then reconciled the CloudFormation template and image parameter.
The gateway stack reached `UPDATE_COMPLETE` and public health returned 200.
Instance `i-06b6670e5b2c53a3d` and retained encrypted state volume
`vol-0fb520477e7c45cf6` were preserved. The larger instance increases hosting cost.
This remains a single-host gateway; container updates should use SSM and any
CloudFormation user-data stop/start needs its own availability planning.

Rust compile and clippy checks, Rust formatting, TypeScript checking, production
frontend/gateway builds, ARM64 backend image build, and whitespace checks passed.
