# DM migration decisions (Silicon Accounts + Silicon Apps)

The Carbon asked for DM to move from IAM and Honeycomb to Silicon Accounts and Silicon Apps, and said not to ask
questions. Every judgement call made during the migration is recorded here, newest stage last, for the Carbon to review.
The brief's decisions (D1–D9) and `apps/dm.md` are the inputs; this file records how they were applied to DM and what was
decided where they were silent. The root `decisions.md` is the historical (IAM-era) log and is append-only.

## Stage 1 — service (2026-10-10)

### Identity and storage keys

- **The storage key of an account is its Accounts uuid.** Every actor column (`actor_id`, `created_by_id`, `sender_id`,
  `recipient_id`, `target_id`, `carbon_id`, `originator_id`, …) holds the uuid for rows written by DM 0.15. Rows written
  before the switch hold IAM public ids (`c:saket`, `si:cos`) until the operator runs `dm-migrate link-identities` at
  cutover, which rewrites them to uuids in one transaction (the 0036 precedent) and records every old → new pair in
  `dm.identity_links`, so the rewrite is reversible and re-runnable with a different mapping before cutover. Legacy ids
  always contain `:` and Accounts uuids never do, so the two forms cannot collide. An unmapped legacy id is left as it is
  (it can never authenticate) and is rendered as itself, marked `legacy`.
  *Why not parallel uuid columns:* DM has 25+ identity-keyed tables tied together by composite foreign keys, immutability
  triggers and DB-computed participant hashes; a second set of columns would double every invariant. The in-place rewrite
  is what 0036 already did for the IAM 3 → public-id change.
- **uuids are text, compared exactly** (`COLLATE "C"`, never lowercased, never cast to `uuid`).
- **The wire keeps `id` as the current public id and adds `uuid`.** The domain works on uuids; the HTTP layer renders
  every actor reference `{type, id: <uuid>}` as `{type, id: <current c:/si: id>, uuid}` from the local accounts cache
  (the same response-translation layer that already turns conversation UUIDs into public addresses). Deleted accounts
  render as `{type, id: <uuid>, uuid, deleted: true}` (no handle is shown for a deleted account, and a uuid can never be
  mistaken for an address). Inputs that name accounts (`participant_ids`, `recipient_id`, `member_ids`, presence paths,
  direct addresses `c:a::si:b`, ISI addresses `isi@si:x`) are resolved to uuids before any business logic runs.
- **ISI routing strings** (`messages.sender_address`/`recipient_address`) keep their meaning but are stored as
  `isi@<uuid>` for new messages and rendered with the current id; legacy strings are rendered as stored.

### Organizations are gone; `organization_id` becomes a storage partition

- `organization_id` columns stay (no data is dropped). They now mean "storage partition": legacy rows keep their IAM
  organization; every conversation created by DM 0.15 lives in the partition `@accounts` (a value no IAM organization
  can have, since organization handles never contain `@`). Rows that hang off a conversation (messages, receipts,
  drafts, bundles, deliveries, handoffs) use their conversation's partition; per-account rows (idempotency keys, recent
  GIFs, bug reports, presence leases, Ting enrolment) use `@accounts`. No request carries an organization any more;
  `X-Org-ID` is ignored if sent.
- **One sync stream per account.** Delivery sequences are allocated from the account's `@accounts` stream whatever the
  conversation's partition (the allocation trigger is redefined), so `/api/v1/sync` reads one contiguous sequence across
  partitions. The re-key starts each re-keyed account's stream above every legacy sequence and records that floor;
  older (pre-cutover) events are not replayed, so every client resets its sync cursor once (documented).
- **One canonical direct conversation per participant set.** A partial unique index on the participant-set hash
  (`NOT is_group AND superseded_by IS NULL`) replaces the per-organization uniqueness. The same pair that had
  conversations in two organizations keeps both histories; the less recently active one is marked `superseded_by` the
  other (by the migration and again by the re-key, whose hash recomputation can surface new duplicates). Superseded
  conversations stay readable by UUID; sending into one answers `409 conversation_superseded` naming the canonical one.
  Direct addresses (`c:a::si:b`) resolve through the accounts to the canonical conversation.

### Who can reach whom (no Teams)

- Any signed-in account can start a direct conversation with a **Carbon** (by `c:` id). First contact (a new
  conversation with an account you have no conversation with yet) is rate limited per account.
