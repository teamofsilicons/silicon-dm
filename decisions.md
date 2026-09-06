# Silicon DM engineering decisions

This is the append-only decision log for the Silicon DM backend. Material
architecture, security, data-model, API, and operational choices are recorded
before or alongside their implementation. Superseded decisions remain in the
file and receive an explicit replacement record.

## D-000 — Contract source precedence and scope

**Status:** Accepted

`UNDERSTANDING.md` defines product intent, `API_DOCS.md` defines the human API
contract and identifies open questions, and `openapi.yaml` defines the current
machine surface. When they conflict, the safer behavior that preserves product
intent is recorded here and the two API documents are updated together. Legacy
interface documents and sibling-service implementations are context only; they
do not silently expand DM into editing, deletion, reactions, search,
moderation, group administration, or other uncontracted features.

## D-001 — PostgreSQL is the source of truth

**Status:** Accepted

PostgreSQL owns conversations, membership snapshots, messages and their stable
sequences, receipts, bundles, drafts, GIF history, durable system events,
delivery attempts, idempotency records, and the delivery outbox. Transactions
and constraints enforce invariants that cannot safely live only in a process.
No successful API response may depend solely on an in-memory write.

## D-002 — Modular monolith with independently scalable processes

**Status:** Accepted

DM is one Rust package with a shared library and thin `dm-api`, `dm-worker`, and
`dm-migrate` binaries. Domain, application, infrastructure, HTTP, realtime, and
worker concerns remain separate modules. The API and delivery worker can scale
independently without introducing premature internal network boundaries.

## D-003 — Rust engineering baseline

**Status:** Accepted

The service uses Rust 2024 with a pinned stable toolchain, rustfmt, strict Clippy
lints, dependency policy checks, `#![forbid(unsafe_code)]`, and no `unwrap`,
`expect`, `todo`, or `unimplemented` in production paths. Axum/Tokio/Tower form
the server stack, SQLx owns PostgreSQL access, and Reqwest with rustls owns
outbound HTTP. Recoverable failures are typed and propagated.

## D-004 — IAM remains authoritative and DM fails closed

**Status:** Accepted

Bearer tokens are opaque and introspected through IAM with DM application
credentials. OBO proofs are verified through IAM and bound to DM, the requested
action, resource, actor, organization, and expiry. Hook service tokens are also
introspected and must identify the configured Silicon Hook service. DM does not
derive authority from unverified token contents or request headers.

The current IAM machine contract does not expose all contactability and
multi-actor-representation checks required by the prose. These checks live
behind an IAM authorization port. Until IAM supplies them, DM accepts only
active same-organization actors returned by IAM and rejects authority it cannot
prove.

## D-005 — Exact participant sets identify conversations

**Status:** Accepted contract clarification

Creating a conversation automatically includes the authenticated actor,
canonicalizes and deduplicates the participant set, and requires at least two
actors after that inclusion. Within one organization, the exact actor set maps
to one conversation because the current contract has no name, topic, or
explicit "new group" discriminator. The operation is additionally idempotent
per organization, actor, operation, and idempotency key.

## D-006 — UUIDv7 identifiers and database-assigned sequences

**Status:** Accepted

DM generates UUIDv7 identifiers for time locality without treating UUID order
as business order. Each conversation owns a monotonically increasing 64-bit
message sequence assigned while the conversation row is locked. Actor delivery
streams have independent monotonically increasing 64-bit sequences. Cursors are
opaque, versioned values scoped to one endpoint family and clients must not
infer ordering from timestamps. Cursor manipulation cannot bypass participant
and organization checks independently performed on every page.

## D-007 — Transactional outbox and replayable actor deliveries

**Status:** Accepted

Creating a message or accepting a Hook event atomically creates one durable
delivery row per recipient actor. PostgreSQL `NOTIFY` is only a low-latency wake
up; it is never the durable payload. Workers and API instances may miss a
notification without losing data because unacknowledged rows are replayed from
PostgreSQL. Claims use bounded leases and `FOR UPDATE SKIP LOCKED`; retries use
capped exponential backoff and dead-letter only after the configured limit.

## D-008 — WebSocket ACK and resume semantics

**Status:** Accepted contract extension

The published WebSocket schema is incomplete. DM adds a versioned `ready` frame,
delivery envelopes carrying actor delivery sequence, client `ack` frames, a
`resume` frame, presence updates, receipt commands, and structured errors.
Heartbeats remain ephemeral, consume no sequence, and close with code 4000 and
`heartbeat-timeout` after two minutes without a matching pong.

