---
title: Wire format
description: DM's JSON envelopes and shapes under contract 4. Conversations and identifiers, messages and their 0.17 fields (attachment details, voice notes, GIFs, speech, metadata, routing), history and deletion, the sync feed and its conversation events, drafts and retries.
kind: reference
order: 68
related:
  - reference/api.md
  - reference/contracts.md
  - learn/identifiers.md
  - learn/message-states.md
---

# Wire format

DM 0.17 uses HTTP contract **4** (`x-dm-contract-version: 4`). Explicit unsupported contract versions return 406. DM's WebSockets are retired; updates come from `GET /api/v1/sync` (or through Ting where the service is configured for it).

Requests and responses use `{"type":"...","data":{...}}`. Full schemas and examples are in `openapi.yaml`.

## Conversations and identifiers

`GET /api/v1/conversations` lists normal DMs and accessible groups. Participants include everyone with access. Filter its items for groups; there is no separate GET `/groups`. Group summaries omit `is_public`, `version`, and `invited_members`; group administration endpoints retain policy details.

A direct conversation's address is its two accounts' current ids joined by `::` (`c:ada::si:scout`); a group's is its id (`g:engineering`, or a three-part id such as `g:tos:engineering` for groups created before contract 4); a conversation of three or more accounts that is not a group, and one kept only for its history, is addressed by its UUID ([identifiers and addresses](../learn/identifiers.md)). Accounts are named `{"type","id","uuid","display_name"?,"pfp_url"?}` with the current id; a Silicon adds `"custodian"`, its custodian named the same way, one level deep (DM 0.17; absent when DM knows no custodian). Message IDs use conversation-local lowercase base36 codes, starting at `000`, padded to three characters and expanding to `1000` after `zzz`. Legacy message/conversation UUID aliases remain accepted. Bundle IDs independently start at `001` in each conversation. A bundle reference is `{"id":"001","role":"display"}` on its summary or `{"id":"001","role":"member"}` on an original message.

Conversation `last_message_status` is `sent`, `delivered`, `read`, or null. Every summary carries `unread_count` (messages from others, not deleted, not bundled away, without a read receipt from the listed account), `last_read_message_id` (a message code or null) and, since DM 0.17, `draft_version`: the listed account's draft version there (the `version` `GET …/draft` answers and `If-Match` takes), or null when it has no draft, so a client asks for a draft only when there is one. Group delivery/read requires every recipient. `updated_at` advances on messages, edits, deletion and group details/membership changes. Receipts, presence, typing and private drafts do not reorder conversations.

## Messages and history

