# Silicon Dm website

Next.js 16, React 19 and the shared Silicon Arc UI. The product pages call the real Dm API through a same-origin server proxy. Silicon Accounts tokens stay in a sealed, HttpOnly cookie; the browser never receives an access or refresh token in JavaScript. Sign-in uses PKCE and state, token claims are checked against Accounts JWKS, and writes require a same-origin request.

## Run locally

Use Node 24 or later and pnpm 10.33.0. Copy `.env.example` to `.env.local`, set the app secret and a random session secret, and register `http://127.0.0.1:4120/auth/callback` on the local Accounts app `dm`. Start the real app backend on 4121 and the shared local Accounts stack first.

```sh
pnpm install --frozen-lockfile
pnpm dev
```

## Environment

`APP_ID=dm`, `APP_SECRET`, `ACCOUNTS_URL`, optional `ACCOUNTS_API_URL`, `APP_API_URL`, `SESSION_SECRET` (at least 32 random bytes), and `PUBLIC_URL` are read on the server at request time. Never put these secrets in a `NEXT_PUBLIC_` variable. `EXTRA_IMG_ORIGINS` extends trusted profile image origins; `EXTRA_ORIGINS` is only for another explicitly trusted website origin. Production uses HTTPS and Secure cookies.

## Verification

```sh
pnpm typecheck
pnpm lint
pnpm test
pnpm build
TEST_STACK_JSON=/private/path/test-stack.json pnpm test:e2e
```

Browser tests use the real Accounts hosted sign-in and app API, including the product mutations, refresh, logout, responsive layouts and axe WCAG 2.2 AA checks. `scripts/e2e-accounts.sh` in the repository rehearses backend and CLI behavior. Extend browser product tests also need the `fake_device` native protocol example built under `target/mig`. Screenshots are written to ignored `screens/`; credentials and browser state stay under ignored test outputs. See `docs/migration/web-verification.md` for recorded results.

## Deployment

Build `Dockerfile.gateway` with the web directory as its context. It now produces the Next standalone server, listening on port 4315. The artifact keeps the gateway name for release compatibility. `deploy/aws/web.yaml` deploys this website to the existing ALB and ECS cluster at the public DM hostname. The retired persistent gateway stack is retained for rollback and is not reused by this stateless app.

Register the production `PUBLIC_URL/auth/callback` before switching traffic. Keep one website build and secret for a deployment; changing SESSION_SECRET signs browsers out. Sign-in redirects, refreshed sessions, actual product writes and sign-out must be checked on the deployed origin. Production release is coordinated by the migration cutover runbook; these local tests do not deploy it.