An ACK is cumulative for one actor stream and one connection: acknowledging
sequence N acknowledges every delivered row through N. Rows are retained for
an operational retention window after acknowledgement rather than immediately
deleted. Reconnect resumes after the client's last acknowledged sequence; a
client may safely receive duplicates and deduplicates by stable delivery ID.

## D-009 — Message aggregate receipt policy

**Status:** Accepted contract clarification

Receipts are monotonic per message, recipient actor, and device. One device is
enough to mark that recipient actor delivered or read. A group message becomes
globally `delivered` or `read` only when every recipient actor has reached that
state; the sender is excluded. `read` implies `delivered`. Duplicate and stale
updates succeed idempotently but never move state backward.

## D-010 — Bundles are atomic, flat, and append-positioned

**Status:** Accepted contract clarification

Only a Silicon participant may create a bundle. Selected members must be unique,
visible, unbundled messages in the same conversation. Nesting and rebundling
are rejected. The display message receives a new sequence at the end of the
conversation, while members keep their original sequences and content. Default
message listing hides members; bundle detail and
`include_bundled_members=true` expose them.

## D-011 — Draft conflicts and safe automatic clearing

**Status:** Accepted contract clarification

`If-Match: 0` or an absent header may create a draft only when none exists.
Replacing an existing draft requires its exact current version; conflicts
return the current server draft. Sending a message clears the authenticated
actor's draft in the same transaction only when the canonical content hash
still equals the sent content. A newer or different draft is never erased.

## D-012 — Voice transcription is best-effort but blocks acceptance

**Status:** Accepted

When a voice attachment has no transcript field, DM calls Waveform before
persisting the message. A successful transcript is stored; a provider failure
is recorded as a null transcript and the voice message is still accepted, as
the product document requires. DM never persists Waveform or Briefcase
temporary URLs. Idempotent request serialization prevents concurrent retries
from starting multiple accepted messages.

The published attachment schema has no duration field, so the 48-hour limit can
only be enforced when Waveform returns duration metadata. DM records this
contract limitation and fails validation when a known duration exceeds it.

## D-013 — Attachment URLs and delegated access

**Status:** Superseded for bearer forwarding by D-039; OBO route handling superseded by D-030

DM stores only canonical HTTPS Briefcase permanent URLs and validates their
configured host and entry identifier. Temporary URLs are fetched on demand and
never logged or stored. Bearer-authenticated requests may be forwarded to
Briefcase as the same actor. OBO requests require an audience-bound proof for
Briefcase. IAM's exchange accepts an actor token issued to the calling app, but
DM receives only a single-use DM-audience proof and cannot chain it into another
proof. DM therefore fails that path closed rather than forwarding an invalid
proof or broadening authority. D-030 later makes temporary-URL creation
bearer-only at the authentication boundary.

## D-014 — Presence is soft state with durable multi-node coordination

**Status:** Accepted

Local connection channels provide immediate delivery. A lightweight PostgreSQL
session lease supplies cross-instance online state, activity, and last-seen
coordination. Heartbeats renew the lease; disconnect or expiry removes activity
and advances last-seen. Presence does not enter the message outbox and may lag
by one heartbeat during process failure.

## D-015 — GIF provider and recent-history policy

**Status:** Accepted

Giphy credentials remain server-side. Trending results are cached per process
for a short configured TTL; searches are not cached or written to history.
Sending a message containing a GIF atomically updates a Carbon sender's
deduplicated, recency-ordered history capped to 20 entries. Silicon callers get
an empty recent list because the product contract defines history only for
Carbons.

## D-016 — Explicit resource bounds

**Status:** Accepted security clarification

Text is limited to the documented 100,000,000 Unicode scalar values, attachment
metadata may declare at most 5 GiB per item, bundles contain 1–100 members, and
page size is 1–100. Because the contract does not cap attachment count, DM sets
a defensive maximum of 100 items per message or draft. HTTP body size is
configurable and defaults just above the maximum encoded text size. All
external calls, database acquisition, statements, and graceful shutdown have
deadlines.

## D-017 — Observability and secret hygiene

**Status:** Accepted

