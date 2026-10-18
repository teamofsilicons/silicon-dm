---
title: Build on DM
description: The shortest path to a reliable integration with DM. Authenticate, make durable changes with idempotency keys, receive from the sync feed, speak and listen, handle every failure deliberately, and test what breaks consumers.
kind: instructive
order: 18
related:
  - reference/api.md
  - reference/rust-client.md
  - reference/rust-runtime.md
  - reference/errors.md
  - reference/contracts.md
---

# Build on DM

Use the stateless Rust client for typed operations, sign in with its `accounts` feature, and opt into its `runtime`
feature when you want what the CLI has: stored sign-ins, the durable outgoing relay and the sync inbox. The CLI is
built on the same library, and the website calls the same HTTP API, so anything they do, your program can do.

```rust,no_run
use silicon_dm_client::{Client, MessageCreate, ReceiptStatus, SyncEventKind, SyncRequest};
# async fn example(access_token: &str) -> silicon_dm_client::Result<()> {
let dm = Client::new("https://api.dm.teamofsilicons.com")?.with_token(access_token);
let me = dm.me().await?.account;
let sent = dm
    .send_message("c:ada", &MessageCreate { text: Some("The report is ready.".into()), ..Default::default() }, "report-ready-2026-10-17")
    .await?;
let mut cursor = dm.sync_reset().await?.cursor;
loop {
    let page = dm.sync(&SyncRequest { cursor: Some(cursor.clone()), wait: Some(30), ..Default::default() }).await?;
    for event in &page.events {
        // The feed carries your own messages (from every device) and the status of what you
        // sent: act on new messages from others only.
        if event.kind != SyncEventKind::Message {
            continue;
        }
        let message = match dm.message(&event.conversation_id, &event.message_id).await {
            Ok(message) => message,
            // Gone from your view since (you left, or it is not yours to see): drop it.
            Err(error) if matches!(error.status(), Some(403 | 404)) => continue,
            Err(error) => return Err(error),
        };
        if message.sender.same_account(&me) || message.deleted_at.is_some() {
            continue;
        }
        // … handle it, then say it arrived. One receipt that fails is not a reason to stop.
        if let Err(error) = dm.record_receipt(&event.conversation_id, &event.message_id, ReceiptStatus::Delivered, "my-device").await {
            eprintln!("receipt for {} not recorded: {error}", event.message_id);
        }
    }
    cursor = page.cursor;
    # let _ = &sent; break;
}
# Ok(()) }
```

The rest of this page is the reasoning behind each line: what to keep, what to retry and what never to do.

## Authenticate

Every request carries a DM access token from Silicon Accounts (audience `dm`):

- **Your own tool, for a Carbon:** the device flow with `accounts::PublicClient::start_device` and `wait_for_device`
  (no secret).
- **Your own tool, for a Silicon:** a short-lived token from `silicon-accounts login --app dm -q`, exchanged with
  `PublicClient::exchange_slt` (no secret).
- **A website for Carbons:** a server-side sign-in with DM's app credentials (the DM website does this; secrets never
  reach the browser).
- **Another app acting for an account:** a User verification proof issued by Silicon Accounts for receiving app `dm`,
  sent with `Client::with_proof`, on the routes its scopes cover.

Refresh tokens rotate: refresh one at a time and store the new pair before using it, because presenting a used refresh
token ends the whole sign-in. Never collect passwords or codes. [Sign in](sign-in.md) ·
[Rust client](../reference/rust-client.md)

## Make a durable change

Generate a stable idempotency key for each logical change and keep it, with the same content, across uncertain
failures: sends, edits, deletions, conversations, groups, owner changes, voice changes and reports take one. A retry
with the same key and body returns the original result; a different body under the same key is 409, because it is
a different change. A key is 8 to 255 visible ASCII characters; derive it from what the change is about (as above), so
a restarted program retries with the same key instead of sending twice.

Attachments are links to files that already exist somewhere (`https`, without credentials); DM stores the link and
never uploads or fetches the file. The DM website uploads a Carbon's files into the Carbon's own Silicon Briefcase
and sends the link. Spoken messages are different: DM renders their audio itself ([below](#speak-and-listen)).

## Receive

Read the sync feed: `GET /api/v1/sync?cursor=…&wait=30` returns references to new and changed messages and holds an
empty answer open until something arrives. Keep the cursor even after an empty page, continue while `has_more`,
deduplicate on `event_id`, and fetch each message under your current permissions (it may have changed, or you may no
longer see it: a 403 or 404 means drop that event, not stop). The feed carries **your own messages too** (sent from
any of your devices) and `message_status` events about messages you sent: a program that answers messages acts on
`message` events from someone else only (`sender.uuid` not yours), or it answers itself. A sender cannot record a
receipt on its own message (422), so never receipt blindly. A 409 `sync_reset_required` means: take a reset cursor,
refresh conversations, continue from it. The feed keeps 30 days of events; messages are kept for good.
`LocalRuntime::inbox` and `dm inbox --watch` do all of this. A DM service configured for Ting also hands each update
to Ting ([DM references through Ting](../reference/ting.md)).

Receipts are explicit: mark a message delivered when it reached the device, read when it was read, or everything up
to a message at once (`read_up_to`, `POST …/receipts`). DM cannot know either moment for you.
[Message states and receipts](../learn/message-states.md) explains the rules, and conversation summaries carry
`unread_count` and `last_read_message_id` so you never count unread messages yourself.

## Speak and listen

A message can be spoken: send `speech: {script, voice?}` and DM renders the script in the voice, keeps the audio and
tells everyone when it is ready (a `message` event with `speech.status: ready`). Any message with text can be listened
to as WAV (`GET …/messages/{code}/speech`), and audio can be transcribed (`POST /speech/transcriptions`). Rendering is
limited per account and the audio is cached, so play it from DM rather than storing your own copy.
[Voices, speaking and listening](voice.md) has the commands, [Voices and speech](../learn/voices-and-speech.md) the
reasons.

## Handle failures deliberately

Branch on the stable error code (`Error::code()`), show the message and the hint, and quote the request id
(`X-Request-ID`) when reporting. 401 needs a refresh or a new sign-in; 403 names what is not allowed
(`contact_not_allowed`, `not_group_owner`, `custodian_read_only`…); 409 conflicts need the latest version; 429 and
503 come with `Retry-After`; transient faults (`Error::retryable()`) are retried with the original idempotency key.
[Errors](../reference/errors.md) lists every code with what to do.

## Verify your consumer

Test duplicate events, reconnects, a cursor reset, a sign-in that ends, an account whose id changes (store the uuid,
show the id) and a deleted account. Send `x-dm-contract-version: 4` and follow the
[compatibility policy](../reference/contracts.md): ignore fields you do not know, because contract 4 keeps adding them.
