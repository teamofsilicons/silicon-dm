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
{"type": "dm.sync.changed", "for": "4143123f-b494-481c-adbf-c14b14cfccc0", "key": "<delivery uuid>",
 "data": {"schema_version": 2, "event": "message.created", "conversation_id": "c:ada::si:scout",
          "message_id": "00a", "delivery_id": "<delivery uuid>", "delivery_sequence": 41},
 "metadata": {"isi": "deliberate"}}
```

`for` is the recipient's permanent uuid. `event` is one of `message.created`,
`message.updated`, `message.deleted`, `message.delivered`, `message.read`, `message.failed`.
`metadata.isi` is present only for a Silicon recipient addressed through an ISI route.

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