Every request receives or propagates a request ID. Structured traces record
operation, latency, status, stable entity IDs, retry counts, and dependency
names, but never authorization headers, proofs, message bodies, transcripts,
attachment URLs, GIF queries, or Hook payloads. Health and readiness are
separate: liveness does not depend on downstream services, while readiness
requires PostgreSQL and configuration validity.

## D-018 — Hook envelope and payload bounds

**Status:** Accepted security clarification

DM stores the exact contracted Hook fields (`event_id`, organization, target
Silicon, type, trace ID, and payload) and rejects payloads larger than 1 MiB or
empty event types. The upstream Hook fields absent from DM's contract are not
invented or silently embedded. `event_id` is the inbox idempotency key. A retry
with the same ID and different canonical content conflicts; an exact retry
returns accepted without creating another delivery.

## D-019 — Retention and terminal delivery behavior

**Status:** Accepted interim operations policy

No conversation message, bundle, draft, receipt, Hook event, or audit-relevant
record is automatically deleted until product retention and legal-hold policy
is defined. Acknowledged actor-delivery envelopes and completed idempotency
records may be compacted after 30 days because their underlying message/event
remains durable. Offline actors do not consume delivery attempts. Only an
actual delivery-processing error increments attempts; exhausting attempts marks
that delivery and its message failed and stops automatic retry pending an
operator replay facility.

## D-020 — Pagination and timeline direction

**Status:** Accepted contract clarification

Conversation pages are ordered by durable activity time descending with UUID as
a deterministic tie-breaker. Message pages are ordered by conversation sequence
descending so the first page contains the newest history; each page itself
retains that order. Cursors encode the last ordering tuple and are scoped to one
resource family. New writes may appear before an existing cursor but never
duplicate or reorder the older sequence range that cursor traverses.

## D-021 — Stable OBO action names

**Status:** Accepted contract extension; route set narrowed by D-030

IAM OBO proofs for DM use explicit lower-case actions such as
`dm.conversations.list`, `dm.messages.create`, `dm.receipts.create`,
`dm.drafts.write`, and `dm.gifs.search`. The HTTP method and matched route
select the action; the concrete request path is the resource binding. Unknown
routes map to no granted authority and are rejected by IAM, preventing a broad
fallback action. D-030 removes actions for routes that cannot complete their
authorization or downstream delegation using a DM-audience proof.

## D-022 — Hook target authorization bridge

**Status:** Accepted dependency assumption

DM retains an already-introspected Hook service bearer only for the lifetime of
the current request and uses it to ask IAM's organization member directory for
the exact target Silicon. The service token must carry the narrow Hook-to-DM
delivery capability `dm.hook_events.deliver`, and IAM must authorize that
directory read. Tokens are
never cached, persisted, or logged. If IAM does not support this service-bearer
lookup, Hook ingress fails closed until IAM publishes a dedicated app-auth
authorization operation.

## D-023 — Explicit process assembly and bounded shutdown

**Status:** Accepted

The API, delivery worker, and migration runner are separate Tokio executables
over one shared bootstrap layer. API and worker processes construct validated
adapters, connect to PostgreSQL, and pass a readiness query before announcing
that they are ready. They never run schema migrations implicitly; deployment
must execute `dm-migrate` with its independently scoped database credentials.

The API enforces the configured request-body and request-duration bounds before
dispatch. API and worker processes translate operating-system shutdown signals
into cooperative cancellation, wait up to the configured shutdown deadline,
and then allow the runtime to close remaining work. Startup and lifecycle logs
contain only stable failure categories, safe socket addresses, and random
process instance IDs; settings, connection URLs, credentials, and dependency
request data are never formatted into logs.

## D-024 — Realtime delivery pumps are co-located with API instances

**Status:** Accepted; clarifies D-002

The realtime connection registry is deliberately process-local. Every API
instance therefore runs a delivery pump over the same PostgreSQL outbox and
claims only actor streams that have a socket registered in that instance.
Multiple API instances may safely race because claims are leased with
`FOR UPDATE SKIP LOCKED`; a saturated or disconnected local channel leaves the
durable row replayable. Organization, actor kind, and actor ID are all part of
the local routing key and database claim predicate.

A standalone `dm-worker` has no authority or mechanism to publish directly
into another process's sockets. With an empty local registry it performs lease,
presence-expiry, idempotency, and retention maintenance without consuming
offline delivery attempts. Introducing a broker or cross-process RPC would be
a new architectural decision; PostgreSQL replay plus co-located pumps is the
current scale-out model.

## D-025 — Realtime command confirmations are ephemeral

