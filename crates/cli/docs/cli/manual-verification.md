# Manual verification record

These are individual command checks performed during implementation on
2026-09-06. No automated scenario test or test harness was added. The build was
validated with `cargo check -p silicon-dm-client`,
`cargo check -p silicon-dm-cli` and `cargo build -p silicon-dm-cli`.

## Offline checks

An explicitly isolated `SILICON_DM_HOME` directory was used; normal user profiles
were not changed. Automatic updates were disabled before offline commands.

| Individual check | Observed result |
| --- | --- |
| `dm --help` | Lists every family and global profile/test/idempotency/JSON/wait flags |
| `dm messages send --help` | Examples for text, attachment-only, metadata, replies and JSON media |
| `dm messages edit --help` | Requires observed version and explains full replacement/conflicts |
| `dm drafts put --help` | Explains version-zero creation and conflict-body recovery |
| `dm relay submit --help` | Shows typed JSON, queue-ACK semantics and result lookup |
| `dm updates disable`, then `updates status` | Opt-out persisted; no registry check; hourly interval and development-build status shown |
| `dm profiles list` | Empty profiles and default name returned without secrets |
| `dm docs` | Guide locations and callback acknowledgement shape returned |
| `dm environments clean` without `--test` | Rejected before network with required command syntax |
| File permissions | State directory 0700; config, lock, SQLite and daemon log 0600 |
| `dm daemon start`, separate `daemon status`, `daemon stop`, `daemon status` | Detached daemon survived the launching command, same PID reported, clean stop then `running:false` |

Two problems found during manual checks were fixed: stopped status originally
returned a raw connection error, and the initial spawned daemon inherited the
shell session. Status now provides a useful stopped state; Unix daemon launch
uses `setsid` with redirected standard streams. A deliberately selected occupied
port failed to bind; a free isolated port succeeded without disturbing its owner.

## Live checks

An existing real IAM test login was used with a locally running DM backend and
separate live CLI state directory. Credentials remained in private runtime
files and are not included here.

| Individual check | Observed result |
| --- | --- |
| `profiles list` | Correct Carbon actor, organization, test UUID and local callback mapping; no tokens |
| `daemon start`, separate `daemon status` | Profile connected on persistent WebSocket after parent command exit |
| `whoami` through selected test profile | Durable exact-request acknowledgement followed by real IAM identity, org role and capabilities |
| `conversations list --limit 1` | Successful empty-page result with `next_cursor:null` in a new sandbox |
| `presence get`, `presence set typing`, `presence get`, `presence set clear` | Online state, durable request ACK, server-observed typing state, then clear accepted |
| `gifs recent` | Empty list returned successfully for the new Carbon profile |
| Restart with two already pending requests | Both original request IDs recovered and completed after restarting the daemon |

Live concurrency exposed a bundled SQLite Unix WAL mutex deadlock while separate
connections were opened/closed concurrently. A sampled daemon stack identified
the blocked SQLite routines. Queue access now uses one process-local connection
guarded for short transactions, with no guard held across an async wait. The
restarted daemon recovered both pending requests from its existing database.
Refresh-file lock acquisition was also made asynchronous to avoid blocking a
runtime worker while another task refreshes the same profile. Clippy with
`-D warnings` passes for both new crates.

## Complete leaf-command coverage

Every leaf command in the current Clap command tree was invoked individually.
`env` is the visible alias for `environments`. A separate CLI-created sandbox was
used for environment lifecycle commands; the primary messaging sandbox was not
cleaned, deleted or rotated. Login/refresh/logout used a fresh independent
Silicon token family, leaving the active Carbon sessions intact.

