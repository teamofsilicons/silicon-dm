# Manual backend and Rust-client verification

This record covers individually chosen operations performed on 6 September 2026
(Asia/Kolkata). No automated scenario suite was run. A fresh local PostgreSQL
database, separate testing database, actual IAM test environment, two test
Carbons and two test Silicons were used. The pre-existing database was preserved.
Credentials and raw callback captures are private runtime files, not repository
fixtures.

The public stateless Rust client was also built in an independent consumer
outside this workspace. Its interactive terminal accepted manually entered
WebSocket frames; it did not execute scripted test scenarios.

## Observed behavior

| Manual operation | Observed result |
| --- | --- |
| Fresh migration and `/live`, `/ready` | Migrations committed; both probes returned 204 |
| Existing database with incompatible historical migration checksum | Migration refused; existing data was not altered |
| Real IAM SLT exchange for both Carbons and both Silicons | 200 with correct actor type, public ID and organization |
| Refresh a Silicon application session, then retry the identical key/body | 200; both token responses exactly matched |
| Revoke that refresh family, retry logout, then use either access token | Both logout calls returned 204; both access tokens returned 401 |
| Carbon–Carbon and Silicon–Silicon conversation creation | 201; exact authorized participant sets returned |
| Unicode text, nested metadata containing false/zero/null/arrays | 202; content preserved in HTTP and WebSocket responses |
| Exact message retry, then changed content under the same key | Same ID and sequence returned; changed body returned 409 |
| Text + attachment + voice + transcript + GIF + reply + metadata | 202; all fields preserved |
| Attachment declared at 5 GiB and voice declared at 48 hours | Accepted as supplied metadata; no upload or remote file fetch occurred |
| Attachment one byte over 5 GiB; voice one millisecond over 48 hours | Each returned 422 |
| Text exactly 100,000,000 ASCII characters | 202 after 11.425 seconds in this local debug run; response contained all characters |
| Receive that message through the independently built Rust WebSocket client | Complete 100,000,000-byte/character text received, with metadata |
| Receive the large sender copy through the CLI daemon | SQLite retained a 100,000,496-byte frame; local callback acknowledged it on its first attempt |
| Text 100,000,001 characters | 422 with the logical character-limit error |
| 34,000,000 four-byte Unicode characters (136,000,011 encoded request bytes) under the default 128 MiB body cap | 413 with a body-limit error; logical character and encoded-byte limits are distinct |
| Read another conversation as a nonparticipant | 404 |
| Supply another actor as sender | 403 |
| Empty participants, missing idempotency key, invalid cursor, zero page limit | Stable 422 errors |
| Edit another actor's message | 403 |
| Author edit at the observed version | 200; same message ID, incremented version, replacement metadata and reply preserved |
| Stale edit version; reply to the message itself | 409 and 422 respectively |
| Delete an authored message | 200 content-free tombstone; version advanced; metadata remained an object |
| Replay deletion with its original key/version | Same tombstone returned |
| Try to edit the tombstone | 409; no resurrection |
| Directly update a stored revision | PostgreSQL rejected the mutation; original revision remained intact |
| Insert a revision without complete delivery fan-out | Deferred database constraint rejected the transaction; a subsequent count confirmed no revision committed |

## Draft concurrency and packaging fixes

An explicit delete/recreate exercise exposed reuse of draft version 1: an old
save could overwrite a newly created draft. Migration 0014 adds a per-participant
version counter retained after deletion and successful-send clearing. It locks
draft writers before backfilling existing versions. A conflict response now
hydrates its snapshot while still holding the transaction lock, preventing a
concurrent delete from changing a conflict into a not-found response.

After the fix, deleting version 1 and recreating returned version 3; a stale
version-1 save returned 409 with unchanged content. Sending exactly that draft
cleared it. Recreating then returned version 5; stale version 3 returned 409,
and a save using version 5 correctly returned version 6. Gaps are intentional.
For an upgrade from an older deployment, clients must discard pre-upgrade draft
tokens: a draft deleted before this counter existed has no historical version
left to backfill.

Adding the migration also exposed Cargo reusing embedded SQLx migration metadata
after a new SQL file was added. The package build script now watches the entire
migrations directory, and Docker includes that script. The rebuilt container's
migration journal was inspected directly and included successful version 14.
The lock-only correction to unpublished migration 14 was aligned only in the
explicitly owned local fixture journals; the pre-existing user database and its
historical checksums were not changed.

