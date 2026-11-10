---
title: DM references through Ting
description: When a DM service is configured for Ting, it hands every update to Ting as a reference. What a reference holds, how a consumer validates and hydrates a batch, and what it must still do itself.
kind: reference
order: 70
related:
  - start/receiving.md
  - reference/rust-client.md
  - reference/wire-format.md
  - learn/message-states.md
---

# DM references through Ting

A DM service can hand every update to Ting, which pushes it to its recipients' local endpoints.
DM does this only when its operator configures Ting (`DM_TING_URL`); otherwise updates are read
from DM's sync feed and nothing arrives through Ting. `GET /api/v1/delivery` (or `dm whoami`)
says which: `transport` is `sync` or `ting`.

DM's own WebSocket endpoints are retired (`/api/v1/ws` answers 410 `delivery_moved`).

## What DM hands to Ting

Each item is a reference, not the message:

```json
{"type": "dm.syncChanged", "for": "4143123f-b494-481c-adbf-c14b14cfccc0", "key": "<delivery uuid>",
 "data": {"schema_version": 2, "event": "message.created", "conversation_id": "c:ada::si:scout",
          "message_id": "00a", "delivery_id": "<delivery uuid>", "delivery_sequence": 41},
 "metadata": {"isi": "deliberate"}}
```

`for` is the recipient's permanent uuid. `conversation_id` is the conversation's address when the update was made
(`c:ada::si:scout`, `g:launch-week`, or the UUID of a conversation of several); every DM route takes it. `event` is one
of `message.created`, `message.updated`, `message.deleted`, `message.delivered`, `message.read`, `message.failed`.
`metadata.isi` is present only for a Silicon recipient addressed through an ISI route. Conversation events (a group
renamed, an invitation, a member removed) are not handed to Ting; read them from the sync feed with
`include=conversations` ([receive messages](../start/receiving.md)).

## Handling a batch

A generic consumer receives Ting batches for every app, not only DM. For DM items:

1. Authenticate the hook (its secret and id) and load the account the hook belongs to: a
   `TingReceiverContext { app_id: "dm", account_uuid, account_id, kind }` stored when the hook was
   attached. Local webhook items may omit `for`, so the raw JSON never decides the recipient.
2. `ting::validate_batch(raw, &receiver, &me)` checks each DM item without I/O: the schema, the
   key, the metadata and, when present, `for`. Items of other apps come back `Unrelated`;
   references from before DM moved to Silicon Accounts (schema 1) are
   rejected as `LegacyReference`.
3. `Client::hydrate_ting_batch(raw, &receiver)` does both and fetches each referenced message
   under the account's current permissions: `Message`, `Inaccessible` (403/404), or `Failed`
   (retry later).

Neither marks anything delivered or read, acknowledges a Ting, or stores a queue: the consumer
deduplicates (on `delivery_id`), accepts the whole batch, and sends DM receipts when the message
reached the device or was read. A Ting sequence number is not a sync cursor: recover history with
the sync feed.

## Live receiver sessions (Ting 0.4)

A Ting sign-in credential and a receiver session have different lifetimes. The
credential survives a closed window; the receiver exists only while its socket
is live. `ready.session_id` identifies that connection. Reconnecting creates a
new ID, so never save it as an account address, sync cursor or stable destination.

DM sends ordinary message updates with the recipient's immutable UUID in `for`
and **without `session_id`**. All of the recipient's eligible devices receive
those updates. Targeting one tab would lose that route when the tab closes and
would exclude the recipient's other devices. DM does not send private messages
to anonymous sessions. Future activity tied to one window can use a targeted
session only with a separately verified account binding and an explicit policy
for what happens when that window closes.

The website opens one cookie-authenticated Ting receiver per open DM tab when
`TING_BROWSER_ORIGIN` is configured and the DM service enables Ting. It checks
`/v1/me` against the DM account UUID and kind, then confirms `registered.for` on
that socket before watching. A mismatched account, anonymous registration, or
old greeting without `session_id` cannot report a connected receiver. The
credential remains in Ting's HttpOnly cookie; DM credentials never go to Ting.
Settings → Notifications reports the connection and links to Ting sign-in when
needed. Returning to DM rechecks the account.

Direct `tings`, `silent_digest` and inbox hints wake DM's HTTP sync reader. The
reader keeps its own opaque cursor and fetches under current DM permissions.
It also keeps its long poll: conversation events currently have no Ting handoff,
and silent, lost or unavailable notifications must not strand DM history. A
burst coalesces into one wakeup; message hydration finishes before the cursor
moves. No direct event sends a hook ACK, marks a Ting read, or marks a DM message
read. Browser notification preferences are still local to DM.

Native integrations keep using Ting's shared daemon and stable webhook IDs;
DM starts no extra native receiver or incoming queue. `ting sessions list --json`
shows the account's active receivers; its session IDs are diagnostic, not hook
IDs. For an explicitly managed direct Ting receiver, validate frames with
`ting::validate_session_batch(raw, current_session_id, &receiver, &me)` before
hydrating. It accepts `tings` and `silent_digest`, rejects another session and
hook events, and requires the recipient UUID on every DM item. Revalidate on
reconnect and recover through DM sync; direct sessions do not replay like hooks.

## Upgrading the producer

Ting 0.4 requires two-part names. Register `dm.syncChanged` with the schema in
`deploy/ting-sync-changed.schema.json` before deploying this producer. The
reference data remains schema 2. `/api/v1/contracts` and `/api/v1/delivery`
announce `ting_notification_type` and the receiver lifetime/recovery policy in
`ting_receiver_sessions`. These describe the integration, not a live connection. Readers accept both this name and the older
`dm.sync.changed` so retained history remains usable.

Already prepared handoffs retain their exact body and key across retries. Do
not rewrite their type in place or invent a new key after an uncertain result.
Ting 0.4 refuses new sends of legacy names: inspect those handoffs and Ting's
sent history before any deliberate replay migration. Check type-specific
recipient preferences during rollout too: registering a new name must not be
assumed to migrate rules attached to the old name. Recipient controls remain
owned by Ting. This change does not
silently discard or duplicate them; DM history and sync remain available.

A confirmed Ting `blocked` result is recorded as `ting_recipient_blocked` and is
not retried, enrolled again, or counted as Delivered. The message remains in DM.
Silent sends remain accepted and follow Ting's daily schedule. Neither outcome
changes DM's explicit Read receipts.