- A **Silicon** can only be reached by its circle (itself, its custodian, the custodian's other Silicons) and by
  accounts on its allow-list. The Silicon manages its list (`/api/v1/allow-list`, `dm allow …`); its custodian manages
  it too (`?for=si:x`). Existing conversations keep working whatever the list says.
- **Custodian powers** (bounded, never acting as the Silicon): the custodian can list and read every conversation,
  message, bundle, draft and presence of its Silicons (`GET /api/v1/conversations?for=si:x`, and every read route of a
  conversation that includes one of its Silicons), act as an owner of groups its Silicon owns, tombstone messages its
  Silicon sent, remove its Silicon from groups, and manage its Silicon's allow-list. It can never send, edit, receipt,
  bundle or write a draft as the Silicon. Conversations that include a Silicon carry `visible_to_custodians` so the UI
  can say so.
- The Silicon ↔ custodian direct conversation is created automatically the first time DM sees a Silicon and on a
  custodian change. Nothing else is auto-created (the IAM-era "chat with every org member" is removed).
- Presence of an account is visible to accounts that share a conversation with it and to its custodian.

### Groups

- Any account can create a group; the creator is its first **owner**. Owners are co-equal (Silicon Apps author
  pattern): edit name/description, invite, cancel invitations, remove members, promote members to owner. The last owner
  cannot leave or be demoted; a group is never left ownerless.
- **Invitations must be accepted.** An invitee sees the pending invitation (`GET /api/v1/invitations`) and accepts or
  declines; members do not see pending invitees. Inviting a Silicon needs the inviting owner to be in the Silicon's
  circle or on its allow-list. An invitation of a custodian's own Silicon by that custodian is accepted at once. An
  accepted member sees the full history (contract).
- **Tag and public groups** (IAM-only concepts) are materialised: every account that had effective access when the
  migration runs becomes an accepted member, so nobody silently gains or loses access. `is_public`/`tag_ids` disappear
  from the API (columns kept).
- **Group ids:** existing `g:{org}:{slug}` ids never change and stay valid; new groups get `g:{slug}`, globally unique
  with `-2`, `-3` suffixes. Both grammars are accepted everywhere.

### Authentication and authorisation

- Every API route takes `Authorization: Bearer <Accounts access token, aud=dm>`, verified locally against the cached
  JWKS (refetched on an unknown `kid`, at most once per 30 s), issuer `ACCOUNTS_URL`. A token whose `iat` is before the
  account's `revoked_before` (set by sign-out/access-removed/deleted webhooks) is refused with `session_revoked`.
- **Introspection** (cached ≤ 30 s) runs on the sensitive routes the decisions name: group owner/settings changes,
  member removal, message deletion and allow-list changes.
- **Inbound proofs:** `Authorization: Proof sap_…` (User verification) is accepted on the conversation/message/receipt/
  draft routes for issuers listed per scope in `DM_PROOF_ISSUERS` (`dm.conversations.read=interface,…`; empty = deny).
  Scopes: `dm.conversations.read`, `dm.messages.read`, `dm.messages.write`, `dm.receipts.write`, `dm.drafts.write`. DM
  acts as `user.uuid` and records the issuing app on messages it sends (`metadata` is untouched; the message's
  `via_app` field says so).
- Removed: `/api/v1/auth/*`, `/api/v1/iam`, `/api/v1/testing-environments*`, `/internal/honeycomb/*`,
  `/api/v1/delivery/authorization*`, sandbox planes and `X-Testing-Environment-*`. Added: `GET /api/v1/me`,
  `GET /api/v1/accounts` (discovery), group owner/invitation/leave routes, allow-list routes, `GET /api/v1/sync?wait=`.
- HTTP contract version 4 (`x-dm-contract-version: 4`), DM 0.15.0.

### Data key, webhook and Ting

- `DM_DATA_KEY` (base64, 32 bytes) now keys the sync-cursor MAC and the at-rest encryption of stored proof refresh
  tokens (both used to derive from the IAM app secret). Cursors issued before the switch fail once with
  `sync_reset_required`.
- Accounts webhook at the existing path `POST /webhook/`: raw-body signature check (5-minute tolerance), dedupe on
  `event_id` in `dm.accounts_webhook_receipts`, ordering by `occurred_at` (ids, custodians, sign-outs) and by account
  `version` (profile updates). `app_revoked` sign-outs end nothing else; every other sign-out reason, access removal and
  deletion set `revoked_before` and drop the account's stored Ting proofs.
- **Account deletion:** messages stay in conversations others are part of (rendered as a deleted account); the account's
  drafts, recent GIFs, allow-list entries, presence leases and Ting enrolment are deleted; it leaves every group (if it
  was the last owner, ownership passes to the longest-standing member).
- **Ting stays off unless `DM_TING_URL` is set** (D4). DM keeps writing `ting_handoffs` either way (the durable
  handoff contract), and while Ting is off the worker marks them discarded with a reason (never deleted). Clients get
  updates from `GET /api/v1/sync?wait=N` (long-poll on Postgres `LISTEN dm_delivery`, N ≤ 30). With Ting configured,
  enrolment uses a User verification proof (receiving app `ting`, scope `tings.subscribe`) and sends use one cached App
  verification proof (scope `tings.send`); recipients are addressed by uuid + current id, with no organization.

### Further decisions made while building and testing (stage 1)

- **Names and photos come from DM's user base, never from lookups.** Silicon Accounts never returns a display name or
  photo to an app's lookup (`GET /v1/accounts/{uuid}` answers `{uuid, kind, id, status, custodian}`); an app reads them
  from its user base (`GET /v1/apps/dm/users/{uuid}`), which follows consent. DM reads it once when it first sees an
  account sign in (or when the cached row has no name yet, at most once a minute per process), only fills rows no
  `account.updated` event has described, and relies on `account.updated` afterwards. Accounts that never signed in to
  DM show only their id. Verified against the shared local stack.
- **Deleted accounts are anonymised** in DM's cache (name and photo dropped, current id cleared). They stay in direct
  conversations as `{"deleted": true, "id": <uuid>}`; such a conversation is addressed by the uuid, stays readable and
  takes no new messages (409 `recipient_deleted`). Lookups of deleted accounts are 404 at Accounts, so DM learns of
  deletions from `account.deleted` only.
- **No group deletion route.** A group ends when its members leave; the last owner hands ownership on first
  (409 `last_owner`).
- **Custodians may decline invitations for their Silicons, never accept them** (accepting is the Silicon's own act);
  a custodian's own Silicons invited by the custodian join at once.
- **Superseded conversations** are not listed; the canonical conversation names them in `supersedes`; a superseded
  conversation's address is its UUID. `link-identities` flattens supersession chains (a conversation superseded at the
  upgrade and its canonical superseded by the re-key both point at the final one), so a 409 never names a superseded
  conversation.
- **Unmapped IAM-era ids stay as stored** (no prefix), are rendered with `"legacy": true`, stay readable through their
  stored address, and are listed by `link-identities` as `unmatched`. `iam_membership_projections` is not re-keyed: it
  is kept as history and resolves principal UUIDs in mapping files.
- **Ting handoffs from before the cutover:** a request body prepared by the IAM-era worker is discarded
  (`prepared_before_accounts_cutover`); a handoff whose recipient is an unlinked IAM-era identity is discarded
  (`recipient_without_account`). Discards do not count towards a claim batch (bounded at 500 per claim), so a legacy
  backlog never stalls new handoffs. The pre-existing unbounded retention of accepted handoffs is noted, not changed.
- **Proof refresh without an idempotency key:** `silicon-accounts-client` 0.4.0's `refresh_proof` takes none, and a
  used refresh token revokes the proof, so Ting re-enrolment (the only refresh) runs one at a time per account across
  every DM process (a Postgres session advisory lock); the App verification proof for sends is cached behind a mutex
  and replaced 60 s before it expires.
- **Limits:** 100 new direct conversations per account per hour (`first_contact_limit`); Accounts lookups budgeted at
  550/min (under the 600/min per app); cached id → uuid answers trusted for 10 minutes before `lookup_by_id`; Ting
  enrolment re-checked at most every 10 minutes per process; JWKS refetches on an unknown `kid` at most once per 30 s
  (the first download does not count).
- **Reports** are stored and marked `simulated` outside production when Postmark is not configured.
- **Versions:** service, client and CLI 0.15.0, protocol 0.13.0. The protocol crate keeps the contract-3 route type
  names (`iam`, `auth/*`, `delivery/registration`, testing environments) so the 0.14 client keeps working until the CLI
  stage; the service no longer serves those routes.
- **The 0.14 client's Ting sandbox login** (IAM test environments) is refused locally with a clear message (the IAM
  client crate is gone); its three tests now assert the refusal. Everything else in the client and CLI moves to
  Silicon Accounts in the CLI stage.