## Final static checks

The final source passed `cargo build --locked --workspace --bins`,
`cargo clippy --locked --workspace --all-targets --all-features -- -D warnings`,
`cargo fmt --all --check`, and `git diff --check` for the implementation files.
Redocly 2.49.0 validated the OpenAPI document with the four narrowly documented
ignores. `cargo deny check` passed advisories, bans, licenses, and sources,
with duplicate-dependency warnings. `cargo package --workspace --allow-dirty
--no-verify --locked` produced local archives for the backend, public client,
and CLI; this did not publish them or run tests. Archive inspection found no
private runtime files or registered production secret values.

## Realtime and acknowledgements

The initial heartbeat check exposed a PostgreSQL syntax error: `current_time`
had been used as an unquoted CTE name. Renaming it to `heartbeat_clock` fixed the
first-heartbeat failure.

| Manual operation | Observed result |
| --- | --- |
| Connect without answering application pings | Pings near 30, 60 and 90 seconds; close code 4000 and `heartbeat-timeout` at 120.56 seconds |
| Connect with matching application pongs | Connection remained active beyond 180 seconds and carried message revisions and receipts |
| Disconnect before ACK, reconnect using the same device | Same unacknowledged delivery IDs and sequence positions replayed |
| ACK a sequence never emitted on that connection | Recoverable validation error; connection stayed open |
| ACK through sequence 5, close, reconnect with the same device and observed test generation | Ready frame reported durable cursor 5; acknowledged messages were not replayed |
| Send a message using an interactive Rust WebSocket frame | `message_accepted` returned the original idempotency key and preserved content; sender copy then arrived |
| Recipient local callback acknowledges the message | Sender received a durable Delivered receipt event |
| Submit Read before Delivered for another message | Read succeeded and set both timestamps |
| Submit Delivered after Read | Status stayed Read; timestamps stayed unchanged |

ACKs concern durable transport positions. Read/Delivered receipts concern user
message state. The checks exercised those separately.

## Related evidence and remaining external checks

### Optional SDK runtime completion

A final scope review found that exposing only protocol methods left relay
hosting and dependency update policy to Rust consumers. Those components now
live in the SDK's optional `runtime` feature. The CLI uses the same implementation;
the default SDK remains stateless. A separate Rust consumer outside the
workspace exercised these APIs through individually selected commands:

| Manual operation | Observed result |
| --- | --- |
| Build the default SDK with no runtime feature | Passed; no local runtime is required by the protocol client |
| Obtain a fresh SLT using the installed IAM CLI, then call SDK `LocalRuntime::login` | Correct Carbon identity saved with local callback; detached `dm-relay` started and connected |
| Submit SDK JSON with an extra nested `caller_context` | Durable ACK and completed result preserved the exact request |
| Send from that SDK runtime to Bob | One message accepted; recipient callback advanced it to Delivered |
| Host two runtimes in one Rust process using separate directories/ports | Same host PID, independent listeners and queues; stopping one left the other available |
| Cancel an embedded runtime future while its authenticated HTTP keep-alive connection was open | Existing connection closed with EOF while the embedding process stayed alive; another relay immediately acquired the same directory and port |
| Start a different runtime on an occupied port | Typed startup error before readiness; the original listener remained healthy |
| Default SDK update policy, opt out, then invoke after-command update | Enabled by default; opt-out skipped the registry without advancing the timestamp |
| Opt in and invoke after-command update against crates.io | Actual registry failure for the unpublished package; attempted-check timestamp retained |
| Invoke after-command update again within the hour | Skipped; timestamp unchanged |
| Age the caller-owned last-check timestamp by 3,601 seconds and invoke again | Actual registry check attempted again; attempt timestamp advanced despite unpublished-package 404 |
| Restart the existing CLI on the shared SDK runtime, then run `whoami` | Existing profiles/queues preserved; real IAM identity returned through the relay |

The shutdown exercise led to explicit propagation of cancellation to Axum's
owned connection tasks and cancellation guards for child workers. SDK-launched
daemon children are also reaped; an early process exit reports a startup error.

