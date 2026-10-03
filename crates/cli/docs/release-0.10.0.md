# DM 0.10 delivery migration

This is a breaking transport migration. Existing message content and HTTP
contract 3 remain unchanged; incoming delivery moves to Ting.

## Release status: 2026-09-23

DM backend 0.10.1 and gateway 0.10.0 are deployed, and the DM and Interface
websites use Ting for incoming delivery. Backend 0.10.1 separates the shared
testing generation from its private cache revision and keeps queued handoffs
retryable during temporary IAM failures. Deployed bidirectional Carbon/Silicon
delivery passed with real IAM and Ting, including exact hydration, idempotent
sends, explicit receipts, HTTP sync and presence. See
[the deployed evidence](ting-deployed-live-verification.json) for the exact scope.
Client/CLI 0.10.1 resolves canonical Ting organization IDs and is published on
crates.io, GitHub and Honeycomb. A fresh catalog installation and the existing
global DM installation both verify 0.10.1 with matching artifact hashes; existing
profiles, logical queue/cursor state and the running relay were preserved.
The published CLI passed 17 native checks, including real Carbon/Silicon login,
HTTP hydration, a 503-to-204 callback retry with one durable consumer effect,
stable hook reconnect, and separate delivered/read receipts. See
[the native release evidence](ting-native-release-verification.json). Task-only
profiles and callbacks were cleaned up; the shared Ting daemon and existing
bindings were retained. The running Ting daemon is 0.1.2, distinct from the
installed Ting CLI 0.1.4; this release did not restart that daemon.
Protocol crate 0.10.0 and HTTP message schema 3 remain unchanged.

IAM 3.0.3 and Honeycomb 0.3.3 are deployed. Both a fresh rotated environment
and the original stuck rotation now pass official IAM SDK audience-credential
validation. The original operation recovered a definitive rejection through
the supported API, followed by successful reconciliation and a new rotation.

Production DM configuration revision 2 includes both required Ting scopes, and
`tos>dm.sync.changed` is registered in `tos`.
Interface retains its existing login/history and confirms a real Ting connection
in Bricks. See the [integration record](ting-integration-issues.md) for verification
details.

## What changes

DM’s `/api/v1/ws` and `/api/v1/ws/shared` return HTTP 410 with
`delivery_moved_to_ting`. Retained SDK socket methods return migration guidance
without connecting. Commands, history, receipts and device presence use HTTP.
The local DM relay retains durable outgoing commands; it preserves legacy inbox
and cursor records without forwarding or silently migrating them.

Ting receives immutable `tos>dm.sync.changed` references, owns incoming sockets,
queues, retries and local destinations, and sends raw `{"tings":[...]}` batches.
A generic destination must route all eligible apps and durably accept the entire
batch before HTTP 204. Consumers hydrate message content through authorized DM
HTTP and reconcile using opaque HTTP sync cursors and canonical snapshots.
Ting acceptance, callback acceptance and Ting read state never automatically set
DM Delivered or Read receipts. Those remain explicit DM operations.

The backend publishes using the initiating logged-in DM account’s verified
access token. Its bounded encrypted cache stores no refresh tokens or OBO proofs.
Retries retain the original body and idempotency key while obtaining a fresh
proof. Expired or revoked authority leaves the handoff pending until that same
account supplies fresh authority through login, refresh or an authenticated
request; another account cannot release it. Delivery outages do not mark the DM
message failed.

See [wire format](wire-format.md), [consumer migration](client/realtime.md),
[CLI authentication and destinations](cli/README.md), and
[deployment](deployment.md) for the current contracts.

## Rollout order and prerequisites

1. Approve DM’s external `subscriptions.register` and `tings.send` Ting scopes
   through Honeycomb/IAM. Publisher sessions need `self.identity.read` and the
   granted `obo:tos>ting:tings.send`; recipient enrollment needs its explicit
   consent. Configuration approval is separate from each account’s grant.
2. Register `tos>dm.sync.changed` in Ting for each intended organization and
   environment. Configure Ting’s exact DM/Interface browser origins, credentialed
   CORS and cookie behavior. Verify fresh test credentials after rotation and
   shared lifecycle readiness before using a sandbox as release evidence.
3. Back up the databases and stop both old API and worker tasks before applying
   migrations through 0035 and the runtime grants. Start the new API
   and worker together; this cutover must not overlap old and new delivery workers. Configure the backend Ting origin and timeout. Coordinate
   consumer, SDK, CLI and website upgrades with backend cutover: old DM socket
   consumers cannot receive after retirement. Rust publication order is protocol,
   client, then CLI; Honeycomb owns installed CLI distribution.