| Command | Actual result |
| --- | --- |
| `login` | Fresh real IAM SLT exchanged through DM; actor/org saved privately; local callback mapped and daemon started |
| `refresh` | New access/refresh tokens saved atomically; same Silicon identity |
| `logout` | New family revoked; profile disabled and pending work retained |
| `whoami` | Real IAM Carbon identity, owner role and capabilities returned through relay |
| `profiles list` | Carbon, Silicon, production and test mappings listed without tokens; logged-out profile shown disabled |
| `profiles use alice` | Default profile persisted as Alice |
| `profiles webhook URL` | Selected actor's callback mapping updated locally |
| `conversations create` | Alice/Silicon participant conversation created |
| `conversations list` | Empty new-sandbox page and existing conversation page returned |
| `messages send` | Text, arbitrary nested metadata, reply reference, attachment-only and combined voice/transcript/GIF/attachment content preserved |
| `messages list` | Limit-one cursor advanced from sequence 5 to 4; bundle views collapsed and expanded correctly |
| `messages show` | Correct message content, Delivered then Read status, and revision returned |
| `messages edit` | Same message ID advanced from version 1 to 2; stale version rejected with structured 409 and exit 1 |
| `messages delete` | Versioned tombstone returned with deleted_at and cleared content |
| `receipts delivered` | Explicit receipt accepted; an already-Read message stayed Read |
| `receipts read` | Explicit recipient action advanced Delivered to Read; callback alone had not marked Read |
| `drafts put` | Version-zero creation, version-one replacement; stale save returned 409 including current draft |
| `drafts get` | Stored draft/metadata returned; matching message send cleared it and subsequent fetch returned 404 |
| `drafts delete` | Explicit deletion returned deleted=true |
| `bundles create` | Silicon summarized two Carbon messages with summary metadata |
| `bundles show` | Display message and both unchanged originals returned |
| `presence get` | Actual online and transient typing state returned |
| `presence set` | Typing and clear accepted through active WebSocket |
| `gifs trending` | Structured 503 dependency_unavailable; Giphy service was unavailable in this local setup |
| `gifs search` | Same explicit dependency error; the following receipt command completed normally |
| `gifs recent` | After 21 distinct individually entered Carbon GIF sends, exactly the latest 20 returned newest-first; Silicon returned an empty list |
| `env create` | Independent sandbox created; returned root key saved privately and omitted from normal output |
| `env list --include-deleted` | Organization environments listed without secrets |
| `env show` | Selected environment metadata returned |
| `env update` | Name and description updated through JSON stdin |
| `env key` | Key retrieved and stored without printing it |
| `env rotate-key` | New key saved privately |
| `env clean` | Selected independent sandbox cleaned using its local key |
| `env delete` | Soft deletion succeeded; response stated 30-day recovery |
| `env restore` | Environment restored with saved replacement key and increased generation |
| `env import-key` | Private key file imported into separate local state directory |
| `daemon start` | Detached process survived the launching command; persisted profiles reconnected |
| `daemon run` | Foreground listener started on isolated port and remained active until stop |
| `daemon status` | Running PID/profile states/queue counts returned without secrets; stopped state useful |
| `daemon stop` | Both foreground and detached daemon stopped through the local API |
| `relay submit` | Entire JSON including caller_context echoed after durable commit; identical request replay accepted; changed content with same request_id rejected 409 |
| `relay result` | Original caller_context and completed result returned under the same request ID |
| `relay credentials` | Correct dm.localhost and numeric loopback URLs; private bearer present and redacted before inspection |
| `updates enable` | Persisted enabled; subsequent ordinary command succeeded despite unavailable package |
| `updates disable` | Persisted opt-out |
| `updates status` | Hourly interval, version, last check and development-build replacement restriction shown |
| `updates check` | Explicit crates.io 404 because silicon-dm-cli is not published |
| `updates install` | Explicit crates.io 404 before installation; no installed executable was changed |
| `docs` | Client/CLI/API/test documentation and callback ACK shape returned |

The initial mixed-media GIF plus 20 later separately entered GIF sends exercised
retention. Recent results contained `manual-finish` first, `manual-wave` last,
and excluded the original `manual-gif`. The commands used distinct explicit
idempotency keys; no generated command loop or scenario harness was used.

## Reliability observations

- Repeating a text send with the original idempotency key returned the same
  message ID and sequence, with no duplicate message.
- Recipient callback acceptance automatically queued Delivered, while sender
  copies and explicit Read remained separate.
- The relay stored a 100,000,496-byte event containing the 100-million-character
  message in SQLite. Its callback completed in one attempt; only lengths and
  IDs were inspected. The same inbox survived a subsequent daemon restart.
- A queued recipient receipt had expected_generation=1 stored independently of
  its original request JSON and completed against the matching sandbox.