A message contains `message-id`, `conversation_id`, `recipient_id`, `sender`, `message`, `attachments`, `voice_transcript`, `reply`, `bundle`, `history`, `created_at`, `updated_at`, `deleted_at`, `delivered_at` and `read_at`, and since DM 0.17 `attachment_details`, `voice`, `gif`, `speech`, `metadata` and `routed_to`. A message an app sent for its sender with a User verification proof also carries `via_app` (the app's id, such as `interface`); it is absent when the sender sent it itself.

`recipient_id` keeps its first meaning: in a group it is the group's id; in a direct conversation the routing address the sender gave, else the other account; in a conversation of several the routing address, else the conversation's UUID. `routed_to` is the routing address alone (`release-notes@si:sol`, current ids), or null when the sender gave none, in every kind of conversation; read it to show where a message was routed. Routing never changes, and a tombstone keeps it.

Attachments are credential-free HTTPS URLs. Text, an attachment, a voice note, a GIF or speech is required. Attachment-only messages have empty text. Reply content is resolved by the server. Caller-supplied content cannot change sender authority or conversation routing.

`attachments` lists URLs only (the attachments, then the voice note and the GIF, each once), as older clients expect. `attachment_details` describes the same URLs in the same order:

```json
{
  "attachments": ["https://files.example/report.pdf", "https://files.example/note.webm"],
  "attachment_details": [
    {"kind": "attachment", "url": "https://files.example/report.pdf", "name": "report.pdf", "content_type": "application/pdf", "size": 52100},
    {"kind": "voice", "url": "https://files.example/note.webm", "content_type": "audio/webm"}
  ],
  "voice": {"url": "https://files.example/note.webm", "permanent_url": "https://files.example/note.webm", "content_type": "audio/webm", "duration_milliseconds": 4200},
  "gif": null,
  "speech": {"voice": "mark", "voice_name": "Mark", "script": "[excited] It works!", "model": "eleven_v4", "status": "ready", "duration_milliseconds": 1520, "peaks": [12, 40, 100]},
  "metadata": {"thread": "release"}
}
```

(`peaks` has 64 values from 0 to 100; shortened here.) `voice.permanent_url` repeats `url` for clients before 0.17. `speech.status` is `pending` until DM has rendered the audio, then `ready`; `failed` after five failed attempts. `metadata` is the sender's, exactly as sent. History snapshots carry the same fields; a reply quotes only `message`, `attachments` and `voice_transcript`. Tombstones render `voice`, `gif` and `speech` as null, `attachment_details` as `[]` and `metadata` as `{}`.

New messages have `history: []`. Every accepted edit appends the previous content snapshot, oldest first:

```json
{
  "message": "See you Friday",
  "history": [
    {
      "message": "See you Thursday",
      "attachments": [],
      "voice_transcript": null,
      "reply": null,
      "created_at": "2026-09-17T10:00:00Z"
    }
  ]
}
```

A history entry's timestamp is when that content became current. Message edits/deletes have no numeric version or `If-Match`. Edits serialize under the message lock; later edits preserve the content they replace. Idempotency keys prevent duplicate retry entries. Group and draft concurrency tokens remain separate.

Deletion sets `deleted_at` without appending history. Stored history is retained; deleted public snapshots hide text, attachments, transcript, reply and history. Deleted messages cannot be edited or resurrected. Use `updated_at` to compare edits and always preserve a known deletion when merging delayed snapshots.

## Updates

`GET /api/v1/sync?cursor=…&wait=30` returns references, not content:

```json
{"type":"sync","data":{"events":[{"event_id":"5f0c…","sequence":41,"type":"message","conversation_id":"c:ada::si:scout","message_id":"00a"}],"cursor":"eyJ…","has_more":false,"upper_sequence":41}}
```

`type` is `message` (a new, edited or deleted message, or one whose speech finished rendering) or
`message_status` (delivered/read changed for a message you sent). Keep the cursor (even after an empty page), continue while `has_more`,
deduplicate on `event_id` and fetch the message. `wait` (0 to 30 seconds) holds an empty page open
until something arrives. 409 `sync_reset_required`: get `?reset=true`, refresh, continue. The
retired `/api/v1/ws` and `/api/v1/ws/shared` answer 410 `delivery_moved`.

With `include=conversations` (DM 0.17) the same feed also carries conversation events, in the same order and with
their own `event_id` and `sequence`:

```json
{"event_id":"0199f2a1-5d0e-7c11-9a43-0c6f9b8e2d11","sequence":42,"type":"conversation","conversation_id":"g:launch-week","change":"renamed"}
```

`conversation_id` is the conversation's id as `GET /api/v1/conversations` names it; there is no `message_id`.
`change` is `created`, `invited`, `added`, `invitation_accepted`, `invitation_declined`, `invitation_cancelled`,
`left`, `removed` (each about your own place in it), or `renamed`, `description_changed`, `members_changed`,
`owners_changed` (the conversation changed for you). Read the conversation (or `GET /api/v1/invitations`) on any of
them; after `left` or `removed` it is 404, so drop it. `read` means you read messages there on one of your devices;
it carries what is unread now, so there is nothing to read:

```json
{"event_id":"0199f2a1-5d0e-7c11-9a43-0c6f9b8e2d12","sequence":43,"type":"conversation","conversation_id":"c:ada::si:scout","change":"read","unread_count":0,"last_read_message_id":"00a"}
``` Without `include` these events are skipped and the cursor
moves past them, so a client that never asks never meets one.

Draft saves use compare-and-swap versions. A stale save returns HTTP 409 with `data.error.code: draft_conflict`, recovery instructions, and the current draft fields in `data`. The save was not applied. Keep local content, resolve against the returned version, and retry explicitly.

A 401 means the credential was refused (expired, signed out, or not a DM token): refresh once and repeat the request with the same body and idempotency key; a second 401 means sign in again.

Routes accept direct-conversation addresses, account ids (on sending routes), group ids, short message and bundle codes, and legacy UUIDs.