The replay audit found another mismatch: the backend may hydrate a newer
message version or status under an older delivery ID. The relay previously
required byte-equivalent parsed content for duplicate IDs. It now verifies
immutable identity, retains the already committed callback payload, and accepts
new revision/receipt deliveries separately. For a real replay exercise, the SDK
relay was stopped, its message edited through the API, and only its owned fixture
cursor was rewound to zero. On restart it remained connected and advanced to
sequence 69. The original delivery at sequence 67 retained version 1, and the
distinct revision delivery at sequence 69 carried version 2; both callbacks
completed. The inbox was not cleared or rewritten for this check.

For the final SDK durability check, the API was killed, a new request received
the exact durable local ACK, and the SDK daemon was then killed. Read-only SQLite
inspection confirmed request `23232323-7777-4777-8777-232323232323` remained
pending with its original key and generation 1. After both processes restarted,
that same request completed; PostgreSQL contained exactly one corresponding
message (`01a07315-f6e3-75b1-a319-32a82e81a048`), and its recipient callback
advanced it to Delivered. No automated scenario suite was used.

### Runtime concurrency checks

A bounded agent review identified three races, all fixed and exercised manually
against the real backend and IAM. A temporary local HTTP forwarding gate delayed
individual requests; it did not fabricate server responses.

- Held logout before forwarding, then started login for the same profile with a
  fresh IAM SLT. Login waited for the profile lock. After the gate was released,
  logout completed first and login committed a usable new family. The profile
  stayed enabled with its original device ID; a subsequent refresh succeeded.
- Held refresh before forwarding, changed the profile callback URL with the CLI,
  then released the request. Refreshed tokens were saved and the new callback URL
  remained. Refresh no longer replaces the whole profile snapshot.
- In a disposable sandbox, delayed a real recipient callback by twelve seconds
  and cleaned the environment while HTTP was in flight. After a 503 callback
  response, the old-generation inbox row remained archived (`delivered=-1`), with
  zero new attempts and zero outbox receipts.
- Repeated with a correct successful callback ACK. The recipient accepted the
  already transmitted event, but the old row stayed archived after generation
  adoption and no stale Delivered receipt was queued. Both archived records
  remained unchanged at generation 3.

Callback completion now checks pending state transactionally before enqueueing
receipts; dispatch also rechecks that the selected work is still pending. Login,
refresh, and logout share a cross-process profile lock. Logout additionally
compares the family before clearing credentials if a direct store writer raced it.
The disposable sandbox was soft-deleted and its runtime and forwarding gate
were stopped after verification. The main test history was preserved.

### Other evidence

- [CLI verification](cli/manual-verification.md) records command, callback,
  draft, bundle, queue and process-recovery checks.
- [IAM verification](manual-iam-verification.md) records real signed callback
  delivery, tampering, replay and authentication boundaries.
- [Testing-environment verification](manual-testing-environments.md) records
  lifecycle, separate-database isolation, restricted runtime roles and cleanup.

Before the application key was supplied, the actual upstream returned 401 and DM
returned the documented dependency-unavailable 503. An empty query returned 422;
recent GIF storage uses DM's database independently of that provider request.

After configuring the supplied real key and restarting the local API, separate
CLI trending and `cats waving` search requests each returned 25 GIFs. Search
results had HTTPS content and preview URLs; both endpoints returned a null next
cursor. The selected real GIF (`ToMjGpRZ4gF6YuAT4li`) was sent from Alice to Bob,
with its discovery metadata unchanged and custom message metadata preserved.
Message `01a07335-a8df-7743-8c98-b240d3732a97` reached Delivered after the actual
recipient callback. Recent GIFs still contained exactly 20 entries, with that
real provider ID first. These were individual CLI operations through the public
Rust client, using the real Giphy service.

A further real-provider search containing Unicode, spaces, an ampersand, slash,
question mark, and emoji succeeded through the CLI. Exactly 50 Unicode
characters also succeeded; 51 returned `422 validation_error`, a terminal
failed request rather than an automatic retry. These checks exercised URL
encoding and the documented query boundary.

This section records local integration evidence, not a proof against every
possible disk, network or infrastructure failure. Subsequent package publication
and registry installation are recorded in the CLI verification guide.

