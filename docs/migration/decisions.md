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