**Status:** Accepted contract extension

After `send_message` commits, its originating socket receives an unsequenced
`message_accepted` frame containing the idempotency key and durable message.
After a monotonic device receipt commits, it receives `receipt_recorded`.
Neither confirmation is a recipient delivery, consumes an actor delivery
sequence, or requires an ACK. Losing a confirmation is safe: message retries
reuse the required idempotency key and receipt retries are monotonic and
idempotent. Durable recipient messages and aggregate sender status updates
continue to use sequenced, replayable delivery envelopes.

## D-026 — Health probes are root-level operational routes

**Status:** Accepted

`GET /live` and `GET /ready` are unauthenticated root-level routes rather than
members of the versioned `/api/v1` product contract. Liveness returns `204`
without a dependency call. Readiness returns `204` only after a PostgreSQL
round-trip and otherwise returns `503`. Ordinary REST calls use the configured
request deadline; WebSocket upgrades are excluded from that deadline so a
healthy long-lived connection is not terminated by REST timeout middleware.
Both probes still receive request IDs, tracing, panic containment, and secret
header redaction.

## D-027 — Ambiguous public actor IDs fail closed

**Status:** Accepted interim contract policy

The current DM wire contract names participants and presence targets by public
actor ID without actor type, while IAM permits public labels to collide across
types. DM never resolves such an ID according to directory page order. It scans
the complete bounded active-member result and rejects any requested public ID
that maps to more than one typed IAM actor. The authenticated principal remains
unambiguous because IAM binds its principal ID, actor type, and public ID.

A future version should accept `ActorRef` or tenant-qualified IAM membership
IDs on every actor-addressing input. That is a breaking API change and is not
silently introduced into the current published string-ID surface.

## D-028 — DM owns voice transcription outcome

**Status:** Accepted; supersedes the conditional-call portion of D-012

Every accepted voice attachment is submitted to Waveform, even if the client
supplies a transcript field. A successful Waveform transcript replaces the
client value and its returned duration enforces the 48-hour bound. A terminal
transport, timeout, rate-limit, server, or malformed-response failure becomes a
null transcript so the voice message is still durably sent as product intent
requires. Bad attachment input, inaccessible media, authentication,
authorization, conflict, and unsupported OBO delegation remain request errors;
they are not misclassified as completed transcription attempts.

## D-029 — Protocol v1 heartbeat timing is fixed

**Status:** Accepted

WebSocket protocol version 1 fixes application pings at 30 seconds and the
heartbeat close deadline at 120 seconds. Environment variables retain explicit
deployment visibility but configuration validation rejects different values;
changing either duration requires a protocol-version decision. Activity expiry
and outbound queue capacity remain operationally configurable because they do
not alter the published heartbeat contract.

## D-030 — OBO security is compound and route-specific

**Status:** Accepted contract correction

An OBO request is authenticated only by the combination of
`X-IAM-OBO-Access-Proof` and `X-App-ID`; OpenAPI models both schemes together so
generated clients cannot omit the issuer application. Bearer-only overrides are
published for WebSocket upgrade, conversation creation, presence lookup, and
Briefcase temporary-URL creation because those paths need representation,
directory, or downstream delegation authority that a consumed DM-audience
proof does not provide. Other routes retain OBO when all resource-level checks
can be completed locally after IAM verifies the exact action and resource.

## D-031 — GIF safety defaults to the strictest provider rating

**Status:** Accepted interim policy

Giphy requests use the fixed `g` rating and HTTPS-only response filtering.
IAM's machine contract does not publish an organization-specific GIF safety
setting, so DM applies this strict default for every organization rather than
inventing or trusting a caller-provided rating. A configurable organization
policy requires a new IAM contract and decision.

## D-032 — PostgreSQL notifications accelerate but never own delivery

**Status:** Accepted

Every committed outbox insertion emits `pg_notify` on the shared delivery
channel. API delivery pumps subscribe with a dedicated pooled connection when
the configured pool has at least two connections; any notification immediately
starts a local-target claim cycle. A listener failure disables only this
optimization, logs a stable failure category, and is retried during maintenance.
The bounded poll interval remains the correctness backstop because PostgreSQL
notifications can be coalesced or missed during reconnect. Pools limited to one
connection skip `LISTEN` so a best-effort wakeup cannot starve authoritative
database operations.

## D-033 — Conversation membership has a defensive cardinality bound

**Status:** Accepted contract clarification

