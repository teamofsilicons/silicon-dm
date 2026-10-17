# Silicon DM website

The chat app Carbons use: conversations at `/c`, a conversation at `/c/<address>`, the settings at `/settings`. Next.js 16,
React 19 and Silicon UI. `components/chat/README.md` is the map of the chat (routes, frame, data flow, keyboard,
extension points). The product pages call the real DM API through a same-origin server proxy. Silicon Accounts tokens stay in a sealed, HttpOnly cookie; the browser never receives an access or refresh token in JavaScript. Sign-in uses PKCE and state, token claims are checked against Accounts JWKS, and writes require a same-origin request.

## Run locally

Use Node 24 or later and pnpm 10.33.0. The local chat stack (`../scripts/chat-stack/up.sh`: Silicon Accounts, the DM
service on 4521, stand-ins for Deepgram, Briefcase and Giphy, and a pool of Carbons and Silicons) writes `.env.local`
for you; otherwise copy `.env.example` to `.env.local`, set the app secret and a random session secret, and register
`{PUBLIC_URL}/auth/callback` on the local Accounts app `dm`.

```sh
pnpm install --frozen-lockfile
PORT=4520 pnpm dev
```

Sign in through the hosted pages, or use the stack's identity helper for a session cookie without email codes
(`node ../scripts/chat-stack/identities.mjs cookie ada`).

## Environment

`APP_ID=dm`, `APP_SECRET`, `ACCOUNTS_URL`, optional `ACCOUNTS_API_URL`, `APP_API_URL`, `SESSION_SECRET` (at least 32 random bytes), and `PUBLIC_URL` are read on the server at request time. Never put these secrets in a `NEXT_PUBLIC_` variable. `EXTRA_IMG_ORIGINS` extends trusted profile image origins; `EXTRA_ORIGINS` is only for another explicitly trusted website origin. Production uses HTTPS and Secure cookies.

## Verification

```sh
pnpm typecheck
pnpm lint
pnpm test
pnpm build
pnpm test:e2e            # against the local chat stack; builds and serves the site on 4530
E2E_PORT=4520 pnpm test:e2e   # against a running `pnpm dev`
```

Browser tests use the real Accounts hosted sign-in and the DM service: two Carbons talking (live delivery, receipts,
reply, edit, draft, delete, a group and its invitation, shortcuts, the palette, a report), refresh, sign-out, responsive
layouts and axe WCAG 2.2 AA checks. With the local chat stack the Carbons come from its identity pool (no email codes). `../scripts/e2e-accounts.sh` rehearses the service and the `dm` command. Screenshots are written to ignored `screens/`; credentials and browser state stay under ignored test outputs. See `docs/migration/web-verification.md` for recorded results.

## Deployment

Build `Dockerfile.gateway` with the web directory as its context. It now produces the Next standalone server, listening on port 4315. The artifact keeps the gateway name for release compatibility. `deploy/aws/web.yaml` deploys this website to the existing ALB and ECS cluster at the public DM hostname. The retired persistent gateway stack is retained for rollback and is not reused by this stateless app.

Register the production `PUBLIC_URL/auth/callback` before switching traffic. Keep one website build and secret for a deployment; changing SESSION_SECRET signs browsers out. Sign-in redirects, refreshed sessions, actual product writes and sign-out must be checked on the deployed origin. Production release is coordinated by the migration cutover runbook; these local tests do not deploy it.