4. Explicitly enroll each recipient with `delivery register`. Sign in separately
   to Ting for the same typed account, organization and production/test context.
   Native consumers attach directly to Ting with `webhook URL --all-apps`, retain
   stable hook IDs and recover through HTTP sync. Browsers use Ting’s own cookie
   and inbox watch plus authorized DM HTTP reconciliation. Login/status never
   silently enroll a recipient.
5. Record publication, deployment, readiness and authorized live send/recovery
   verification separately. The status above distinguishes completed deployments
   from the pending patch acceptance and distribution checks; passing sandbox
   tests alone does not establish message delivery in every production organization.

## Configuration publication

DM’s existing configuration publication completed through Honeycomb on 2026-09-23.
Both external Ting scopes, `subscriptions.register` and `tings.send`, are effective
at DM configuration revision 2 and IAM revision 23. The existing publication
request was reused; no duplicate configuration request was created.

- Publication request: `e13529a1-ee3d-451a-82e8-4391e74408ec` (`published`).
- Honeycomb decision operation: `33438fd9-6df5-43fd-ab21-5bf03fd89d8d`.
- Accepted activation: `a4debe2e-74e1-4e67-9c7c-010b38885d32`.

This establishes application configuration approval. Per-account consent and
recipient enrollment remain explicit. Production type registration is verified
in `tos`.

## Historical 0.10.0 candidate validation

The isolated 0.10.0 release worktree passes the full Rust CI test command:
138 tests across 27 targets, with none ignored. The suite covers retired-route
responses, actor-bound HTTP sync, explicit receipts, Ting handoff/worker recovery,
credential isolation, group authorization, bundle atomicity and native delivery
bindings. Delivery database tests provision disposable PostgreSQL when no explicit
native fixture is configured. Workspace checks, formatting, Clippy with warnings
denied, and dependency policy checks pass. All three publishable crates pass
`cargo package` with all features, including compiling their packaged contents
and resolving the candidate protocol/client dependencies through Cargo’s temporary
package registry.

The 0.10.0 web application passes TypeScript checks, all 83 tests and its production
build. The docs build verifies 40 pages with their local links and navigation.
OpenAPI lint passes with eight warnings, including the deliberately 410-only
retired delivery endpoints. These local checks do not change the live-release
gates below.

## Evidence before the version bump

The recorded implementation checks passed before the 0.10.0 version change:
27 native client unit tests plus three HTTP fixtures, 12 CLI tests, 83 DM web
tests plus 29 local Chrome checks, and 359 Interface tests. Both website builds
passed. Backend evidence includes local PostgreSQL handoff, worker, sync,
presence, credential-cache and publisher recovery fixtures, transport fixtures
and Clippy. The versioned release suite and packaging checks are recorded separately above;
these earlier counts do not describe the final 0.10.0 artifacts.

The local candidate backend also passed **21 real-service checks** with deployed
IAM and Ting 0.1.3 in the task-owned original sandbox: bidirectional Carbon/Silicon
delivery, exact authorized hydration, idempotent sends, originator revocation and
same-account recovery across a backend restart, actor-bound cursors, HTTP presence
and explicit receipts. Real cookie/CORS/WebSocket network probes are distinct from
the local Chrome UI fixture. See [backend evidence](ting-backend-live-verification.json)
and [browser fixture evidence](ting-browser-verification.json).

The actual pre-bump CLI identified itself as 0.9.6. It passed separate DM/Ting
login, direct destination attachment, send/hydration, HTTP 503 callback replay
followed by 204, stable hook reconnect and explicit receipts. A local replay of an
authentic batch produced no duplicate consumer effects; this is not an exactly-once
transport claim. The installed Ting CLI was 0.1.3, while the already-running shared
daemon remained 0.1.2. A full shared-daemon restart was not tested. See
[native evidence](ting-native-live-verification.json). Only task-owned profiles,
processes and the task hook were cleaned up; other Ting bindings were preserved.

## Upstream fixes and remaining delivery checks

The stale rotated Ting audience credential and original stuck Honeycomb rotation
are fixed and verified on deployed IAM 3.0.3 and Honeycomb 0.3.3. No lifecycle
fence or saved request was bypassed. See [IAM issue evidence](iam-ting-e2e-issues.md)
and [Honeycomb issue evidence](honeycomb-ting-e2e-issues.md).

The [integration record](ting-integration-issues.md) retains historical Ting
findings and current verification. Backend 0.10.1 deployment and bidirectional
acceptance are complete in the task-owned testing environment. Client and CLI
publication, fresh installation, global update verification and native delivery
checks are complete. The immutable v0.10.1 package includes preparation-time
release status; this live record and the GitHub verification assets capture the
final deployed result.