One conversation contains 2–100 unique actors including the authenticated
creator. The product contract did not define an upper bound; 100 aligns with
the realtime representation and bundle cardinality bounds and prevents one
request from creating unbounded IAM scans, participant rows, outbox fan-out,
and receipt aggregation work. Raising the limit requires explicit capacity
testing and a contract revision.

## D-034 — Production PostgreSQL verifies peer identity

**Status:** Accepted security policy

API, worker, and migration configuration reject a production database URL
unless it contains exactly one `sslmode=verify-full`. Encryption without server
identity verification is insufficient for message content and credentials in
transit. Development and test environments may use local plaintext PostgreSQL;
the connection URL remains secret and is never included in validation errors or
logs.

## D-035 — Dependency policy separates private code from third-party licenses

**Status:** Accepted

The unpublished DM crate uses the custom SPDX reference
`LicenseRef-Proprietary` and is excluded from third-party allow-list evaluation.
Every registry dependency remains subject to `cargo-deny` advisory, yanked,
license, source, and wildcard checks. Transitive version duplication is visible
as a warning rather than a release failure because independent current stacks
such as SQLx, Rustls, Reqwest, and Testcontainers do not always converge on one
version; denied advisories, sources, licenses, and wildcard requirements remain
hard failures.

## D-036 — Empty message text is not content

**Status:** Accepted contract clarification

A supplied message `text` value must contain at least one Unicode scalar value;
an empty string cannot by itself satisfy the requirement that a message carry
content. Empty synchronized draft text remains valid because clearing an editor
is a meaningful local-first draft state. Whitespace is preserved rather than
silently normalized because DM stores user-authored content verbatim.

## D-037 — Service base URLs are structural configuration

**Status:** Accepted security hardening

Public and dependency base URLs accept only absolute HTTP or HTTPS URLs without
embedded credentials, queries, or fragments. Production requires HTTPS; local
development may use HTTP only on loopback. Provider credentials and request
parameters are added by typed adapters rather than encoded into base URLs, which
keeps configuration validation deterministic and prevents accidental secret
propagation when endpoint paths are composed.

## D-038 — Provider enrichment does not alter idempotent request identity

**Status:** Accepted reliability hardening

Message and bundle request hashes include their conversation, selected bundle
member order, and canonical user-authored content, but exclude sender routing
metadata and the Waveform-owned transcript. The authenticated actor already
scopes the key, and DM always replaces any client transcript with the provider
outcome. A retry therefore replays the original durable result even if a
best-effort transcription attempt changes from terminal failure to success; it
does not conflict or create a second message because enrichment timing changed.

## D-039 — Provider calls use first-hop IAM OBO exchange

**Status:** Accepted; supersedes bearer forwarding in D-013

DM never forwards an incoming actor bearer to Briefcase or Waveform. For a
bearer-authenticated operation it uses its IAM application credentials and the
actor-bound DM token with `POST /api/v1/obo-access/exchanges`, then sends only
the returned single-use proof and DM app ID to the provider. Briefcase proofs
are bound to its configured audience, action `briefcase.file.temporary_url`,
organization, and parsed entry UUID. Waveform proofs are bound to its
configured audience and action `waveform.stt`.

Incoming DM-audience OBO proofs remain non-chainable because IAM exchange
accepts only an actor token issued to the calling application. Voice content on
such a request fails closed, while the temporary-URL route remains bearer-only.
Distinct delegated-credential and inbound-credential Rust types prevent an
adapter from accidentally receiving a raw bearer.

## D-040 — OBO verification idempotency is attempt-scoped

**Status:** Accepted security correction

Each call that consumes an incoming OBO proof uses a fresh non-secret IAM
idempotency key. DM's HTTP client does not automatically retry that call. A
stable key derived only from proof, action, and path would let IAM replay its
successful verification for a second DM request, defeating the proof's
single-use property and permitting different message bodies under different DM
idempotency keys. A second request with the same proof now reaches IAM as a new
consume attempt and is rejected as replay.

The first-hop provider exchange likewise uses a fresh operation key. If its
response is lost, the outer request retries and receives a newly minted proof
rather than replaying one that the downstream service may already have
consumed.

## D-041 — Readiness proves schema identity and runtime access

**Status:** Accepted operations hardening; supersedes the connectivity-only portion of D-026

