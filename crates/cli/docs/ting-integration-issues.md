# Ting integration: release status and historical diagnostics

Initial findings: 2026-09-22. Ting fixes rechecked on 2026-09-23 IST
(2026-09-22 after 20:09 UTC).
This record separates supported Ting behavior from changes still needed for
DM's delivery migration. The latest verified outcomes are recorded below;
the original failure evidence is retained for context.

## Release follow-up: 2026-09-23

Production DM configuration revision 2 (IAM revision 23) now includes both Ting
external scopes, and `tos>dm.sync.changed` is registered in production `tos`.
DM backend 0.10.1, gateway and website are deployed. The backend now separates
the shared sandbox generation from internal credential-cache invalidation, and
idle workers no longer poll IAM. Deployed bidirectional message/receipt acceptance
passed with real IAM and Ting in the task-owned testing environment; see
[the deployed evidence](ting-deployed-live-verification.json). Client/CLI 0.10.1
publication and installed-native verification are in progress.

The live Interface browser retained its production login and message history.
Both websites now resolve their selected organization through authenticated Ting
`/v1/orgs` before comparing watch acknowledgements and hints with its canonical
organization ID. Interface's real browser confirms the matching account and
workspace connection. Fresh real IAM SLTs also passed Carbon/Silicon Ting cookie,
CORS and WebSocket checks from both website origins in the exact test generation.
These checks do not by themselves claim message delivery or recipient enrollment.

Ting's production app list is empty for `bricks`, and listing/registering DM's
notification type there is denied, although the same app/type is visible in
`tos`. Read-only source review found that sends also look up the type in the
recipient organization. The inferred correction is to resolve publisher-owned
types independently of recipient organization while retaining existing type
management authorization and recipient grants. No Ting change has been made;
cross-organization delivery remains an outstanding dependency. A connected
browser is not proof that a DM message can be handed off in Bricks.

IAM 3.0.3 and Honeycomb 0.3.3 are deployed. The rotated fresh-environment credential
passes a new official-SDK OBO audience validation. The original pending Honeycomb
operation recovered its definitive configuration-revision rejection through the
supported API; reconciliation and a new rotation then succeeded. A fresh official
IAM SDK exchange returned the exact new Ting credential for that original
environment. See the two linked issue reports for the dated evidence.

## Historical check: Ting 0.1.3 fixes verified

At that earlier check, the live service reported 0.1.3. Both DM and Interface origins receive
credentialed CORS on successful and error responses; an unrelated origin still
returns 403. Authenticated test Carbon and Silicon sessions return the correct
`environment: {kind: "testing", id, generation: 1}`. Cookie-authenticated
WebSocket connections from both permitted origins successfully acknowledged
`watch_inbox`. These were real HTTP/WebSocket checks against Ting, separate from
the local browser fixture below.

