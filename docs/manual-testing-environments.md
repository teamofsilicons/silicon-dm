# Manual test-environment verification

Performed on 2026-09-05 UTC (2026-09-06 in Asia/Kolkata) against the running
local Silicon DM backend and the actual IAM testing service. These are observed
outcomes from individually chosen requests and interactive WebSocket sessions.
No automated test scenario runner was used. Compilation and lint checks were
performed separately.

The main local API ran at `http://127.0.0.1:18790`. Production authentication
was obtained for the registered `tos>dm` application. Test identities came
from the paired IAM testing environment. HTTP requests used a private utility
that issues one selected request per invocation. WebSocket checks used an
interactive program built with the public Rust DM client; frames and receipts
were sent manually, with an optional protocol-only ping responder.

## Fixtures and scope

The lifecycle exercise used DM environment
`01a072d4-d98e-7212-af08-59ced62c212f`. Its test actors were `dm-alice`,
`dm-bob`, and Silicon `dm-agent-a:tos`. This environment was created separately
from the main messaging exercise's environment. Root keys, IAM credentials,
tokens, pairing JSON, and raw responses were kept in private files outside the
repository. No secret values are included here.

The retention exercise used another environment,
`01a072d8-cce7-7192-8127-87ad3a58dfe8`. It was permanently purged. A separate
least-privilege exercise used disposable databases and a disposable database
role; all were removed after verification.

## Lifecycle and request isolation

| Manually selected action | Observed result |
| --- | --- |
| Read backend readiness | 204 with `Cache-Control: no-store` |
| Send malformed or unknown DM root key | 401; no production fallback |
| Send repeated root-key headers | 422 |
| Create a paired DM environment | 201 with environment metadata and a 32-character ASCII alphanumeric root key |
| Repeat the exact create with the original idempotency key | Same 201 response, environment UUID, and root key |
| Rename with a stable idempotency key | 200 with updated metadata |
| Reuse that key for a different rename body | 409 |
| Retrieve the current root key using production owner authentication | 200 |
| Rotate the root key | 200; old key immediately returned 401 |
| Repeat the exact rotation | Original replacement key and version, without another rotation |
| Clean with the matching root key and no actor token | 204 |
| Try to clean a different environment with this root key | 401; no mutation |
| Use a production actor token with a valid test root on a normal data route | 401 |
| Delete using production owner authentication | 204; previous root key returned 401 |
| Inspect the deleted environment | 200 with deleted status and 30-day recovery deadline |
| Retrieve a deleted environment's root key | 409 |
| Restore during retention | 200 with preserved environment UUID and a fresh root key |
| Repeat the exact restore | Original restored key and version |
| Replay the original successful delete after restoration | Original 204 response; restored environment remained active |

Initial, rotated, and restored root values were compared privately. They were
distinct, 32 characters long, and contained only ASCII letters and digits.

## Cleaning actual conversation data

Both Alice and Bob first called `/auth/me` in the selected environment. A real
conversation was then created through the normal API, and a message was
accepted with 202. Cleaning the environment returned 204; subsequently listing
conversations returned an empty list.

After clean, the actors authenticated again, a new conversation was created,
and a new message with identifiable text and metadata was sent. Repeating the
previous clean with its original idempotency key returned 204. Bob could still
read the new message with its text and metadata intact. This verifies that an
uncertain-response retry of an already completed clean does not delete data
created afterward.

These conversations were later intentionally cleared during the separate
WebSocket clean exercise below.

## Generation admission and WebSocket recovery

An HTTP message mutation carrying expected generation 5 against the active
generation 6 was rejected with 409 before creating a message. A request with
current generation 6 succeeded and showed only the existing message. A
generation header without a root key returned 422; a negative generation with
a valid root key also returned 422.

An Alice socket connected in generation 6, received delivery sequence 1, and
manually acknowledged it. Rotating the root key advanced the environment to
generation 7 and closed the existing socket with code 4001. Reconnecting the
same device with the replacement key and stale generation 6 returned a ready
frame with generation 7 and acknowledged sequence 0, then safely replayed the
existing delivery.

After acknowledging that delivery, a root-authorized clean advanced the
environment to generation 8 and closed the socket with code 4001 and reason
`testing-environment-changed`. Reconnecting the same device with stale
generation 7 returned generation 8 and acknowledged sequence 0. A newly
created conversation and message produced delivery sequence 1 on this socket,
despite sequence 1 having been acknowledged before clean. The new message's
metadata was retained. All interactive socket helper processes from this
exercise were closed afterward.