API and worker startup, and every `/ready` probe, verify every embedded SQLx
migration version and checksum against `_sqlx_migrations`, then execute a
permission-sensitive query against a required DM table. A reachable but
unmigrated, modified, stale, or inaccessible schema is not ready. Migrations
remain an explicit release step and runtime processes never mutate the schema.

## D-042 — Mutable database time advances after lock acquisition

**Status:** Accepted concurrency hardening

Mutable monotonic timestamps use PostgreSQL's wall clock at statement execution
and `GREATEST` with stored state instead of a transaction-start timestamp. An
earlier-started transaction that waits behind a later commit therefore cannot
move directory refreshes, receipts, heartbeat state, or generic `updated_at`
backward or receive a false durable-invariant error. Creation timestamps remain
transaction-scoped where a single aggregate needs one stable creation instant.

Final offline transitions additionally take deterministic actor-scoped
transaction advisory locks. Concurrent closes and lease expiry serialize per
actor, so two final sessions cannot each observe the other and permanently
skip `last_seen_at`; unrelated actors remain concurrent.

## D-043 — Encoded request memory has an independent hard ceiling

**Status:** Accepted capacity and security policy; clarifies D-016

The product's 100,000,000-character bound applies to decoded text. HTTP and
WebSocket JSON are independently limited to 128 MiB encoded so escaping and
multi-byte input cannot turn one contract field into an unbounded process
allocation. Configuration cannot raise this process safety ceiling. Oversized
input is rejected before durable acceptance.

Canonical content and idempotency hashes stream JSON directly into BLAKE3
instead of allocating a second complete serialized body. Realtime fan-out
queues share one immutable frame with `Arc` across local connections instead of
deep-cloning message content per socket. Raising either limit requires measured
heap, database, and fan-out capacity evidence.

## D-044 — Release panics unwind into the HTTP containment layer

**Status:** Accepted resilience correction

Release builds retain Rust's default unwind behavior because the router's panic
containment can only translate an unwinding handler panic into a controlled
server response. `panic = "abort"` would terminate the entire process and make
the recorded containment guarantee false. Ordinary recoverable failures remain
typed errors; orchestration restart is still the backstop for failures outside
an unwind boundary.

Worker configuration is rejected at startup when its batch exceeds the storage
claim maximum or its durations cannot be represented by PostgreSQL and the
time library. A configuration accepted as ready cannot later stop the API on
the first connected delivery target solely because of a conversion mismatch.

## D-045 — Migration ownership and runtime data access are separate

**Status:** Accepted deployment hardening

The final migration revokes public access to DM schemas and private functions.
Deployment runs `dm-migrate` as the object-owning migration role, then applies
`deploy/runtime-grants.sql` to a distinct API/worker role with only schema
usage, DM table data access, and read access to SQLx migration metadata.
Default privileges preserve that split for later migrations.

CI builds the release image, migrates PostgreSQL as a migration role, applies
the runtime grants, boots API and worker as the runtime role, and probes
liveness/readiness. The image supplies a container-specific
`0.0.0.0:8080` bind default while local non-container execution remains on
loopback.

## D-046 — Published identity contracts are a release integration gate

**Status:** Accepted integration constraint; Hook portion superseded by D-049 and Waveform portion superseded by D-048

DM targets the reviewed IAM, Briefcase, and Waveform machine contracts and does
not translate missing delegated actions into broader organization
capabilities, trust unverified public actor identifiers, forward actor bearer
tokens to providers, or query another service's database. Those substitutions
would weaken audience, actor, organization, resource, or single-use proof
binding.

The sibling IAM implementation in this workspace currently diverges from its
published contract: the generic application-authenticated token introspection
route is not mounted; OAuth userinfo is Carbon-only; app-bound OAuth tokens
cannot perform the membership lookup needed to cross-bind the introspected
principal to a public actor ID; and the OBO action registry accepts only IAM's
organization capabilities rather than DM or provider actions. The generic
service-token path is therefore unavailable as well. Briefcase requires
organization role/tag claims absent from IAM's OBO verification result, and
Waveform's production delegation and Briefcase adapters remain fail-closed.

These mismatches are explicit upstream release blockers. DM retains strict
validation and fails closed until cross-service contract tests demonstrate
Carbon and Silicon bearer authentication, Hook service authentication, every
DM OBO action, Briefcase temporary-URL delegation, and Waveform transcription.
Database readiness remains DM-local and intentionally does not report an
upstream contract mismatch as a local schema failure.

