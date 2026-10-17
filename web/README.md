# Silicon DM website

The chat app Carbons use: conversations at `/c`, a conversation at `/c/<address>`, the settings at `/settings`.
Next.js 16, React 19 and Silicon UI. `components/chat/README.md` is the map of the chat (routes, frame, data, keyboard,
each area, testing); `DESIGN.md` is the look.

The website's own server stands between the browser and the DM service. Silicon Accounts tokens stay in a sealed,
HttpOnly cookie, so no script on a page can read them; sign-in uses PKCE and a state cookie, token claims are checked
against Silicon Accounts' JWKS, and every write must come from this site's own pages.

| Route | What |
| --- | --- |
| `/auth/sign-in`, `/auth/callback`, `/auth/refresh`, `/auth/session`, `/auth/sign-out` | The sign-in at Silicon Accounts and the session cookie |
| `/api/<service path>` | The DM service, called with the session's token (`app/api/[...path]/route.ts`); Range, ETag and If-None-Match pass through for speech audio |
| `POST /api/uploads?name=` | Saves a file or voice note into the Carbon's own Silicon Briefcase (`apps/dm/`) as DM acting for them, and answers its link |
| `GET /api/uploads/config` | Whether uploads are on here, why not, and the largest file |
| `GET`/`HEAD /api/uploads/file?url=` | The only way a browser loads Briefcase's bytes: the configured Briefcase's public files only, inline only for pictures, audio, video and PDF (everything else is a download), always nosniff and a sandbox CSP |
| `/llms.txt`, `/llms-full.txt`, `/robots.txt`, `/sitemap.xml` | For Silicons and crawlers (`lib/agent/files.ts`) |

Every page gets a fresh CSP nonce (`proxy.ts`): scripts from this site only, `connect-src 'self'`, no framing, and the
microphone allowed for this site alone. The website records no telemetry, and tells the DM service so on every call.

## Run locally

Use Node 24 or later and pnpm 10.33.0. The local chat stack (`../scripts/chat-stack/up.sh`: Silicon Accounts, the DM
service on 4521, stand-ins for Deepgram, Briefcase and Giphy, and a pool of Carbons and Silicons) writes `.env.local`
for you; otherwise copy `.env.example` to `.env.local`, set the app secret and a random session secret, and register
`{PUBLIC_URL}/auth/callback` on the local Accounts app `dm`.

```sh
pnpm install --frozen-lockfile
PORT=4520 pnpm dev
```

Sign in through the hosted pages, or put the stack's session cookie in a browser without email codes
(`node ../scripts/chat-stack/identities.mjs cookie ada`; it works on any 127.0.0.1 port). To run beside someone else's
server, give yours a port and a build directory of its own: `PORT=4702 NEXT_DIST_DIR=.next-mine
PUBLIC_URL=http://127.0.0.1:4702 pnpm dev`.

## Environment

Read on the server at request time (a missing or malformed value stops the server with an exact message,
`lib/server/env.ts`); never put a secret in a `NEXT_PUBLIC_` variable. `.env.example` has each with a comment.

| Variable | What |
| --- | --- |
| `APP_ID` | This app at Silicon Accounts: `dm` |
| `APP_SECRET` | Its app secret (server only) |
| `ACCOUNTS_URL`, `ACCOUNTS_API_URL` | Silicon Accounts' public origin (the hosted sign-in and the token issuer), and where this server reaches its API (defaults to the first) |
| `APP_API_URL` | The DM service |
| `SESSION_SECRET` | At least 32 random bytes; seals the session cookie. Changing it signs every browser out |
| `PUBLIC_URL` | This site's origin; `{PUBLIC_URL}/auth/callback` is registered at Silicon Accounts |
| `EXTRA_IMG_ORIGINS`, `EXTRA_ORIGINS` | More profile-photo origins; another explicitly trusted origin of this site |
| `BRIEFCASE_API_URL` | Silicon Briefcase's API origin, where this website saves a Carbon's files and voice notes (https; plain http only for a stand-in on this machine). Unset: uploads are off, the + menu and the microphone say so, and links, GIFs and dictation still work |
| `UPLOADS_MAX_BYTES` | The largest file this website takes, in bytes (default 209715200, 200 MiB); a larger one is refused before any byte is sent |

Production uses https and Secure cookies. Uploads in production also need Briefcase to accept DM's proofs for its
upload scopes.

## Verification

```sh
pnpm typecheck
pnpm lint
pnpm test
pnpm build
pnpm test:e2e                                                     # the local chat stack; builds and serves on 4530
E2E_PORT=4712 E2E_DIST_DIR=.next-e2e-mine pnpm exec playwright test --project=e2e   # a run of your own, beside others
E2E_PORT=4520 pnpm test:e2e                                       # against a running `pnpm dev`
```

The browser tests use the real Accounts sign-in and the DM service, as two Carbons from the stack's identity pool (no
email codes): live delivery and receipts, replies, edits, drafts, deleting, groups and invitations, uploads through the
Briefcase stand-in, voice through the Deepgram stand-in and a fake microphone, the shortcuts and the palette,
conversations that start elsewhere, reports, sign-in and refresh, responsive layouts, and axe WCAG 2.2 AA. Screenshots
go to the ignored `screens/`; credentials and browser state stay under the ignored `test-results/`. The chat README's
Testing section has the rules for running beside others on one stack.

## Deployment

Build `Dockerfile.gateway` with the web directory as its context. It produces the Next standalone server, listening
on port 4315 (the artifact keeps the gateway name for release compatibility). `deploy/aws/web.yaml` deploys the
website to the existing ALB and ECS cluster at the public DM hostname.

Register the production `PUBLIC_URL/auth/callback` before switching traffic. Keep one website build and secret per
deployment. Sign-in redirects, refreshed sessions, writes, uploads and sign-out must be checked on the deployed origin;
deploying is a deliberate step a Carbon takes (`../docs/operations/deployment.md`), never part of these tests.