## Inactivity, retention, and permanent purge

To exercise real time-dependent code without waiting weeks, only the separate
retention fixture's timestamps were changed directly in PostgreSQL. Its
`last_activity_at` was moved 16 days into the past. The running backend's
actual maintenance pass soft-deleted it and assigned a 30-day recovery period.

Only that deleted fixture's timestamps were then moved beyond its recovery
deadline. A restore request returned 409 before the purge pass ran. After
maintenance, inspecting the environment returned 404. Read-only SQL confirmed
that its lifecycle record, mutation-journal entries, data schema, and helper
schema were all absent. No other environment's timestamps or data were
modified for these checks.

## Non-superuser database compatibility

A disposable PostgreSQL login was created with `NOSUPERUSER`, `NOCREATEDB`,
`NOCREATEROLE`, and `NOINHERIT`. An owner migrated a disposable production
database and applied `deploy/runtime-grants.sql` to that runtime role. In a
second disposable testing database, the runtime role received database CREATE
permission so it could create and own per-environment schemas. It did not
receive superuser authority.

A second DM API process used this role for both database connections. Its
readiness endpoint returned 204, and a real IAM-paired environment creation
returned 201. Using `SET ROLE` for a direct constraint probe, an insert carrying
a different `testing_environment_id` failed its check constraint. A valid
insert that omitted the field acquired the selected environment UUID by
default.

A root-key-only API clean returned 204 and removed the fixture row. A new row
inserted after clean survived an exact retry of that clean. Deletion returned
204. After aging only this fixture past retention, the actual runtime
maintenance process removed its control record, journal entries, and both
schemas. The second API process was stopped, and both disposable databases
and the runtime role were dropped successfully.

Direct SQL here was limited to the explicit database permission/association
checks and time manipulation; ordinary conversation/media/reply behavior was
exercised through the normal DM API.

## Message boundaries in the isolated environment

After the lifecycle checks, Alice and Bob used a new conversation in generation
8. Attachment URLs used an `.invalid` domain because DM stores passive
references; these checks do not represent uploading or downloading 5 GiB.

| Concrete request | Observed result |
| --- | --- |
| 100 generic attachment references | 202; all 100 preserved |
| 101 generic attachment references | 422 with the 100-item limit |
| 99 generic attachments plus one voice attachment and transcript | 202; all 100 total items, voice details, and transcript preserved |
| Voice in that accepted request: 5,368,709,120 bytes and 172,800,000 milliseconds | Exact 5 GiB and 48-hour metadata boundaries accepted |
| 100 generic attachments plus one voice attachment | 422; voice counts toward the same total of 100 |
| Empty JSON object | 422; a message requires content |
| Text with explicitly null metadata | 422; metadata must be an object |
| Transcript alone without voice | 422 |
| Text plus transcript without voice | 422 with `voice_transcript requires a voice attachment` |
| Reply pointing to an Alice/Silicon message from the Alice/Bob conversation | 404, even though Alice belongs to both conversations |
| Bob reads that Alice/Silicon message directly | 404 |
| Bob replies to a message in the same Alice/Bob conversation | 202 with reply reference and metadata preserved |

The final lifecycle fixture remains available with its generation 8 sample
conversations and messages. All obsolete root keys are revoked. The retention
and database-role fixtures were fully removed. Separate manual evidence for
the broader CLI, IAM callbacks, messaging revisions, and large text delivery
is recorded by the corresponding integration work.

## Packaged runtime

The actual release Docker image was built and its `dm-migrate`, `dm-api`, and
`dm-worker` binaries run against a disposable database. The migration journal
contained successful versions 1–8 and 10–14. Both API probes (`/live`, `/ready`)
returned 204; the worker reported ready. The API process ran as UID/GID 10001.
A deliberately invalid testing-generation header returned 422 with
`Cache-Control: no-store`, confirming that early dispatch failures receive the
same cache protection as normal responses. An incorrect `/health/ready` path
returned the expected JSON 404; the documented probe is `/ready`.
After the draft migration's final lock correction, the image was rebuilt and
the migrator was run against another completely empty disposable database.
All 13 migrations through version 14 succeeded, and this final image's API
and worker both started successfully against that fresh schema.
Both disposable container databases and the packaged API/worker containers
were removed afterward. The main manual messaging fixtures remain available.