## D-047 — External attachment references are passive HTTPS content

**Status:** Accepted contract change; supersedes the Briefcase-only storage portion of D-013

Messages and drafts may store either a canonical permanent Briefcase entry URL
or another absolute HTTPS URL. Every reference is bounded, must have a host,
and cannot contain embedded credentials. A URL whose origin matches the
configured Briefcase service must retain the exact canonical entry shape so a
temporary or misleading same-origin URL cannot be persisted as a permanent
entry.

DM never fetches, proxies, scans, signs, or follows redirects for an external
attachment reference. This keeps arbitrary links outside the server-side SSRF
boundary. Only the explicit temporary-URL endpoint contacts Briefcase, and that
endpoint continues to reject every non-Briefcase URL. DM guarantees durable
storage of an external reference, not the availability, safety, or privacy of
third-party content rendered by a client.

## D-048 — Voice metadata and transcript are supplied message content

**Status:** Accepted contract change; supersedes D-012, D-028, the voice portion of D-038, the Waveform portion of D-039, and the Waveform portion of D-046

A voice item is represented by a distinct typed structure containing its URL,
basic display metadata, and required `duration_milliseconds`. New voice writes
must declare a positive duration no greater than 48 hours. Duration, size,
media type, and transcript are caller-supplied metadata unless another content
service independently verifies them.

`voice_transcript` is optional client-owned content. DM preserves it and
includes both transcript and voice duration in message, bundle, draft,
idempotency, and safe-draft-clearing hashes. Reusing an idempotency key with a
different transcript or duration therefore conflicts instead of silently
replaying different content.

DM no longer performs blocking Waveform speech-to-text, exchanges a
Waveform-scoped proof, overwrites a supplied transcript, or requires Waveform
configuration at startup. This aligns voice acceptance with arbitrary external
audio references and removes a provider dependency from the durable send path.

The forward migration retains the deprecated transcription-outcome column and
enum so an upgrade does not erase historical provider results; an insert-only
database guard requires new writes to leave that field at `not_applicable`.
Historical voice rows whose provider never supplied duration remain readable
with `duration_milliseconds: null`; DM does not invent playback metadata or
mutate sealed message content. `NOT VALID` attachment checks preserve those
rows while PostgreSQL still enforces required duration and credential-free URLs
for every new or changed attachment.

Message and draft hashes are explicitly versioned. Existing rows remain v1,
while this release writes v2 hashes that include duration and transcript. A send
computes both shapes so a matching v1 draft can still be cleared before its
parent is deleted. Pre-v2 voice idempotency keys intentionally conflict when
retried with newly required duration rather than risking a duplicate message;
their existing completed responses and immutable request hashes remain intact
until ordinary idempotency retention expires.

## D-049 — Silicon Hook ingress is retired without erasing history

**Status:** Accepted scope removal; supersedes the Hook portions of D-004, D-007, D-018, D-019, D-022, and D-046

The product contract no longer includes Silicon Hook requests. DM therefore
removes the internal Hook route, service-token authentication path, Hook target
authorization, `SystemEvent` domain/API types, and public realtime system-event
frames. Keeping an undocumented authenticated ingress would create a shadow
attack surface.

Previously applied migrations remain immutable. The retirement migration keeps
historical system-event rows for an explicit future retention policy, marks
unacknowledged legacy deliveries dead-lettered with a stable removal reason,
prevents new deliveries of the retired kind, and prevents runtime claim or
replay code from hydrating it. Deployment also revokes the API/worker role's
direct privileges on historical Hook sources. No Hook payload is silently
reclassified as a conversation message and no historical row is destructively
purged by this release.

## D-050 — Giphy credentials are required runtime configuration

**Status:** Accepted configuration clarification

`DM_GIPHY_API_KEY` is required for API and worker startup instead of allowing a
partially configured process whose GIF calls fail later. The key remains a
redacted secret, is added only to outbound Giphy query parameters, and is never
logged or returned. Trending remains an explicit authenticated endpoint with a
bounded process-local cache; there is no implicit fallback request mode.

## D-051 — Realtime protocol version 2 removes system-event frames

**Status:** Accepted breaking protocol revision

The server advertises realtime protocol version 2 after removing the retired
`system_event` delivery frame and adding required voice-duration metadata to
new message content. Historical voice rows may carry a null duration when that
metadata never existed. Existing message and receipt delivery IDs, actor sequences,
cumulative ACKs, replay behavior, heartbeats, and command confirmations remain
unchanged. A version bump makes the wire incompatibility explicit instead of
letting an older client infer support from a version-1 ready frame.