- Parent-agent manual callback check: HTTP 200 with the wrong delivery ID kept
  the callback pending through five attempts and the message Sent. Restoring
  the correct ACK completed that retained event.
- Parent-agent slow-callback check: Bob's callback was delayed 12 seconds. A
  separate Silicon callback reached Delivered about 630 ms after creation while
  Bob remained Sent. Callback endpoints were then restored to normal.
- With the backend forcibly stopped, a new send returned a durable local ACK
  under request ID `7355dacd-34ad-48c0-892f-e047a8e3d128`. The daemon was then
  killed with SIGKILL. Read-only SQLite inspection found the original pending
  request, idempotency key, and expected test generation intact. After restarting
  the API and daemon, that same request completed and reached Delivered. Direct
  database inspection found exactly one corresponding message, with ID
  `01a072fd-9cba-7761-9b4f-940b161f26a4`. Session refresh and test-generation
  admission recovered without replacing or duplicating the queued request.
- Final daemon inspection showed all four enabled profiles connected and zero
  pending requests. Three retained callback events belonged to the deliberately
  logged-out `leaf-auth` profile; they had not been discarded or delivered after
  logout. Six failed request records were the earlier deliberate invalid-command
  and dependency-failure checks.

Read dependency failures originally retried indefinitely and could hold later
writes. Reads now finish with structured errors; retryable writes remain
durable. Requests and callbacks now use separate bounded worker pools, one
in-flight item per profile, so slow profiles do not delay unrelated profiles.

These initial checks preceded registry publication. Successful Giphy
trending/search results could not be checked while the configured provider was
unavailable. The later release checks below supersede the initial registry
limitation; these earlier provider failures remain recorded as observed.

## Installed offline documentation

The rebuilt `dm` executable was invoked manually from `/tmp`, with
`SILICON_DM_HOME=/dev/null` so a credential or state-directory lookup would
fail. No repository path or login was needed by the commands.

| Command | Actual result |
| --- | --- |
| `dm docs --help` | Listed all guide topics, topic descriptions, search, full export, and Markdown extraction examples |
| `dm --json docs` | Returned the topic index and existing guide/ACK fields as compact JSON |
| `dm --json docs runtime` | Returned the full optional SDK runtime guide, including login, hosted relay lifecycle, and update policy |
| `dm --json docs openapi` | Returned the complete YAML contract in the JSON content field |
| `dm docs --search acknowledged` | Returned matching guides with one-based line numbers and excerpts |
| `dm --json docs --all` | Returned all 15 complete guide documents; each content value matched its canonical source file |
| `dm docs --search '   '` | Exited 1 with a structured error explaining that search requires text |
| `dm docs not-a-guide` | Exited 2 with the accepted topics and help hint |

`cargo build -p silicon-dm-cli --bin dm --locked` and
`cargo clippy -p silicon-dm-cli --all-targets -- -D warnings` passed.
`cargo package -p silicon-dm-cli --allow-dirty --locked --list` included
the documentation module, all 15 Markdown files, and the OpenAPI file inside
the CLI crate. Every `include_str!` target resides within that package.
The canonical-to-package synchronization command is
`python3 scripts/sync-cli-docs.py`; run it after guide changes and before
building a release. These were direct command inspections, not automated
test scenarios.

## Remaining presence values and explicit key output

A final command-tree audit found these option/value branches were implemented
but not explicitly recorded. Each was then invoked manually against the live
backend:

| Individual command | Observed result |
| --- | --- |
| `presence set recording-voice`, then `presence get dm-alice` | Completed; backend returned `recording_voice` and online availability |
| `presence set transcribing-voice`, then presence get | Completed; backend returned `transcribing_voice` |
| `presence set uploading-file`, then presence get | Completed; backend returned `uploading_file` |
| `presence set searching-gifs`, then presence get | Completed; backend returned `searching_gifs` |
| `presence set clear`, then presence get | Completed; backend returned no activity |
| `env key ID --show` on the disposable sandbox | Exit 0; captured secret output matched the private stored key, length 32 |
| `env rotate-key ID --show` on that sandbox | Exit 0; captured new key matched the saved replacement, differed from the old key, and contained 32 ASCII alphanumeric characters |