The earlier release-prerequisite audit on 6 September 2026 (Asia/Kolkata) confirmed that
the configured public hostname did not resolve, so public `/live` and `/ready`
could not be reached. The checkout supplies a deployment runbook and local
PostgreSQL Compose service, but no selected production host. Both private
configuration files retained the supplied IAM credentials with mode 0600;
neither contained a usable Giphy key. Production hosting/DNS configuration,
Giphy success, registry publication, installed updater verification, and the
production webhook exercise were open release gates at that point. The user
subsequently supplied the Giphy and registry credentials and selected AWS with
Namecheap DNS; later verification records supersede those initial blockers.

## AWS deployment and large-message recovery — 2026-09-06

The private ARM64 Fargate deployment uses separate encrypted, TLS-verified RDS
production/testing instances, an HTTPS ALB, its own WAF, and restricted runtime
roles. CloudFormation initially attempted EC2, but the regional vCPU quota was
full; the retained databases/ALB were imported into the Fargate stack. RDS
bootstrap initially failed on explicit restricted-role attribute alterations;
it was corrected to validate existing attributes and change only the password
transactionally. The corrected bootstrap completed with exit 0.

Manual public HTTPS checks returned 204 for live/readiness, 404 for the absent
OBO route, and 401 for unauthenticated access with a valid organization header.
Production owner login and paired test Alice/Bob logins succeeded. A real
message progressed from Sent to callback-confirmed Delivered and explicit Read.
Giphy search returned provider results. A combined message accepted text, a
declared 5 GiB passive attachment URL, 48-hour voice metadata/transcript, a real
Giphy item, reply linkage, and nested metadata. These are link/metadata limits;
DM did not upload 5 GiB or transcribe 48 hours of audio.

A 100,000,000-character ASCII message committed to RDS and reached the receiver,
but the initial 4 GiB API task was killed for out-of-memory before completing
the sender response. The client kept the original idempotent request pending.
Inspection identified payload-bearing retry queues, batch hydration and
retained SQLx buffers; client polling also repeatedly transferred full requests.
The corrective build queues only delivery positions, hydrates replay
incrementally, bounds history pages, trims released database buffers, and
removes redundant payload copies. The API allocation is now 2 vCPU/8 GiB.
The SDK/CLI uses metadata-first byte-bounded scheduling and small status polls.
The preserved request is the recovery fixture; successful recovery must be
verified separately before declaring this failure resolved.


### Manual verification after the large-message memory fix

On 6 September 2026, the rebuilt debug backend was run separately on loopback
port 18792, using the existing `silicon_dm_manual` and `silicon_dm_testing`
databases on local PostgreSQL port 55450. Each operation below was an individually
selected HTTP request; no automated scenario runner or test suite was used.
Fresh SLTs were obtained through the installed IAM CLI. The existing IAM
management profile was used only to create isolated test actors
`dm-memory-a:tos`, `dm-memory-b:tos`, and `dm-memory-c:tos`; application logins
used copied private credential homes. Existing local listeners were not used
for the new large-message fixtures.

- Existing history requested with `limit=100` returned sequences 7 through 4,
  then the complete 100,000,000-character message at sequence 3, then sequences
  2 and 1. Following the returned cursors produced no duplicates or omissions;
  the final cursor was null. The large page returned HTTP 200 with 100,000,519
  response bytes.
- Three separate 100,000,000-character message submissions to the isolated
  actor conversations returned HTTP 202 with complete message bodies. Local
  debug-build request times were 10.829–11.334 seconds.
- A conversation containing two large originals returned one message per page
  even with `limit=100`, preserving descending sequence order and a null cursor
  after the second page. A conversation list whose two latest messages were
  each 100,000,000 characters likewise returned one conversation per page,
  preserving both IDs and returning a null final cursor.
- Creating a bundle from two 100,000,000-character originals returned HTTP 201.
  Expanding it returned HTTP 413 `response_too_large` in 0.276 seconds, with
  guidance to retrieve originals individually. Requesting that bundle under a
  different conversation in which the same caller was a participant returned
  HTTP 404, before expansion-size reporting.
- A bundle containing one 100,000,000-character original and a 38-character
  original expanded with HTTP 200: 100,001,698 response bytes in 4.958 seconds.
  Both original IDs remained in order and both text lengths were intact.
