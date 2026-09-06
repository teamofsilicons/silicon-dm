# DM browser gateway

This Node 24 gateway holds IAM-issued DM access/refresh tokens and testing root
keys in private server-side session files. The browser receives an opaque,
HttpOnly, SameSite=Lax cookie. HTTPS uses a `__Host-` cookie with Secure and no
Domain attribute. Each browser can retain up to sixteen actor profiles; token
values and testing keys are absent from public session responses.

Run `npm run dev` for the Vite frontend and gateway on port 4315. The Node bundle
uses `npm run build`, then `NODE_ENV=production npm start`. It can serve the built
frontend locally and streams authenticated HTTP and WebSocket traffic.

The hosted frontend is static SolidJS on Vercel, with this persistent
Node gateway behind the API load balancer. The separate deployment is defined in
[the gateway stack](../../deploy/aws/README.gateway.md). REST requests go directly from the frontend to the
gateway using `credentials: "include"`; WebSockets connect directly to the same
gateway host. Access tokens, refresh tokens, and testing root keys stay on the
gateway. Vercel receives static frontend requests only, so its Functions request
body limit does not restrict DM's large-message transport.

| Variable                 | Default / purpose                                                                                                                                                |
| ------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `DM_WEB_ORIGIN`          | `http://127.0.0.1:4315` in development; required in production. Exact external HTTPS origin, or HTTP loopback.                                                   |
| `DM_FRONTEND_ORIGIN`     | Defaults to `DM_WEB_ORIGIN`; exact public frontend origin allowed by CORS and WebSocket Origin checks.                                                           |
| `VITE_DM_GATEWAY_ORIGIN` | Public frontend build setting; defaults to its own origin locally. Set to the separate gateway HTTPS origin for Vercel. Never put credentials in VITE variables. |
| `DM_API_ORIGIN`          | `https://backend.dm.teamofsilicons.com`; exact DM backend origin.                                                                                                |
| `IAM_LOGIN_ORIGIN`       | `https://auth.iam.teamofsilicons.com`; IAM browser authentication origin.                                                                                        |
| `DM_WEB_APP_ID`          | `tos>dm`; canonical IAM application ID.                                                                                                                          |
| `DM_WEB_DEFAULT_ORG`     | App owner's organization, `tos`; browser sign-in also accepts a validated `org_id` query parameter.                                                              |
| `DM_WEB_STATE_DIR`       | `~/.silicon-dm/web`; absolute private directory outside the application checkout and assets.                                                                     |
| `DM_WEB_MAX_BODY_BYTES`  | 134217728 bytes (128 MiB); configurable up to 3 GiB. This also bounds a proxied WebSocket message. Authentication JSON is separately limited to 16 KiB.          |
| `HOST`, `PORT`           | `127.0.0.1`, `4315`; use a suitable bind address behind the hosting ingress.                                                                                     |
| `ASSET_DIR`              | `dist/client`; production asset directory.                                                                                                                       |

Use persistent local storage and one gateway process per state directory. The
gateway enforces that ownership at startup; this file-backed store is not a
shared multi-replica session database. Directories are mode 0700, credential
files and the persisted idempotency HMAC key are mode 0600, and writes use fsync
and atomic replacement. These files contain credentials and belong in private
backups, never frontend bundles. Sessions are bound to the configured frontend,
backend, and app ID. Browser-session retention is thirty days, subject to IAM
revocation and refresh expiry. Refresh, login, and logout serialize per browser;
refresh retries reuse an HMAC-derived idempotency key for the same token.

`GET /auth/login` creates a ten-minute, one-use state bound to the browser and
selected organization, then redirects to IAM. IAM returns its SLT to
`/auth/callback?state=...`; the gateway exchanges it through DM and redirects to
the frontend. IAM's current redirect contract accepts canonical HTTPS URLs or
HTTP loopback URLs; a callback registration is not required. Hosting therefore
needs the exact external origin, TLS, ingress routing to this gateway, and a
working DM application registration. No app secret is needed by this gateway:
the DM backend already holds it. Do not log callback query strings at ingress.
Silicon sessions and testing profiles can use the explicit SLT login form;
testing credentials are submitted once to the gateway and retained privately.

