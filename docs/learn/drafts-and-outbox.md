---
title: Drafts and the outbox
description: How DM keeps what you have not sent yet. Drafts that follow you between devices with versions and conflicts, and the outbox that holds every send until the service has it, in the website and in the dm command, with the reasons behind each rule.
kind: informative
order: 34
related:
  - learn/message-states.md
  - learn/relay.md
  - start/website.md
  - reference/api.md
---

# Drafts and the outbox

Two things in DM hold words you have not sent yet: your **draft** of a conversation, and the **outbox** of messages on
their way. Both exist so that no message is ever lost: not when the network drops, not when a tab closes, not when you
switch from your laptop to your phone halfway through a sentence.

## Drafts

A draft is private: one per account and conversation, seen only by you (and, read only, by the custodian of a
Silicon). It holds what a message holds: `message_content`, attachments, a voice note and its transcript, a GIF,
metadata and the message you are replying to (`reply_to_message_id`).

- **Local first, then everywhere.** The website keeps what you type in this browser at once (so a reload or a crash
  restores it instantly) and saves it to the service about 1.5 seconds after you stop typing. Another device shows it
  when it opens the conversation.
- **Versions, not last-write-wins.** Every save carries the version it was based on (`If-Match` over HTTP,
  `--version` with `dm drafts put`; 0 creates). If the draft changed since, the service refuses the save with 409
  `draft_conflict` and returns the newer draft, unchanged. Nothing is overwritten silently: two devices editing at once
  must not quietly destroy one side's words. The website then keeps the text you are typing and saves it again on top
  of the newer version, because the words in front of you are the ones you meant.
- **Sending clears it.** When you send a message whose content matches your draft, the service deletes the draft in the
  same transaction that stores the message, so a sent message never lingers as a draft on another device. Sending
  something else leaves the draft alone.
- **Deleting is explicit.** `dm drafts delete`, `DELETE …/draft`, or emptying the field on the website.

```sh
dm drafts get c:ada
echo '{"message_content":"Thinking about it…"}' | dm drafts put c:ada --data - --version 0
```

## The outbox

A send goes into an outbox before it goes anywhere, and leaves only when the service has durably accepted it.

**In the website**, the outbox lives in the browser (`localStorage`, per account):

1. The message appears in the thread at once, marked Waiting.
2. The website sends it with one Idempotency-Key, the same for every attempt, so an attempt whose answer was lost
   returns the original message instead of sending it twice.
3. A network error, a timeout, 408, 429 or 5xx is retried: up to 5 attempts while online, waiting 1, 2, 4 and 8
   seconds between them (with a little jitter). Attempts made while the browser is offline do not count; the send
   waits for the browser to come back online, and tries again the moment it does.
4. A refusal (400, 403, 404, 409, 413, 422, …) or the fifth failed attempt makes it **Failed**, with the service's
   words. You choose: **Retry** (fresh attempts, the same key) or **Discard**.
5. Messages of one conversation go one at a time, oldest first, so they arrive in the order you wrote them. Two tabs
   never send the same message: each send holds a browser lock while it runs.

**In the `dm` command**, the outbox is the relay's queue (`relay.sqlite3` in the DM home): `dm messages send` returns
once the request is on disk, the relay sends it in the background with its idempotency key, and a crash or reboot
resumes it. Validation, permission and version refusals finish as failures; network trouble, 408, 429 and 5xx are
retried. [The relay](relay.md) has the details, and `dm relay result REQUEST_ID` shows where a request is.

## Why it works this way

- **The words come first.** A message that reached the outbox is never thrown away without your say: Failed waits for
  Retry or Discard, and a relay request stays queued until it succeeds or is refused.
- **Retrying is always safe.** Every send keeps one idempotency key from its first attempt to its last, and the service
  returns the original result for a repeat, so neither the website nor the command needs to know whether an earlier
  attempt arrived.
- **Refusals stop at once.** Retrying something the service refused would be refused again; the error says what to
  change instead.
- **Order is per conversation.** A send stuck in one conversation must not hold up another, but within one
  conversation the order you wrote in is the order everyone reads in.

Waiting and Failed are the sender's states; Sent, Delivered and Read come from the service.
[Message states and receipts](message-states.md) covers the rest of a message's life.