- **Tests removed with retired features:** IAM sessions and directory, testing environments, WebSocket delivery
  (including the WebSocket frame case of `wire_contract`), IAM-era Ting credentials and authorisation. The Docker-only
  historical migration tests (which failed at baseline without Docker) are replaced by the upgrade test on a dataset
  written by the 0.14.1 code (`tests/fixtures/legacy-0038.sql`). Their behaviours are covered again by
  `messaging_flow`, `accounts_*`, `link_identities` and `ting_delivery`.
- **Operator copy:** `dm-migrate link-identities` and `draft-identity-mapping` name IAM in their help and messages
  because they are the cutover tools; no Carbon- or Silicon-facing surface mentions it.
- **Deploy templates** (`deploy/aws/*.yaml`) still pass the old variables; the ship stage switches them using the list
  in [cutover.md](cutover.md). CI's container smoke test already boots with the Accounts variables.

### Decisions made while finishing stage 1 (second session, 2026-10-10)

The first session's work was found uncommitted and complete; it was reviewed, re-tested and committed as it was,
with these additions:

- **Sign-out cutoffs compare at the precision of `iat`.** A token's `iat` has whole seconds and the webhook's
  `occurred_at` has milliseconds, so a token is refused only when it was certainly issued before the cutoff
  (`iat + 1 s <= cutoff`). A Silicon that rotates its STK and signs straight back in within the same second is not
  locked out; the at most one-second window of older tokens is closed on the sensitive routes, which introspect.
- **`GET /api/v1/sync` accepts `dm.messages.read` proofs.** The feed only references messages the account can already
  read, and with Ting off by default it is the only way an app acting for an account (Silicon Interface) learns about
  new messages without polling every conversation. Presence, GIFs, reports, groups changes, invitations, the
  allow-list and `/me` still need the account's own token.
- **No account-grouping words in API surfaces.** The OpenAPI document, API guide and contract discovery say "its
  custodian and the custodian's other Silicons" instead of "circle", and describe older three-part group ids by
  example (`g:tos:product-design`) instead of `g:{org}:{slug}`. The contract discovery keeps the literal retired paths
  (including `/api/v1/iam`) in `retired_routes`, because old clients need to see why those paths now answer 404.
- **The local test stack's `dm` webhook** was pointed at the local service only while the live checks ran and was
  restored to its earlier setup (`http://127.0.0.1:9593/dm/webhooks`, every update) afterwards; the stored secret was
  never rotated. No other app's setup was touched.

## Stage 2 — client crate and CLI (2026-10-10)

### Signing in