`/api/dm/*` exposes only the existing DM product routes and testing management
routes. The gateway supplies authorization, organization, and testing headers;
browser-supplied authorization is never forwarded. Testing management requires
a selected production profile. `X-DM-Profile` selects a browser profile for each
HTTP call. `X-Testing-Environment-Generation` preserves the browser's mutation
fence. Requests and responses stream with backpressure; an already-consumed
mutation body is not replayed automatically after an upstream 401. Its access
token is marked expired, so the browser can refresh and retry with the original
idempotency key.

`/api/ws` authenticates the same cookie, accepts `profile_id`, `device_id`, and
optional `testing_generation`, and supplies the selected actor and organization
to DM. Optional `actors` and `org_id` must match that profile. Resume cursors are
sent in protocol frames after `ready`. Socket buffers are bounded; logout or
replacement login closes the affected profile's sockets. Browser writes and
WebSocket upgrades require the exact configured Origin. REST and WebSocket
credentials are never exposed in browser URLs or logged by the gateway.

## Static Vercel hosting

Set the Vercel project root to `web` and its public build environment variable
`VITE_DM_GATEWAY_ORIGIN` to the separately hosted gateway's exact HTTPS origin.
`web/vercel.json` builds `.vercel/output/static` using the Build Output API;
`scripts/build-vercel.mjs` emits static routes and a CSP restricted to the
configured gateway for HTTP and WebSocket connections. It creates no Functions
and copies only `dist/client`. `.vercel` and local environment files are ignored.

Use same-site custom HTTPS hosts, for example `dm.teamofsilicons.com` for the
frontend and `gateway.dm.teamofsilicons.com` for the gateway. These names describe
the production routing. A default `*.vercel.app`
frontend is cross-site to the gateway; it cannot use this SameSite=Lax session
cookie setup. Vercel preview deployments need an explicitly configured same-site
preview domain and a separate gateway/session directory, or use local development.
Origins are single exact values; there is no wildcard credentialed CORS.

The gateway host needs TLS, a load-balancer route preserving its Host
header, WebSocket upgrades, suitable request/idle timeouts, and persistent
writable storage for `DM_WEB_STATE_DIR`. Set `DM_WEB_ORIGIN` to the gateway host,
`DM_FRONTEND_ORIGIN` to the Vercel custom frontend host, and `HOST=0.0.0.0` behind
the ingress. `/healthz` remains an unauthenticated readiness probe. Run one
gateway process on one persistent host; ephemeral Fargate storage loses browser
sessions when a task is replaced, and the local PID lock does not make a shared
filesystem safe across hosts. A future multi-replica deployment needs an external
session store with serialized refresh and logout; none is silently substituted.

The direct Node gateway preserves its configurable encoded-body cap: 128 MiB by
default, up to 3 GiB. DM's decoded text/transcript limits are separate. A 100M
ASCII message fits the default transport cap; worst-case encoded Unicode may
require matching higher caps and adequate memory in both gateway and backend.
Large payloads never traverse a Vercel Function's
[4.5 MB request limit](https://vercel.com/kb/guide/how-to-bypass-vercel-body-size-limit-serverless-functions).
The persistent gateway also avoids depending on Vercel's beta WebSocket runtime.

For a local split-origin check, build the Node bundle, run the gateway with
`DM_WEB_ORIGIN=http://127.0.0.1:4316`,
`DM_FRONTEND_ORIGIN=http://127.0.0.1:4315`, `PORT=4316`, and a separate private
`DM_WEB_STATE_DIR`. Run Vite with
`VITE_DM_GATEWAY_ORIGIN=http://127.0.0.1:4316 npm run dev`. When this public setting
is present, Vite serves only the frontend and does not start another embedded
gateway. Use the same hostname on both ports; `localhost` and `127.0.0.1` are
different browser sites.

Manual gateway verification on the local split: credentialed frontend preflight
returned 204, config returned 200 with an absolute gateway IAM login URL, invalid
login input returned 400 with readable CORS headers, foreign-origin preflight
returned 403, a trusted-origin WebSocket without a session returned 401, and an
untrusted-origin WebSocket returned 403. These checks made no IAM or backend
mutations; successful live sign-in and conversation checks are recorded separately.