Secret output was captured privately and compared without including either key
in the transcript or documentation. The disposable sandbox was restored only
for these checks and soft-deleted afterward. Primary messaging data and keys
were unchanged.

## Published registry packages

On 2026-09-05 UTC, `silicon-dm-client` and `silicon-dm-cli` version 0.2.0 were
published to crates.io in that order. The backend remained unpublished. Both
retained their existing `LicenseRef-Proprietary` metadata, which the registry
accepted. Registry checksums matched the exact uploaded archives. The client
archive was verified with both default and optional runtime features; CLI
package verification downloaded the published client dependency.

The following commands were selected and invoked manually from `/tmp`, using
separate Cargo installation and DM state directories. Existing actor profiles,
tokens, daemons, and the user's normal Cargo binary directory were untouched.

| Individual check | Observed result |
| --- | --- |
| `cargo install silicon-dm-cli --version 0.2.0 --locked --root PRIVATE_DIRECTORY` | Downloaded both released DM crates and installed an optimized `dm` executable |
| Registry-installed `dm --version` | Reported `dm 0.2.0` |
| Registry-installed `dm docs runtime`, with unusable state-directory path | Returned the complete embedded runtime guide without a checkout or credentials |
| `dm updates status` in fresh private state | Enabled by default; hourly interval; Cargo-bin replacement supported |
| `dm profiles list`, then update status | Empty profile result completed; automatic registry check recorded its timestamp afterward |
| Repeat profiles list within the hour | Command succeeded; update timestamp stayed unchanged |
| `dm updates check` | Current and latest CLI versions both 0.2.0 |
| `dm updates disable`, ordinary command, then enable | Opt-out and opt-in persisted; ordinary command remained usable |
| `dm updates install` | Cargo downloaded and rebuilt the released package, replaced the isolated installed binary, and returned `installed:true` |
| Build a separate Rust consumer using only the crates.io SDK dependency | Compiled the released SDK with the optional runtime feature |
| SDK `check_update()` | Current and latest SDK versions both 0.2.0 |
| SDK after-command update with default policy | Checked crates.io successfully and reported no newer version |
| Repeat SDK after-command update within the hour; repeat after disabling | Both skipped; the timestamp remained unchanged |

The 0.2.0 CLI installation was then kept with automatic updates disabled, and
the SDK consumer retained its 0.2.0 lockfile, for a later real upgrade check.
No automated test scenario or `cargo test` was run.

## Actual upgrade from 0.2.0 to 0.2.1

The documentation patch was published on 2026-09-05 UTC, client first and CLI
second. Backend version 0.2.0 remained unchanged and unpublished. The archive
changes contained release metadata, README updates, and refreshed operating and
verification guides; runtime source files were unchanged. The bundled AWS guide
describes the EC2 deployment option and its acceptance procedure, rather than
recording a completed cloud deployment.

`cargo check -p silicon-dm-client -p silicon-dm-cli --all-targets` passed.
`cargo package` verified the client with `--features runtime` and verified the
CLI against the newly published client. The 14-file client archive and 26-file
CLI archive were checked against 43 current private credential values, including
the supplied registry and provider credentials; no values or private runtime
paths appeared. Both kept `LicenseRef-Proprietary`. The exact uploaded SHA-256
checksums matched the crates.io sparse index:

| Package | SHA-256 |
| --- | --- |
| `silicon-dm-client 0.2.1` | `86b2484003ade63c5cf76e2c1701ee5e50f5a992c7ca092cec26c763a3f1af21` |
| `silicon-dm-cli 0.2.1` | `ce502d1067060e089056996df17b913fdc39bb57242e0096454c72d273476e8a` |

These checks used the preserved 0.2.0 fixtures from the previous section. Each
command was invoked individually. Only the isolated updater timestamps were
aged to make the hourly check due; no actor profiles or tokens were present in
the CLI fixture.