- Retrieving an individual original from the rejected expansion returned HTTP
  200 with its complete 100,000,000-character text and bundle-member reference.
  Its response was 100,000,447 bytes.

The isolated API remained alive and `/ready` returned HTTP 204 after these
requests. One final RSS sample was approximately 529 MiB; this was an observed
point, not a measured peak or a concurrency-capacity claim. The isolated API
was then shut down gracefully. Existing histories and testing keys were not
cleared or rotated; only the new controlled conversations were bundled. Private
request/response evidence uses the `memory-` filename prefix beneath the local
manual-verification directory. This verifies local pagination and expansion
behavior; public ingress, WebSocket retry pressure, and deployed capacity are
covered by the separate production verification work.

### Observed cloud recovery result

The original queued request completed against the corrected deployment with the
same message ID and idempotency key. Full CLI readback matched all 100,000,000
characters exactly (SHA-256
`4a1208e65257e3b9e3c7d4fca19c2b3e886feef8182a3b6532c116a363f99de4`),
and the durable message status was Delivered. All three profiles reconnected;
request and callback queues reached zero with no failed requests. Giphy
production trending separately returned 25 items.

The Linux test daemon twice exited with SIGBUS, with zero cgroup OOM events,
while its SQLite WAL files were on a macOS-shared Docker bind mount and were
also inspected from macOS. A consistent backup of the preserved queue was moved
to a native Docker volume; subsequent inspection stayed inside Linux. Recovery
and full readback then succeeded. Shared-memory/locking across the two kernels
is a plausible harness cause, not a proven core-dump diagnosis. The original
files and the recovered native-volume state are retained.

The final client build restarted against the same native queue, reconnected all
profiles, and explicitly marked the large message Read. Giphy recent returned
the sent item. Final queue counts were zero pending/failed requests and zero
pending callbacks. Public readiness returned 204 with successful TLS verification.

CloudFormation finished UPDATE_COMPLETE. API/worker each had one running task,
zero pending tasks, and completed deployments. Drift detection found only the
PostgreSQL parameter group's provider representation (`{}` versus `null`);
`rds.force_ssl=1` remained the effective system default. No other resource
drift was reported. Temporary privileged inspection task definitions and the
manual bootstrap revision were deregistered; managed bootstrap revision 3 remains.


### Read-only production deployment audit after the memory fix

The final AWS audit on 6 September 2026 (Asia/Kolkata) found
`silicon-dm-production` in `UPDATE_COMPLETE`, with the bootstrap task definition
restored at revision 3. API revision 2 and worker revision 2 each had one running
task, zero pending tasks, and a completed rollout. Both used runtime image digest
`sha256:b6fe50010c38a2d93458f45dd880d215fedee0b34cc4e8ce7af45dece34a04ea`.
The API had 2 vCPU/8 GiB and the worker 0.5 vCPU/4 GiB. Runtime containers used
UID 10001 and a read-only root filesystem, with no application task IAM role.
The execution role could read only the restricted runtime secret. Public HTTPS
`/live` and `/ready` each returned 204, and the ALB target was healthy.

The dedicated WAF remained attached with its IP-only limit of 2,000 requests per
300 seconds, without message-body content filtering or request sampling. ALB
idle timeout was 180 seconds. Both PostgreSQL 17.9 instances were available,
private, encrypted, and deletion-protected; PostgreSQL ingress was limited to
the DM task security group. The live parameter metadata reported
`rds.force_ssl=1`. The successful bootstrap log records authenticated TLS checks
and restricted production/testing role configuration; this audit did not read
secret values or issue fresh SQL role queries.

CloudFormation drift detection completed and reported exactly one difference:
`DatabaseParameters` had expected `/Parameters={}` versus provider-reported
`null`. No other drift was reported. The live TLS-enforcement parameter was
checked separately and remained enabled.

For the UTC interval 5 September 21:16 through 21:22, CloudWatch returned six
one-minute ECS memory samples. The API maxima were respectively 3.003%, 2.417%,
1.599%, 3.003%, 2.686%, and 3.210% of 8 GiB; worker maxima were 0.07324% of 4 GiB
throughout. These are sampled service metrics, not an instantaneous RSS peak or
a general concurrency-capacity guarantee. Both services remained at one running
task with completed rollouts at the final read. The audit changed no deployment
resources, credentials, or secret values.