The published contract and source revision
[`3be6ef37a8be561c774562b83b02a4d2769567f2`](https://github.com/teamofsilicons/silicon-ting/tree/3be6ef37a8be561c774562b83b02a4d2769567f2)
now define explicit production/testing session environments and reusable native
WebSocket APIs. DM and Interface validate the account and exact environment;
testing also requires the same positive generation. Missing legacy browser
metadata remains visibly unverified, while malformed or mismatched context is
rejected. The native DM client requires explicit matching context for login and
subsequent destination operations. The existing SDK suffices for these additive
session fields; no dependency upgrade was required.

The original DM lifecycle import also completed: operation
`bfefc652-12cb-42da-a579-7b0005bfebc2` is accepted and its environment has no
pending lifecycle operation. A later test Ting credential rotation exposed a
separate Honeycomb revision issue, recorded in
[honeycomb-ting-e2e-issues.md](honeycomb-ting-e2e-issues.md). A fresh task-owned
environment, `1d32b4c6-dc84-4c44-b7b7-a16be7a31d06`, accepted both app
configurations before credential rotation and avoided that setup failure.

In both task-owned environments, DM declares `subscriptions.register` and
`tings.send`, and Ting's `tos>dm.sync.changed` type is registered. Production
configuration revision 2 (IAM revision 23) was subsequently accepted, including
both Ting scopes; the production `tos>dm.sync.changed` type is registered in
`tos`. The historical candidate validation below includes 27 native client unit tests
plus three HTTP fixtures, 83 DM web tests and 29 local Chrome checks, and 359
Interface tests; both website builds pass. The candidate backend passed 21
real-service checks covering bidirectional
message delivery, allowed-origin cookie WebSockets, exact message hydration,
idempotent sends, revoked originator access-token recovery across a restart,
actor-bound sync cursors, presence and explicit receipts. See
[ting-backend-live-verification.json](ting-backend-live-verification.json).
The local candidate used deployed IAM and Ting in the original sandbox; DM
production was not deployed. The actual candidate CLI also passed its shared
Ting daemon/hook flow: both logins, direct destination attachment, message send,
503 replay followed by 204 acceptance, authorized hydration, retained hook ID
on reconnect, consumer deduplication and explicit delivered/read receipts. See
[ting-native-live-verification.json](ting-native-live-verification.json).
Only task-owned profiles and its hook were cleaned up; the existing shared Ting
daemon was left running. The installed Ting CLI is 0.1.3; that already-running
daemon still uses 0.1.2, and a full daemon restart was not tested.
The candidate backend, tunnel and task PostgreSQL
instance were stopped after verification.
The sections below preserve the original 0.1.2 observations and remaining operational constraints.

The historical failed run also exposed a separate IAM problem after rotating Ting test credentials: newly
issued OBO proofs supplied an old audience credential that IAM rejected.
The exact failure and successful unrotated-context comparison are documented in
[iam-ting-e2e-issues.md](iam-ting-e2e-issues.md). This failed run was not
counted as successful delivery, and no rejected credential was used as a fallback.

Evidence inspected initially:

- [Public API contract](https://ting.teamofsilicons.com/docs/api.md).
- [Live application information](https://backend.ting.teamofsilicons.com/v1/iam)
  names `tos>ting`, API `v1`, and Rust package `silicon-ting-client`.
- [Live health](https://backend.ting.teamofsilicons.com/healthz) reports `0.1.2`.
- [Published crate index](https://index.crates.io/si/li/silicon-ting-client)
  lists `0.1.2` as the latest non-yanked client, published 2026-09-22.
- [Published client archive](https://static.crates.io/crates/silicon-ting-client/silicon-ting-client-0.1.2.crate)
  records source revision `e8e90005d39e5601116846892707f21494b70739`.
- Source inspection below uses Ting repository revision
  [`1999c7b02762077da77153b23de4bf5c8d6f9498`](https://github.com/teamofsilicons/silicon-ting/tree/1999c7b02762077da77153b23de4bf5c8d6f9498).
  A health/version response alone does not establish that every inspected source
  path was exercised on the live service.

## 1. Selected authentication: the initiating account's DM session

The user selected the logged-in Carbon or Silicon's DM authority. Every pending
handoff retains the typed actor that caused it. Message sends retain the actual
authenticated initiator (even when representing another author); delivery/read
receipt events retain the receipt recipient. Unattributable autonomous legacy
events remain pending. The target or another logged-in account never supplies
substitute authority.

The backend encrypts verified, unexpired DM application access tokens in the
selected database plane. It retains no refresh tokens or OBO proofs. Existing
DM clients remain the exclusive owners of refresh-token rotation. Login,
refresh and ordinary authenticated requests capture verified access-token
authority; a new eligible token wakes pending work for that same account.

Before every handoff attempt, DM re-introspects the cached token, checks the
exact organization and originator, and uses the official IAM SDK to obtain a
fresh single-use `tings.send` proof over the unchanged persisted request bytes.
IAM app identity binds the `tos>dm.sync.changed` type to DM. Sandbox proofs also
require IAM-verified Ting audience credentials from the same test environment.

Expired or revoked authority leaves the event pending until that same account
returns with an eligible session. This is the chosen behavior, not independent
application-only publishing. Ting 0.1.2 still requires actor-bound OBO authority:
its [proof verifier](https://github.com/teamofsilicons/silicon-ting/blob/1999c7b02762077da77153b23de4bf5c8d6f9498/crates/ting-server/src/auth.rs#L862)
and [WebSocket send](https://github.com/teamofsilicons/silicon-ting/blob/1999c7b02762077da77153b23de4bf5c8d6f9498/crates/ting-server/src/ws.rs#L159)
do not accept application-only keys. A connected socket does not extend session
authority. No service account or alternative publishing identity is selected.

**Verified live recovery.** The 0.1.3 follow-up saved an immutable handoff, revoked
its originating DM access token and restarted the publisher. Both recipient
handoffs remained pending while a different account was active. Fresh authority
from the original actor released those same handoffs to Ting; the recipient
fetched the unchanged message. See the backend verification report above.

**Broader release gate.** Save a message and its handoff, interrupt the publisher,
revoke the initiating session, then restart. Verify it remains pending. Sign
that same actor back into DM with the required consent, then observe the real
Ting acceptance and recipient delivery. Also verify a different actor's login
does not release the pending event. Local fixtures cannot satisfy this gate.

## 2. Required setup: recipient enrollment is distinct from receiver login

`POST /v1/subscriptions` is supported today. Prepare exact bytes containing
`org_id`, `app_id: "tos>dm"` and optionally `for`. Use the recipient's DM access
token as the OBO subject, the DM app credential to sign the exchange, audience
`tos>ting`, endpoint `subscriptions.register`, metadata `{}`, and the SHA-256 of
those exact bytes. Submit them unchanged with the returned proof. The
[registration implementation](https://github.com/teamofsilicons/silicon-ting/blob/1999c7b02762077da77153b23de4bf5c8d6f9498/crates/ting-server/src/store.rs#L170)
binds the grant to the verified issuing app and represented recipient.

DM must declare the relevant external scopes, obtain Ting's critical review and
obtain recipient consent. Existing login tokens do not automatically gain a new
scope. The checked-in `releases/honeycomb-application.json` now declares
`tos>ting` / `subscriptions.register` and `tings.send`, and already declares
`self.identity.read`.
Production configuration revision 2 is now accepted at IAM revision 23, with
both external Ting scopes active. Publication request
`e13529a1-ee3d-451a-82e8-4391e74408ec` and activation operation
`a4debe2e-74e1-4e67-9c7c-010b38885d32` record the approved configuration. Both apps
and the exact consent disclose `self.identity.read`, because Ting checks the
verified nested public identity against the proof actor.

**One-time type setup is also required in each organization and environment.**
Ting stores notification types by environment, organization and application;
`tings.send` rejects an unregistered type. An authorized Ting application
manager must register `tos>dm.sync.changed` before DM can hand off updates in
that organization. The installed CLI exposes:

```sh
ting --org ORG types register --type 'tos>dm.sync.changed' \
  --description 'A DM message or receipt changed; fetch its current authorized state from DM.'
```

Use a deliberately selected session for the intended environment; this example
does not configure test credentials. Recipient enrollment cannot replace type
registration, and ordinary senders must not need type-management authority.
A shared test clean removes types as well as grants/hooks, so this setup must
be repeated explicitly after a clean. Production `tos>dm.sync.changed` was
registered in `tos` using the verified production Carbon session; this does not
register it in other organizations or testing environments.

Enrollment does not log the recipient into Ting or create a destination. A
receiver separately needs a Ting-bound IAM SLT, Ting session, and authenticated
hook/inbox subscription. DM cannot substitute a DM token for a Ting session.
Repeated enrollment reactivates a revoked grant, so it belongs to an explicit
recipient enrollment/reconnection action, not an unconditional background retry
or every ordinary DM request.

Ting registration has no producer idempotency-key contract comparable to a send.
DM therefore reserves each explicit registration key durably before calling
Ting, caches its confirmed public response, and replays that response without
another enrollment. If the result is uncertain after a crash or lost response,
the same key returns a conflict; a new explicit registration needs a new key.
This avoids silently reactivating a grant revoked after an uncertain acceptance.
Transparent reconciliation would require an agreed grant-query policy or a
Ting registration idempotency contract. No user token or proof is stored in
DM's enrollment receipt table.

## 3. Resolved in Ting 0.1.3: browser origins, CORS and session context

Ting 0.1.2 only accepts its configured frontend/public origins. Its
[origin check](https://github.com/teamofsilicons/silicon-ting/blob/1999c7b02762077da77153b23de4bf5c8d6f9498/crates/ting-server/src/main.rs#L260)
runs for HTTP and WebSocket upgrades. A browser WebSocket upgrade also requires
an already authenticated Ting cookie before receiving any protocol frames.
Ting's own browser uses a host-only HttpOnly cookie and same-origin requests.

Read-only live reproduction on 2026-09-22 at 16:32:58 UTC:

```sh
curl --include https://backend.ting.teamofsilicons.com/v1/iam \
  -H 'Origin: https://dm.teamofsilicons.com'
```

Result: HTTP 403, `permission_denied`, `This browser origin is not permitted.`
Request ID: `req_1c5dc7f987d545e7b62116df73e0d15b`.

The denial was reproduced again at 16:57:02 UTC with request ID
`req_919e5ad5f54f4f71a21443ea83eba16a`; upstream main remains the revision above.

The cookie-issuing frontend host was checked again at 18:17:35 UTC: both DM and
Interface Origins returned HTTP 403 with the same denial. Request IDs were
`req_bd8b357f3142433cb1d7af829c40e8f1` (DM) and
`req_3aefa59e48004fe1bd436183c3db3ab1` (Interface).

**What existing browser authentication can do.** A host-only cookie prevents
reading or sending it to another host; it does not inherently prevent a same-site
page from opening a WebSocket to the cookie's issuing host. After the user logs
in through Ting's existing browser flow, DM could open that Ting-host socket and
use `watch_inbox` if Ting explicitly permits DM's Origin. The analogous Interface
flow is also possible. This is a supported-contract design inference; neither
flow has been configured or exercised. Use the host that actually received the
login cookie: logging into the Ting website does not set a cookie on its separate
backend hostname. DM login alone does not create any Ting session.

**Actual deployment and integration gaps.** The current
[configuration](https://github.com/teamofsilicons/silicon-ting/blob/1999c7b02762077da77153b23de4bf5c8d6f9498/crates/ting-server/src/main.rs#L48)
permits only one frontend origin and its public origin, with no additional app
origin list. Supporting Ting's website plus DM and Interface therefore needs an
explicit deployment/origin policy. The candidate DM and Interface websites now
use matching-account Ting hints to trigger DM's authenticated HTTP sync without
fetching or acknowledging Ting's inbox. They expose a Ting sign-in link and an
explicit reconnect action. Ting's
[login return path is local only](https://github.com/teamofsilicons/silicon-ting/blob/1999c7b02762077da77153b23de4bf5c8d6f9498/crates/ting-server/src/main.rs#L729),
so an automatic cross-app return must not be assumed.

**Session context is not attested.** Ting's current `GET /v1/me` returns
`id`, `kind` and `authenticated`; it does not report the session's testing
environment or generation. A matching actor therefore cannot establish that
the Ting cookie and DM profile select the same plane. Both websites label
these hints as environment-unverified. Hints contain no DM content or authority:
the independently authenticated DM session, organization, environment and
generation govern every subsequent sync/content request. A confirmed browser
delivery status requires a verified upstream session-context contract; absent
fields are never interpreted as production. HTTP reconciliation also covers
silent notifications and lost hints.

Direct cross-origin Ting HTTP access additionally needs correct credentialed
CORS on actual responses. The inspected server adds CORS headers only to
[OPTIONS responses](https://github.com/teamofsilicons/silicon-ting/blob/1999c7b02762077da77153b23de4bf5c8d6f9498/crates/ting-server/src/main.rs#L299).
A live GET to `https://ting.teamofsilicons.com/v1/iam` with its permitted Ting
Origin at 16:58:57 UTC returned 200 without `Access-Control-Allow-Origin` or
`Access-Control-Allow-Credentials` (request
`req_75277710123c487a8ce321359a139aaf`); OPTIONS returned both (request
`req_e1ade846b70a4e718a5df8bc091ec834`). Its own same-origin browser is unaffected.
An Origin configuration change alone would not repair external HTTP fetches.
Do not expose IAM refresh tokens, use wildcard credentialed CORS, put credentials
in WebSocket URLs, strip Origin checks, or silently restore DM-owned client
sockets. These are specific setup and integration/code gaps, not proof that
browser delivery requires a fundamentally different authentication protocol.

Ting's existing `watch_inbox` is a refresh hint, not a full delivery frame. It
contains only `org_id`; Ting's inbox consumer refetches its inbox, while a DM
adapter can synchronize current DM state through DM's API. Both need recovery
after reconnect. Silent arrivals produce no hint. A view in DM must not
automatically mark unrelated Ting inbox items read. These semantics must be
preserved by any browser adapter.

**Real E2E gate.** From the actual DM and Interface origins, sign in through the
supported flow, receive a real Ting update, fetch the referenced DM data with DM
authority, reconnect and recover, then reject revoked/wrong-context authority.

## 4. Resolved in Ting 0.1.3: reusable WebSocket client

Published `silicon-ting-client` 0.1.2 exposes `Prepared`, `ProofOperation`,
`Prepared::sha256`, HTTP `Prepared::execute`, generic HTTP methods, and local
daemon IPC. It does not expose a reusable WebSocket publisher/receiver or a
browser SDK. The public WebSocket wire contract is implemented by the Ting
server and its daemon.

A DM adapter can implement that documented wire protocol with its existing
WebSocket library; this does not require a made-up Ting SDK API. It must preserve
the exact prepared body string, correlate `request_id`, answer ping, reconnect
with the documented backoff, and retain the original event key/body when a
reply is lost. The selected session-bound issuer supplies authority; browser integration in
issue 3 remains a separate dependency.

### Native receiver boundary

Ting 0.1.2 hooks receive all eligible notifications for their recipient and
organization; they do not offer a DM-only app/type filter. The local callback is
`{ "tings": [...] }`, and callback items omit `for`. HTTP 204 accepts the entire
batch and changes Ting's own read state. A generic consumer must route every
item it accepts. A DM-specific consumer must not silently discard other apps'
items and acknowledge the batch.

DM therefore hands an explicitly configured local destination directly to
Ting's shared daemon, with no DM incoming socket, forwarding endpoint or retry
queue. The Rust client supplies reference validation/hydration helpers; it does
not acknowledge Ting batches. Consumers authenticate the configured hook and
secret, supply its trusted recipient binding, fetch current DM contents through
DM authorization, and send DM delivered/read receipts separately.

## 5. Sandbox lifecycle is implemented, but must be proved across both apps

Ting validates the paired `IAM_TEST_APP_SECRET` and
`X-Testing-Environment-Key` with IAM and partitions by the verified IAM
environment UUID. For OBO calls the forwarded app secret is Ting's audience test
secret from IAM's returned testing context, not DM's test secret. A Ting session
retains its verified testing context. A DM-local root key must not be confused
with IAM's environment key.

The [lifecycle participant](https://github.com/teamofsilicons/silicon-ting/blob/1999c7b02762077da77153b23de4bf5c8d6f9498/crates/ting-server/src/lifecycle.rs#L28)
uses dedicated Honeycomb service authority, validates revision/generation/key
version, fences authority, and clears Ting payloads, hooks, grants, types,
preferences and producer keys on clean/purge. Thus a clean also requires fresh
type setup, enrollment and hook setup before the next real send. DM must not
declare Ting cleanup completed or call this control path with user/root-key
credentials.

The inspected router exposes the participant operation as PUT, with replayed
receipt returned by the same operation; no GET receipt route is present. Confirm
that the deployed Honeycomb participant adapter supports this before claiming
automatic lifecycle recovery. This is a compatibility question, not evidence
that an authorized lifecycle call failed.

**Required validation.** Import DM and Ting into one Honeycomb/IAM environment;
verify both participants' lifecycle completion; carry DM environment identity
and cleaning generation in each reference event; clean with pending deliveries;
then prove stale events cannot recreate DM state and fresh enrollment/delivery
works. Test secret/key rotation and retired contexts must fail closed with no
production fallback. Local fixtures do not prove this shared lifecycle.

## Resolved setup blocker: DM lifecycle connection

The task-owned shared environment is
`d70c8674-6d2e-41d4-bf8d-96ddd882edbd`. Honeycomb, Briefcase (a dependency) and
Ting imports were accepted. The DM import operation
`bfefc652-12cb-42da-a579-7b0005bfebc2` remains pending at environment revision 4.
Its service receipt reports:

> Protected lifecycle transport is not configured for this application

The import command returned success and top-level `state: ready`, but also
`operation_state: pending`. Inspecting `services` showed `tos>dm` failed and
`operation_pending: true`; DM is absent from the accepted `imports` list. The
sandbox therefore cannot yet be reported as ready for the complete DM flow.
The environment and original operation were re-read after local website
verification: revision 4 is still pending, with the same failed DM receipt and
DM still absent from accepted imports.

Repair requires the deployed Honeycomb participant registry to include `tos>dm`
and DM's explicit backend origin, referencing a dedicated matching
`DM_HONEYCOMB_SERVICE_TOKEN` in both services. Configure
`DM_HONEYCOMB_BASE_URL`, load the settings in both running services, and retry
that original operation. Do not substitute a user/root key for the service
credential or force a successful receipt. This is a deployment dependency,
not evidence of a failed Ting notification delivery.

The source audit confirmed there is no supported test-only override for this
transport. Honeycomb constructs `ParticipantRegistry` from
`HONEYCOMB_LIFECYCLE_PARTICIPANTS` at startup; changing a catalog or test app's
`base_url` cannot select a control destination. Test application configuration
also cannot activate while this import is pending. A separate local coordinator
would not repair or adopt this deployed operation.

The deployment change must preserve existing participant entries and add:

```json
{"app_id":"tos>dm","base_url":"<DM HTTPS backend origin>","token_env":"DM_HONEYCOMB_SERVICE_TOKEN"}
```

Provision the matching dedicated credential through the services' secret stores,
then reload their configuration. Afterward, inspect the environment's current
revision and retry through its environment action route:

```text
POST /api/v1/environments/d70c8674-6d2e-41d4-bf8d-96ddd882edbd/actions/retry
If-Match: <current environment revision>
Idempotency-Key: <fresh retry key>
```

The authenticated environment manager can also use `honeycomb environments
action ENVIRONMENT_ID retry --revision CURRENT_REVISION`. This resumes the
original operation and skips already-ready receipts. The generic
`/operations/{id}/retry` route handles configuration operations and is not the
environment retry route. Success requires the original operation to complete,
DM to appear in accepted imports, and `operation_pending` to become false;
top-level `state: ready` alone is insufficient.

A candidate backend was built and started against two task-owned local
PostgreSQL databases with a temporary HTTPS test endpoint. The tunnel ended and
the unused backend was stopped while the shared lifecycle prerequisite remains
unresolved. No production deployment/configuration was changed and no real
message was sent. Test credentials are kept outside the repo.

## 6. Delivery limits and semantics remain integration constraints

- The complete send body is limited to 256 KiB. DM should send bounded reference
  events and fetch message contents through DM authorization, preserving edits,
  deletion and permission changes.
- The producer key scope is `(environment, org, app, key)`, not recipient or
  conversation. Include the recipient and unique DM event identity in each key.
  Retain immutable event content across retries. Deduplication lasts 14 days;
  retrying an uncertain acceptance after that window can produce a new Ting ID.
  Consumers must additionally make the DM event idempotent. An unlimited
  exactly-once Ting acceptance guarantee is not available.
- Ting `read` means a destination durably accepted the batch or the recipient
  viewed its notification. It does not establish a DM message delivered/read
  receipt. Send those receipts explicitly through DM's API.
- Hooks have independent unfinished copies. Preserve stable hook IDs across
  recovery; replacement IDs cannot recover copies already globally read.
- Read or silent events expire one calendar month after creation; other unread
  events expire after three months. A read elsewhere can shorten retention of a
  still-pending hook copy. DM history remains authoritative after Ting expiry.
- Muted events are accepted as silent and never automatically replay. This is
  Ting policy, not transport failure or proof that a DM client is synchronized.

All outstanding external gates above must be reported separately from local
compilation, protocol fixtures and DM unit/integration tests. Successful mocks,
health checks or a configured client are not a real Ting delivery result.

## DM implementation checkpoint

The backend now wires the originator-authenticated Ting worker into production
and sandbox assembly. It includes atomic reference handoffs, immutable retry
bodies, encrypted verified access-token storage, fresh proof issuance, explicit
recipient enrollment, HTTP reference catch-up and device presence. Message
objects are unchanged. Sandbox attempts require the live DM lifecycle fence.
The Honeycomb application configuration declares both `subscriptions.register`
and `tings.send`; live review and recipient consent are separate prerequisites.

Backend cutover validation passed locally: 78 workspace library tests, seven
WebSocket transport fixtures, native PostgreSQL handoff/worker/sync/presence/API
fixtures, encrypted credential cache and originator publisher recovery tests,
and workspace Clippy. The cancellation fixture verifies that a dropped publish
attempt discards its in-flight socket; optional login credential capture runs
outside the token-return response and reacquires its sandbox lifecycle fence.
These are local contract tests, not real remote delivery evidence.

The Rust client now removes the old incoming DM delivery daemon and forwarding
queue from active operation. It configures destinations directly on Ting's
installed daemon, maintains a separate verified Ting login, and supplies
reference validation and authorized content-fetch helpers. Legacy incoming
queue records remain available for inspection; they are not forwarded or
acknowledged. Current local verification passes 27 client unit tests and three HTTP
fixtures, including transient presence failures, late authentication responses
and explicit Ting environment attestation. Presence failures cannot hold later durable sends in the outgoing queue.

The CLI now exposes explicit recipient registration, separate Ting login,
status, reconnect, logout, and direct destination attachment. Its local command
tests are passing: 12 CLI tests, package Clippy and documentation mirror checks.
Workspace Clippy also passes after the client follow-up. Both website migrations
are implemented locally. Real backend send/receive and recovery checks now pass
against deployed IAM and Ting as recorded above. Embedded CLI API documentation
is updated with backend documentation.

Before the 0.1.3 follow-up, DM web passed 65 tests and Interface passed 351 tests.
After environment-attestation support, DM web passes 83 tests and Interface
passes 359 tests; TypeScript checks and both production builds pass. Its Solid workspace test
covers initial discovery recovery, hidden hints without read receipts, visible
message fetch/read, explicit enrollment and scope disposal. The DM gateway's
retired socket and exact-origin CSP fixtures pass. The actual-browser DM run
passed 29 local checks, including explicit 0.1.3 environment matching, legacy
unverified context, mismatched-environment rejection, direct Ting cookies/hints, paused/reconnect
recovery, opaque cursor pagination, transactional IndexedDB rollback, generation
fencing, and HTTP send/authentication recovery. Its
[saved report](ting-browser-verification.json) explicitly identifies the run as
a loopback fixture and contains no real credentials. Final cross-review fixed
duplicate-reference hydration and delayed cross-tab generation races; focused
regressions cover both. These local results do not satisfy the shared remote
environment or live browser gates.

The installed Ting CLI's latest read-only inspection reported version 0.1.2,
logged in as `saket`; organization `tos` and app `tos>dm` are visible with
`can_manage_tings: true`. Explicit-org type and subscription listings were
empty. No production type, grant or notification was created by those inspections.

An isolated Honeycomb environment, `DM Ting delivery integration`
(`d70c8674-6d2e-41d4-bf8d-96ddd882edbd`), was created for this task. Its test Carbon
and organization were created through IAM's documented test-only verification
flow. Shared import is blocked on DM's missing protected lifecycle connection, as described above. Isolated private test
configuration can stage the Ting scopes without changing production DM's
accepted configuration; that does not replace production scope review.

No real Ting message delivery or deployment has been proved yet. Browser-origin
support in issue 3 remains a separate live E2E prerequisite. Report each external
gate separately from local implementation and fixtures.