The pre-1.0 Rust package and OpenAPI document advance together to `0.2.0` for
the same breaking contract boundary.

## D-052 — IAM application sessions and official client

**Status:** Accepted; supersedes bespoke IAM and OBO integration assumptions

DM uses the official Silicon IAM Rust client. A caller exchanges an opaque,
organization-bound IAM short-lived token through DM; the backend keeps the
application secret and uses current IAM authorization snapshots for actor,
organization, role, and membership checks. DM exposes no OBO endpoints and
accepts no inbound OBO proofs. Signed IAM webhooks trigger authoritative
revalidation instead of trusting a possibly older event projection.

## D-053 — Passive attachments, metadata, replies, and revisions

**Status:** Accepted; supersedes Briefcase-specific D-013/D-039/D-047 behavior

DM stores supplied HTTPS attachment references and voice metadata without
fetching, uploading, signing, or transforming media. The Briefcase temporary-URL
endpoint and dependency are removed. Every message and draft includes a JSON
metadata object. Metadata and same-conversation replies participate in content
hashes so retries and draft clearing cannot silently discard them.

Sender-authored edits append full replacement revisions under an exact version
precondition. Deletion appends a content-free tombstone. Original accepted
content remains durable; all authenticated clients receive increasing message
versions and reconcile by message ID. Sender devices receive message copies too,
while aggregate delivered/read state still concerns recipient actors.

## D-054 — Isolated testing data and runtime selection

**Status:** Accepted

A separate shared PostgreSQL database holds per-environment schemas. Every test
row carries its fixed environment UUID. Each pool resolves SQL only in its
selected schema. Production lifecycle records encrypt retrievable roots, IAM
roots, and test-only application credentials. Unknown/deleted keys never fall
back to production. Lifecycle fences, generation changes, and socket disconnects
prevent stale operations from crossing a cleanup or key rotation boundary.

The same HTTP/WebSocket routes serve both planes. CLI UUID selectors resolve
locally to secret roots. Ordinary test actions require the corresponding IAM
actor session; root-authorized environment operations remain explicitly scoped.

## D-055 — Stateless protocol client with optional shared local runtime

**Status:** Accepted

The default Rust client performs protocol operations without local storage or
background processes. Its optional `runtime` feature supplies explicit
caller-owned state directories, durable queues, local callback routing, token
refresh, daemon launch, and in-process hosting. The CLI calls this shared
implementation instead of maintaining its own relay. Multiple runtime instances
can coexist in one process without a global SQLite connection or state path.
Cancellation stops both worker tasks and existing local HTTP connections.

The optional runtime also supplies a default-on hourly update policy. Rust
applications own policy persistence and an explicit Cargo manifest to update
and rebuild after their command finishes. Running linked code changes only
after application restart. CLI executable updates keep their distinct install
policy and only replace the recognized Cargo-installed command.

## D-056 — Draft versions survive deletion

**Status:** Accepted concurrency correction

Per-participant draft counters remain after explicit deletion and automatic
send clearing. A recreated draft receives a greater version, so an old save
cannot overwrite a new draft through token reuse. Existing drafts are backfilled
under a writer lock; a draft deleted before this mechanism existed has no
recoverable historic version. Conflict snapshots are hydrated while holding
their transaction lock.

## D-057 — Delivery identity is stable while replay views may advance

**Status:** Accepted protocol clarification

Replay can hydrate current message state under a stable delivery ID. Local
deduplication validates its frame kind, actor, sequence, and immutable message
identity, retaining the first committed callback payload for that ID. It does
not require mutable content/status to remain identical. Revisions and receipts
also have their own durable delivery IDs; their callbacks progress the client
view without changing the meaning of an already acknowledged callback.

## D-058 — Profile transitions and archived callback completion

**Status:** Accepted concurrency correction

Login, refresh, and logout serialize on the same per-profile file lock, including
across CLI and relay processes. Refresh updates only credentials and expiry,
preserving concurrent callback settings. Logout checks the current family before
clearing it to protect direct store writers as well.

Sandbox generation adoption archives pending callbacks. An in-flight HTTP
completion may update only a still-pending row, in the same transaction that
queues any Delivered receipt. A late success or failure cannot revive archived
work. Callback requests already transmitted before adoption may still arrive.
