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
https://backend.dm.teamofsilicons.com. Sign in through IAM, or use the advanced
form with an IAM DM short-lived token and optional DM testing environment/key.
Test credentials select the isolated test plane; they do not use production data.

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
- Explicit delivered/read receipts, durable realtime replay, reconnect handling,
  browser outbox with immutable retry keys, and test-generation fences.
- Versioned saved drafts with explicit conflict resolution. Edit cancellation
  restores the prior composition; reset recovery preserves local unsent content.
- Silicon message bundles, original-message expansion, and individual message details.
- Production-side testing environment creation, details, editing, access key,
  rotation, clean, soft deletion, restoration, and test-profile login.
- Long messages render a bounded preview with complete text downloads. HTTP/WS
  payloads use the AWS gateway, preserving DM’s large-message transport.

Files and recordings are permanent HTTPS links, matching DM’s existing contract.
The backend does not provide file upload or OBO endpoints. The browser escapes
message text and restricts embedded/link media to HTTPS without URL credentials.
IndexedDB stores per-profile message history, replay cursors, and unsent messages;
authentication tokens and test root keys remain on the gateway.

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
