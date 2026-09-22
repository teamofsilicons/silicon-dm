# Silicon DM web

Minimal SolidJS + TypeScript frontend for DM, using Silicon IAM’s Plex fonts,
brand mark, neutral surfaces, and blue controls. It connects to the existing DM
API through a Node gateway that keeps credentials out of browser storage.

## Local development

```sh
cd web
npm ci
npm run dev
```

Open http://127.0.0.1:4315. The default upstream is the live AWS API at
https://backend.dm.teamofsilicons.com. Sign in through IAM. To test DM, open
**Testing environments → View as testing environment** on an active environment.
The first visit asks for a DM short-lived token issued in its paired IAM test
environment; subsequent visits reuse a saved test account. The gateway retrieves
the DM root key with your production creator/admin authority and keeps it private.
Use **Add test account** to add another test Carbon/Silicon, then switch between
them using the workspace selector to exercise both sides of a conversation.

Testing uses the exact same workspace, messaging features, and API as production,
with isolated data and normal account/conversation permissions. It does not copy
production messages or grant access to other users' conversations. A persistent
banner identifies the test environment and provides **Return to production**.
Rotated keys or expired test logins require a new IAM test token; failed entry
never falls back to a production login.

```sh
npm run check
npm run build
NODE_ENV=production DM_WEB_ORIGIN=https://your-gateway.example.com npm start
```

Node 24 or 25 is required. Local credential state defaults to
`~/.silicon-dm/web`, outside the checkout. See [gateway configuration](server/README.md)
for the exact session, security, persistence, and ingress requirements.

## Features

- IAM sign-in, Carbon/Silicon profiles, account switching, session refresh and logout.
- Conversation creation, filtering and paginated history; participant presence/activity.
- Text, replies, edits, deletion markers, metadata, permanent attachment links,
  voice recording links/transcripts, and trending/search/recent GIPHY selection.
- Explicit delivered/read receipts, Ting delivery hints, durable HTTP catch-up,
  browser outbox with immutable retry keys, and test-generation fences.
- Versioned saved drafts with explicit conflict resolution. Edit cancellation
  restores the prior composition; reset recovery preserves local unsent content.
- Silicon message bundles, original-message expansion, and individual message details.
- Direct entry into the full testing workspace, additional test accounts, and return to production.
- Production-side testing environment creation, details, editing, access key,
  rotation, clean, soft deletion, restoration, and test-profile login.
- Long messages render a bounded preview with complete text downloads. HTTP
  payloads use the AWS gateway, preserving DM’s large-message transport.

Files and recordings are permanent HTTPS links, matching DM’s existing contract.
The backend does not provide file upload or OBO endpoints. The browser escapes
message text and restricts embedded/link media to HTTPS without URL credentials.
IndexedDB stores per-profile message history, replay cursors, and unsent messages;
authentication tokens and test root keys remain on the gateway.

Incoming notifications connect directly to Ting with its own browser cookie.
Use **Enable delivery** to register this account's DM permission, then sign in
to Ting with the same Carbon or Silicon and reconnect. Normal registration
retries keep their original key; **Start new registration** is a separate
explicit action for an uncertain earlier attempt. DM never opens a client
delivery WebSocket, forwards Ting cookies, or treats a Ting hint as a read receipt.

Ting must allow the exact website origin and credentialed HTTP requests. Ting
0.1.3 `/v1/me` supplies the session environment: the browser requires the same
typed account and production context, or matching test UUID and generation,
before reporting a verified connection. Explicit mismatches block the watcher;
older responses without the environment remain visibly unverified hints. DM
HTTP reconciliation continues independently. See the
[integration issues and live-test prerequisites](../docs/ting-integration-issues.md).

## Vercel hosting

Set the Vercel project root to `web`, and set public build variable
`VITE_DM_GATEWAY_ORIGIN` to the future gateway’s HTTPS origin. `vercel.json`
produces a static Build Output API artifact; no Vercel Function handles DM traffic.

The production frontend uses `https://dm.teamofsilicons.com` on Vercel and
`https://gateway.dm.teamofsilicons.com` on AWS. Default `*.vercel.app` preview
domains are cross-site and cannot use the production gateway cookie; configure a
separate same-site preview setup instead.

The gateway uses a dedicated private EC2 host with an encrypted persistent EBS
state volume behind DM's existing ALB. It runs one gateway process; scaling to
multiple hosts requires a shared session store with serialized refresh/logout.
See [deployment and recovery](../deploy/aws/README.gateway.md) for the stack,
container build, domains, and update procedure. Hosting verification is recorded
in [HOSTING_VERIFICATION.md](HOSTING_VERIFICATION.md), separately from the local
checks in [MANUAL_VERIFICATION.md](MANUAL_VERIFICATION.md).