| Individual check | Observed result |
| --- | --- |
| Old installed CLI `--version`, then `updates check` | Current 0.2.0, latest 0.2.1 |
| Enable CLI updates, age its private timestamp, then ordinary `profiles list` | Profile JSON completed first; automatic updater ran Cargo and replaced the installed CLI from 0.2.0 to 0.2.1 |
| New invocation `--version`, then `updates status` | Reported 0.2.1; automatic updates enabled, Cargo-bin replacement supported |
| Repeat ordinary profiles list within the hour | Succeeded without another installation |
| New CLI `updates check` | Current and latest both 0.2.1 |
| New CLI `docs deployment` from `/tmp` with `SILICON_DM_HOME=/dev/null` | Full embedded deployment guide exactly matched the published snapshot; no checkout or credentials needed |
| Old compiled SDK consumer `check_update()` | Current 0.2.0, latest 0.2.1 |
| Old consumer after-command updater with its explicitly supplied manifest and due policy | Ran `cargo update` from 0.2.0 to 0.2.1, then `cargo build --release --locked`; returned `updated:true` and `restart_required:true` |
| Inspect consumer lockfile | Registry dependency now 0.2.1 with the published checksum |
| Start the rebuilt release consumer, then `check_update()` | Current and latest both 0.2.1 |
| Rebuilt consumer after-command updater within the hour | Skipped with `disabled_or_not_due`; persisted policy timestamp unchanged |

The original 0.2.0 CLI binary and SDK lockfile backup remain available privately.
The isolated installed CLI was left on 0.2.1 with updates disabled after the
checks. No automated scenario suite or `cargo test` was run. This record was
appended after publication, so it is not part of the immutable 0.2.1 archive.

## Relay memory changes prepared for 0.2.2

The following checks were chosen and performed manually on 2026-09-06 in an
isolated local state directory. Its single profile used deliberately invalid
credentials and an unavailable loopback backend, so requests stayed queued
without changing any IAM session or remote data. The new source was checked
with `cargo check -p silicon-dm-client -p silicon-dm-cli --all-targets`, strict
Clippy with `-D warnings`, and a CLI build. All passed; no automated scenario
suite or `cargo test` ran.

| Individual check | Observed result |
| --- | --- |
| Submit a conversation mutation and wait one second with the updated CLI and daemon | Returned the original ACK and full pending result at the deadline; the daemon retained and retried the offline write |
| Read the new authenticated `/requests/{id}/status` route | HTTP 200 with a 71-byte JSON body containing only `request_id` and `state` |
| Read that route without the local relay bearer | HTTP 401 |
| Retrieve the same request through `dm relay result` | Preserved the original request, pending state, transport error, and null result |
| Replace only the isolated daemon with the preserved registry-installed 0.2.0 binary | The status route returned HTTP 404; the updated CLI fell back to the existing full-result route and still returned the queued ACK and result after its one-second wait |
| Restart the updated daemon and submit 1,048,849 bytes of JSON, including an unknown `caller_context` field with a 1 MiB string | HTTP 202; the 1,048,933-byte ACK preserved the complete original JSON, including unknown fields |
| Issue two concurrent status reads for that large queued request | Both returned HTTP 200 and exactly 71 bytes |
| Fetch the full result for that large request | HTTP 200 with 1,048,958 bytes; the original JSON matched exactly, with pending state and null result/error |

The private daemon was stopped after these checks. Existing production and
manual messaging daemons were left untouched. These observations demonstrate
constant-size progress polling and compatibility with an older daemon; they
do not establish a total process memory bound. The shared 128 MiB work budget
limits admitted encoded queue payloads, while one larger payload can proceed
alone. Socket frames, HTTP responses, parsing, and the exact full ACK/result
still require additional memory.

### Waiting for the testing generation

The 0.2.2 prerequisite retry correction was also checked manually in a separate
native macOS fixture. A deliberately unavailable loopback backend kept its
testing profile from receiving a realtime `ready` frame. After submitting one
conversation mutation, two SQLite metadata observations eight seconds apart
showed `pending`, `awaiting_testing_generation`, and zero attempts. The due time
continued advancing in one-second increments rather than accumulating failure
backoff.

The private daemon was then stopped, that fixture's attempt count was seeded
to seven to represent earlier backend failures, and the daemon was restarted.
The request remained pending with exactly seven attempts and a due time within
one second. This confirmed that waiting for the prerequisite neither erases
earlier failures nor adds new ones. The existing HTTP retry path was unchanged.
The fixture daemon was stopped afterward; no real credentials, production
state, or active messaging daemons were used for this check. The updated CLI
build and strict Clippy checks passed without running automated tests.
