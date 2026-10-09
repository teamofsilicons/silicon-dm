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