| # | Decision | Why |
| --- | --- | --- |
| C1 | Sign-in lives in `silicon-dm-client` behind a new `accounts` feature (`runtime` implies it): `accounts::PublicClient` with the device flow, short-lived token exchange, rotating refresh and revoke, all as DM's public client (`client_id=dm`, no secret). The default build stays stateless and adds no Accounts dependency. | The contract makes the Rust package the primary interface; the CLI has no capability the package lacks. Apps that only call DM (Interface) need neither. |
| C2 | The short-lived token exchange and the revoke are direct form POSTs (`grant_type=urn:silicon:params:oauth:grant-type:slt`; `/v1/oauth/revoke` with `token_type_hint=refresh_token`). Device flow and refresh use `silicon-accounts-client` 0.4.0 (`app_device_authorize`/`app_device_poll`, `refresh_app_public_client`). | crates.io 0.4.0 has no `exchange_slt_public_client` (the brief's cheat sheet names one that exists only in the unreleased repo), and its public revoke accepts only first-party client ids. |
| C3 | A refused short-lived token is classified from Silicon Accounts' description: `slt_already_used`, `slt_expired`, `slt_wrong_app` (names the app), `slt_unknown`, `slt_not_a_short_lived_token`, `slt_sign_in_ended`, else `slt_refused`; `invalid_client` is `public_client_disabled`. Input that does not start with `slt_` is refused before anything is sent. The token is never printed, logged or stored. Every hint is `silicon-accounts login --app dm -q \| dm login --slt-stdin`. | Accounts answers every refusal with `invalid_grant`; its description is the only signal. Checked against the real stack (progress.md). Same codes as mcport so Silicons see one vocabulary. |
| C4 | Login grammar: `dm login` (device flow), `dm login <slt>` (kept and documented: UNDERSTANDING.md names it and the Silicon runtime calls it), `--slt`, `--slt-stdin`, and `--token-file F\|-` (the 0.14 form). No `DM_SLT` variable (DM had none). `--base-url` stays a hidden alias of `--api-url`. | Brief D5 and the transition-alias decision. |
| C5 | Before spending a code or token, `dm login` reads the service's `GET /api/v1/accounts` and refuses `accounts_mismatch`, `app_id_mismatch`, or `service_not_accounts` (404/406: a DM before 0.15). An unreachable service only warns. The CLI never adopts the service's Silicon Accounts URL by itself. | Single-use tokens; a look-alike service must not steer a Carbon to another approval page. Same rule as mcport. |
| C6 | Device flow: polls every `interval` seconds, 5 more after each `slow_down`, gives up at the code's expiry (at most 10 minutes); a poll that cannot reach Silicon Accounts is retried until then. Label `dm CLI on <host>` from the host name (no subprocess); `--label` overrides; `--open` is the only way a browser opens. Human mode prints the code and page on stderr; `--json` prints `{"event":"device_code",…}` (and `slow_down`) lines on stdout before the result. | Stage instructions; scripts and test harnesses read the code from the first stdout line. |
| C7 | The sign-in is a profile in the existing `config.json` (profile format 2) in the existing state directory (`$SILICON_DM_HOME`, the `dm config home` pointer, `$SILICON_HOME/.silicon-dm`, `~/.silicon-dm`): 0700/0600, atomic replace under `config.lock`, symbolic links refused. Each profile remembers its DM API URL, Silicon Accounts URL and app id, so refresh and sign-out go where it signed in. | D5 ("existing state dir"); one file keeps the relay port and bearer with the profiles as before. |
| C8 | Refresh when less than 60 s are left (the relay at 80% of the lifetime, at most 5 minutes early), under `refresh-<profile>.lock`, re-reading after the lock and storing the new pair before use. A refused refresh token signs the profile out and keeps the reason (`ended`); a passing Silicon Accounts failure keeps using a token with more than 10 s left. A 401 from DM expires the attempted token and repeats the call once. | Rotating refresh tokens: a reused one ends the whole sign-in. Verified live: four concurrent commands rotated once and the sign-in kept working. |
| C9 | The internal profile key stays `<name>:production`. | Requests queued by 0.14 are stored under that key; changing it would orphan them. |
| C10 | Profiles from before Silicon Accounts load as signed out with only their old public id (`legacy.actor_id`); their tokens are dropped. Their queued requests are sent when the same id signs in again and fail with `account_changed` otherwise; sandbox profiles stay inert and their queued rows fail with `testing_environments_retired`. Settings of retired features (test keys, webhook secrets, update policy, the old organization) are not written back. | Survey: never send as the wrong identity; never crash on old files; retired secrets serve no purpose. |
| C11 | An unreadable `config.json` is kept as `config.json.unreadable-<time>` and replaced by defaults (under the lock); `dm login status --json` only peeks and reports `reason: unreadable_state`. | A damaged file must never block signing in again, and nothing is deleted. |
| C12 | A profile holding queued requests is never taken over by another account or another DM service (`profile_in_use`); otherwise `dm login` replaces the sign-in and revokes the replaced refresh token. | Queued work must not be sent as someone else; a replaced sign-in can never be used again from this home. |
| C13 | `dm login status --json`: verifies with `GET /api/v1/me` (refreshing when needed); `--offline` reads the file only (`verified: false`). Signed out: exactly `{"authenticated":false}`, with `reason`, `message`, `hint` only when there is something to say (a sign-in from before Silicon Accounts, an ended sign-in, an unreadable file). With `--json` it always exits 0; without, 1 when signed out. A service that cannot be reached reports the stored sign-in unverified with a `warning`. | Stage instructions and silicon-accounts' own behaviour. |
| C14 | `dm accounts --json` = `app_id`, `client_id`, `accounts_url`, `api_url`, `version`, `contract_version`, `device_flow`, `public_client`, `sign_in {carbon, silicon}`, `status`, `install`, website/docs/repository/package URLs; it reads only `ACCOUNTS_URL`, `DM_API_URL`, `DM_APP_ID`. It, `dm docs`, `--help` and signed-out `dm login status --json` run before any async runtime, need no home and write nothing. | Silicon Apps validates these three commands signed out in a clean sandbox (no network, writes only to scratch). |
| C15 | `dm logout` revokes at Silicon Accounts and forgets the tokens; a failed revoke still signs out locally (`revoked: false`, `warning`). Queued requests stay for the same account's next sign-in. | The service ignores `app_revoked` sign-outs, so other machines stay signed in. |

### Commands and the client crate

| # | Decision | Why |
| --- | --- | --- |
| C16 | Removed: `--test`, `--app-secret`, `--app-secret-file` (now argument errors), `environments`, `updates`, the Ting receiver setup (`delivery …`, `webhook`, `unhook`), `profiles webhook`, `login --webhook`. The retired commands stay as hidden commands that answer `command_retired` with what replaced them. `SILICON_DM_TEST` or `DM_TEST_APP_SECRET` in the environment makes every networked command refuse (`testing_environments_retired`). | D5. A Silicon runtime still calling them fails loudly with guidance instead of "unrecognized subcommand", and a home that selects a test environment can never reach the live service by accident. |
| C17 | The Ting receiver setup is gone from the CLI and the client runtime (`ting_delivery`, and with it the `silicon-ting-client` dependency and reqwest 0.12). The client's `ting` module stays: it validates and hydrates DM references at schema 2 (recipient `{uuid, id}`, no organization) and rejects schema-1 references as `LegacyReference`. | Ting stays on IAM (organization-scoped API and sessions), so a DM profile can no longer be bound to a Ting session; while `DM_TING_URL` is unset nothing arrives through Ting; when Ting adopts Silicon Accounts its own CLI configures its daemon, as UNDERSTANDING.md already says (`ting webhook`). |
| C18 | Receiving without Ting: `dm sync` (one page; stateless; `--cursor`, `--reset`, `--limit`, `--wait`) and `dm inbox [--watch]` (JSON lines: `start`, `message`/`message_status` with the current message, `cursor` after each batch, `reset`, `done`; cursor saved per profile after every page; first run from now, `--from beginning`, `--cursor`; `--mark-delivered` opt-in; `--references-only`; Ctrl-C/SIGTERM exit 0). Both call the service directly (not through the relay), refreshing on 401. | Decisions file: `dm sync [--wait]` / `dm inbox --watch`. Long-polls would tie up relay workers. Receipts stay explicit per the contract; at-least-once is documented. Verified live: a new message reached a watching Carbon 0.04 s after the send command started. |
| C19 | New commands: `dm allow list\|add\|remove [--for si:…]`, `dm groups owners add\|remove`, `groups invitations [--for]`, `groups accept`, `groups decline [--for]`, `groups leave`, `dm conversations list --for si:…`. The removed group flags `--public`/`--tag` are hidden and refused with the reason. | Stage 1's routes; the custodian view uses `?for=`. |
| C20 | The long-message guard treats a send to a `c:` account id as a send to a Carbon (and a `si:` id as a Silicon) instead of failing to find the conversation. | Sending to an account id is the documented common case. |
| C21 | Relay: refuses requests that name a `testing_environment_id`; fails queued sandbox rows when a home attaches; reports profiles with uuid/id/kind and `incoming_delivery.code = sync_feed` (the 0.11–0.14 marker `delivery_moved_to_ting` is still recognized so a newer CLI replaces an older relay); reports a decoding or configuration failure by its own code instead of `transport_error`. | Old relays must be replaced, never mistaken for strangers; errors say exactly what failed. |
| C22 | Errors are JSON on stderr: `{"error":{"code","message","hint"?,…}}`, keeping the service's hint and adding one for common codes (`token_expired`, `contact_not_allowed`, `unsupported_contract`, `sync_reset_required`). | D5: errors say exactly what and why. |
| C23 | `silicon-dm-client` 0.15 API: `with_token`, `with_proof`, `without_credentials`; `accounts`, `me`, `delivery`, allow-list, owners, invitations, accept/decline/leave, `conversations_for`, `send_to_recipient`, `sync` with `wait`. Removed: `with_auth`, `login`/`refresh`/`logout`, `iam`, test environments, delivery registration/authorization, `check_update`, the WebSocket shims and the `ClientFrame`/`ServerFrame` types. `Actor` gains `uuid`, `display_name`, `pfp_url`, `deleted`, `legacy` (`same_account` compares uuids); errors gain `hint` and accessors. The crate allows `clippy::result_large_err` (documented): `Error::Api` keeps the whole error answer. | Contract 4; the WebSocket frames carried organization fields of a retired protocol. Semver-breaking (0.14 → 0.15). |
| C24 | The protocol crate drops the contract-3 route type names. | Nothing names them any more. |
| C25 | `silicon-dm-cli` is `publish = false`; the client and protocol crates stay publishable. | Silicon Apps is the only distribution channel and updater (survey open decision). |
| C26 | Guides: IAM/Honeycomb-era records move to `docs/history/`; `scripts/sync-cli-docs.py` skips `history/` and `migration/`, removes stale copies and gains `--check` (now a CI step); the docs site skips both directories; new `docs/accounts.md` and `docs/cli/receiving.md`; a unit test keeps retired services and account-grouping words out of the packaged guides. | Brief "History docs"; D9. |
| C27 | OpenAPI corrected where the live service differed: allow-list entries are `{account, added_by, added_at}`, drafts use `message_content`, owner changes need `Idempotency-Key`. | Found by the live run (the code is the truth); the CLI packages the document. |

### Local stack

| # | Decision |
| --- | --- |
| C28 | DM's sign-in setup on the shared stack required a phone number, so the device-flow approval was refused (`requirements_missing`). `apps/dm.md` says nothing is required, so this stage set `required_fields` to `[]` (read-modify-write with `expected_version` 2 → 3; redirect URIs, methods, optional fields, device flow and public client unchanged). DM's webhook was not touched. Production should match (cutover.md). |

## Stage 3 — packaging, CI, deployment and documentation (2026-10-10)

### Packaging and release

| # | Decision | Why |
| --- | --- | --- |
| S1 | `packaging/apps.yaml.in` and `scripts/package-apps.sh` (implemented in `scripts/package_apps.py`) pack one archive per target, `dist/apps/dm-<version>-<target>.tar.gz` with `apps.yaml` naming only that target and `bin/dm[.exe]`, after `silicon-apps validate` and `pack`, then check the archive's exact contents and validate it again extracted. The design is the one the other apps of this migration use. | The brief's packaging convention; one design across the eight apps is easier to review and operate. |
| S2 | The packager refuses a version that differs from `crates/cli/Cargo.toml` **or** from the workspace `Cargo.toml`. | DM releases the service and the CLI under one version (the Honeycomb packager enforced the same). |
| S3 | Linux binaries may need at most glibc 2.39. The release workflow keeps DM's native runners (`cargo build` on ubuntu-24.04 and ubuntu-24.04-arm); the Mac path (`scripts/build-release.py`) keeps cargo-zigbuild against glibc 2.28. | "Same toolchains as before": DM's releases were built natively on Ubuntu 24.04, which is what the fleet hosts and the Apps Linux workers run. |
| S4 | The six targets DM shipped stay (linux x86_64/aarch64, windows x86_64/aarch64, macOS x86_64/aarch64); linux-i686 and linux-armv7hf are not added. The packager accepts all nine, so adding one later needs no packager change. | The brief keeps the existing targets; the fleet runs x86_64 and aarch64; 32-bit cross builds of the rustls/aws-lc and SQLite stack are unproven. |
| S5 | Every build runner is native, so each one runs the discovery commands with `--discovery require`; one Linux job installs `silicon-apps-cli` 0.2.0 and packs all six, with SHA256SUMS, as the artifact `dm-silicon-apps-release`. Tags `v*` must equal the CLI (and workspace) version. `release.yaml` became `release.yml`. | The Silicon Apps workers re-run the commands at upload, but only the Linux ones exist; checking on the runner that built each binary catches the others now. |
| S6 | `publish-release.yml` (GitHub release of the Honeycomb bundle) is removed, not ported. | Silicon Apps is the only distribution channel; a GitHub release with binaries would be a second one. Tags still mark releases. |
| S7 | `scripts/build-release.py` replaces `build-honeycomb-release.py`: it builds the chosen targets on a Mac and packs them with the same packager. `honeycomb.yaml` and `package-release.py` are deleted. | Keeps the local release path without the retired packager. Verified for macos-aarch64 here (no zig or MSVC SDK on this Mac). |
| S8 | `scripts/install.sh` stays (the docs site serves it at `/install.sh`) and hands over to `silicon-apps install dm`; without `silicon-apps` it prints the official installer commands and exits 1 instead of downloading and running another installer. | Anything that still fetches the old URL gets a working instruction; running a second remote installer silently is not ours to do. |

### CI

| # | Decision | Why |
| --- | --- | --- |
| S9 | The Rust job uses a Postgres 16 service container (`DM_TEST_POSTGRES_URL`) instead of testcontainers, runs the discovery check on the built `dm`, and its timeout is 45 minutes. New jobs: the packager's unit tests, the deployment tests plus `cfn-lint` 1.57.2 on `fargate.yaml` and `production.yaml`, and the docs site's build and link check. | The stage asks for what the new code needs; the docs site was never built in CI; `gateway.yaml` is left out of `cfn-lint` because the website rewrites it and it carries an older warning. |

### Deployment configuration

| # | Decision | Why |
| --- | --- | --- |
| S10 | Non-secret Silicon Accounts settings are stack parameters (`AccountsUrl`, `AccountsApiUrl`, `AppId`, `ProofIssuers` defaulting to `interface` for the five scopes, `TingUrl` empty); `IamBaseUrl` is gone. Secrets stay in the existing `AppSecretArn` and runtime secret under the new key names (`DM_APP_SECRET`, `DM_ACCOUNTS_WEBHOOK_SECRET`, `DM_DATA_KEY`). | "The same secret store, new key names"; parameters are reviewable in a change set. |
| S11 | The testing RDS instance stays in both templates, annotated as retained and unused; the bootstraps no longer configure it and the bootstrap task role no longer reads its secret. | Never delete data; removing the resource would make CloudFormation delete (or fail to delete) a protected database. Its fate is a Carbon's call. |
| S12 | The ECS bootstrap carries the 0.14 runtime keys over unchanged (`RETIRED`) and never rotates `DM_DATABASE_URL` or `DM_DATA_KEY`. Section 5 of the cutover removes the retired keys after the rollback window. | The 0.14 task definitions name those keys; without them a rollback could not start. A new data key would reset every cursor and orphan stored proofs. |
| S13 | Bootstrap refusals now print the field or step (`Missing or invalid secret field: DM_DATA_KEY`); before, a missing field printed only `ValueError`. Values are never printed. | Errors say exactly what and why. |
| S14 | The cutover steps are commands of the bootstrap image (`status`, `migrate`, `draft-identity-mapping`, `link-identities`), started by `deploy/aws/run-bootstrap-task.py` in the app security group. The mapping travels in `DM_IDENTITY_MAPPING` and reaches `dm-migrate` on stdin; the draft is printed between markers. Operator runs log at `error` level. | The database is reachable only from the app security group; the bootstrap task already has the right roles and network. The task's filesystem is read-only; dm-migrate logs to stdout. Proven with the real `dm-migrate` against local Postgres and the local Accounts stack. |
| S15 | The mapping is drafted before the window with a one-off task definition (`silicon-dm-cutover`: the 0.15 bootstrap image on the 0.14 roles). | The stack cannot be updated before the window without moving the running services to task definitions whose secrets do not exist yet. Reviewing the mapping must not lengthen the window. |
| S16 | The window stops 0.14 before the snapshot, and rollback restores the snapshot under the same identifier. Running 0.14 on a 0040 database is not offered as a rollback. | Nothing is written between the stop and the restore point, so a rollback loses nothing; 0040 changes group access, delivery sequences and new group ids, which 0.14 was never tested against. |
| S17 | The webhook URL is set in the window once 0.15 answers; its secret is generated before (`silicon-apps webhook dm rotate` works without a URL). The picks add `custodian_change` to the defaults. | Deliveries to 0.14 would fail; DM needs custodian changes, which are not a default pick. |
| S18 | Production sign-in setup: only the website callback as redirect URI, `device_flow` and `public_client` on, no required fields, email and timezone optional; `allowed_origins` untouched. | The CLI's device flow and short-lived token exchange; the website exchanges codes on its server. |
| S19 | The website's hosting files (`gateway.yaml`, `gateway-bootstrap.sh`, `render-gateway.py`, the gateway image job, `README.gateway.md`) are left for the web stage, with a pointer in `README.gateway.md` and the plan in the cutover's "The website". | They configure the SolidJS gateway, which keeps running until the new website replaces it; the Next.js server's files do not exist yet. |

### Documentation

| # | Decision | Why |
| --- | --- | --- |
| S20 | Dated records (`RELEASE_IAM5.md`, `deploy/*` records and verification, `deploy/releases`, `releases/`) moved to `docs/history/`, which gained a README saying nothing there is current. | The brief's history rule; nothing referenced them. |
| S21 | `docs/releasing.md` is the release guide and a packaged topic (`dm docs releasing`), so the CLI's retired-word test guards it. | Releasing is part of operating DM, like `dm docs deployment`. |
| S22 | Install copy names both ways in: Carbons use the website (nothing to install), the command comes from `silicon-apps install dm`, with a link to install Silicon Apps. | The stage's install instruction for Carbons and Silicons. |
| S23 | The docs site links to `main` (it linked a retired release branch), its preview banner no longer names the earlier sign-in, and its package version follows DM's. | No current page may point at the retired systems. |
| S24 | The relay's internal type `Tenant` (a home attached to the shared relay) keeps its name. | An internal name for a state directory, never shown; not an account grouping. |
| S25 | The EC2 user data accepts only standard base64 for `DM_DATA_KEY`; the ECS bootstrap also accepts URL-safe base64, as the service does. | The EC2 template is the alternate path and its user data has a size limit; the documented `openssl rand -base64 32` is standard base64 either way. |

## Stage 4 — end to end against Silicon Accounts (2026-10-10)

A first session wrote the harness, found and fixed the bugs below and ran every scenario green across two runs, then
stopped before committing; this session reviewed that work, fixed three Clippy findings, wrote the missing guide,
ran the whole suite once more from a fresh database (151/151) and committed it.

### The harness

| # | Decision | Why |
| --- | --- | --- |
| E1 | Python 3 with the standard library only: `scripts/dev-accounts.sh` and `dev-accounts-stop.sh` (`scripts/dev_accounts.py up\|down\|restart\|status\|env`), `scripts/e2e-accounts.sh` (`tests/e2e/accounts_e2e.py`, scenarios 1–8, `--only`, `--keep-running`, `--run`) and `scripts/dev_ting_stub.py`. Identities come from the Silicon Accounts testkit through `tests/e2e/mint.mts` (`carbon`, `app-signin [--existing]`, `slt`), which takes the testkit and stack file from the environment and refuses a stack that is not on this machine. | DM's other operator scripts are Python (packager, release build, docs sync), so nothing new to install; the testkit is the stack's own way to drive the hosted pages and its mock email sender. |
| E2 | The harness uses the stack the way an app and its people would: Carbons through the hosted pages (code exchange with DM's secret) and the device flow, Silicons created by their custodian (`POST /v1/me/silicons`) and signed in with short-lived tokens, the stack's own `silicon-accounts` CLI for `login --silicon`, `login --app dm -q` and `apps remove dm`. Each Carbon signs in once per run (`--existing` saves an email code). | The stack's per-address and per-network code limits are shared by every app's tests. `mint.mts silicon`/`approve` do the same two calls the harness makes directly. |
| E3 | `dm-api` runs as a role holding only `deploy/runtime-grants.sql` (migrations as the database owner), on `127.0.0.1:4121` with the database `dm_e2e`. | Production's privileges: a missing grant fails here, not in the window. |
| E4 | DM's webhook at the stack points at the local `dm-api` only while it runs, and `down` puts the earlier setup back (`http://127.0.0.1:9593/dm/webhooks`). The seeded secret is used; only if the stack refuses it does the script rotate the secret, keeping the new one in `.mig/dev/webhook-secret` (0600, untracked). This stage never rotated it. | The stage's setup; other stages rely on the seeded secret. |
| E5 | The stack has no `ting` app, so scenario 6 runs `dm-api` with `DM_TING_APP_ID=interface` against a Ting stand-in on `127.0.0.1:4122` that checks every proof DM presents with `POST /v1/proofs/verify` using `interface`'s credentials and refuses a send on request (`recipient_not_registered`, `proof_expired`). | DM issues proofs only to Ting (cross-app matrix); the receiving side must verify them with a real app's credentials, and the testkit's fake `interface` is the only spare app. |
| E6 | Scenario 7 builds the release CLI during the run and packs it for this machine's target (`macos-aarch64`); `silicon-apps validate` runs with an empty home and an unreachable server. | Linux archives come from the release workflow's native runners; the production sign-in of the `silicon-apps` on this Mac must never be used. |
| E7 | Results go to `.mig/e2e/run-<n>/` (`result.json`, `transcript.txt`) with every token, short-lived token, STK, proof and secret redacted. CI only checks that the scripts parse. | Running the scenarios needs a Silicon Accounts stack, which CI does not have; the cutover runbook asks for a run on the release commit (2.7). |

### Fixes the scenarios found

| # | Decision | Why |
| --- | --- | --- |
| E8 | A token whose `iat` falls in the same second as an account's sign-out cutoff is decided by Silicon Accounts (introspection), and the answer is kept per token and cutoff for 30 minutes; tokens from earlier seconds are refused and from later seconds accepted. This supersedes stage 1's "benefit of the doubt" for the cutoff's own second. Proof requests are unaffected (proofs are verified with Silicon Accounts). | Scenario 5: a Silicon's access token minted just before it removed DM kept working, and its CLI kept saying `verified: true`, because token and removal shared a second. A Silicon that rotates its STK and signs straight back in still works: Silicon Accounts says that new sign-in is live. |
| E9 | Routes that check the sign-in live (group owner and settings changes, member removal, message deletion, allow-list changes) ask Silicon Accounts on every request and reuse only a negative answer (30 s); a proof on such a route is verified afresh. | Scenario 5: after DM's website signed a Carbon out (`app_revoked`, which sets no cutoff), its next group change still went through for up to 30 s on a cached "active". These requests are rare, so asking each time costs little. |
| E10 | Messages carry `via_app` (the app that sent it with a User verification proof) in the API, the client model and the OpenAPI document; the UNDERSTANDING proposal says so in one sentence. | Scenario 6: the column (0039) was written but never returned; D3 asks receiving apps to record the issuing app on what it creates, and the other participants should see how a message was sent. |
| E11 | `DM_TING_APP_ID` (default `ting`) names Ting's app id for enrolment and send proofs; DM's own id is refused at boot. The deployment templates do not pass it (the default applies). | E5, and a staging Ting may be registered under another id. |
| E12 | The CLI's relay answers a read at once when the profile's sign-in has ended (`authentication_required` with the reason) and keeps a change pending for the same account's next sign-in; a `--wait` that runs out on a pending change prints the last attempt's reason. | Scenario 5: `dm whoami` in a home whose sign-in had ended waited out its whole wait and said only "Request remains durably queued". |
| E13 | Every visible CLI argument has a help line, and the help test fails for one without. | Positional arguments and several options showed no description in `dm … --help` (D5: help follows the code). |

The first runs also failed on mistakes of the harness itself (a missing `Content-Type`, a CLI flag that does not
exist, a check reading the wrong field of a delivery, a shadowed variable); those were fixed in the harness, not in
DM. Nothing was found wrong in Silicon Accounts.

## Website implementation (2026-10-10)

- The shared Next/Arc BFF replaces Solid and the persistent gateway. A separate ECS website service uses `deploy/aws/web.yaml`, port4315 and the existing ALB; retained old gateway state is reserved for rollback. No migration code formats or copies that state.
- Browser sessions are sealed HttpOnly cookies. A durable local outbox contains only the signed-in account’s pending message content/idempotency keys; it is scoped to the immutable account UUID. Same-origin writes and server-verified Accounts tokens protect service calls.
- The service remains authoritative for custody, invitation, owner, allow-list and version conflicts. A conflict refreshes the query and preserves the person’s editor values for a deliberate retry.
- Runtime request forwarding allows the service’s128MiB message body ceiling. One website replica preserves the refresh replay cache semantics; horizontal scaling needs shared coordination first.
