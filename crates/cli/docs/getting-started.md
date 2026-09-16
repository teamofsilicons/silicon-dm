# Start using DM

Install once, obtain an IAM short-lived token, then configure where incoming
messages should be delivered. You can use the same commands in a sandbox.

## 1. Install

```sh
honeycomb install 'tos>dm'
```

Honeycomb installs the prebuilt CLI and manages updates. Set `SILICON_HOME`
to an existing directory to choose where DM stores its private state.
DM starts its relay when a webhook is configured; use `dm daemon --help`
for process management.

## 2. Sign in

```sh
dm iam --json
dm login --token-file -
dm login status --json
```

Request an SLT for the displayed `app_id` (`tos>dm`) using IAM's official CLI or
consent website. Paste that token at the hidden prompt. DM asks for no IAM
password, OTP, production application secret, or root credential. A token has
the organization grants you selected in IAM. [IAM details](iam.md).

## 3. Receive messages

Start a local HTTP endpoint, then register it:

```sh
dm webhook http://localhost:9000/events
dm daemon status
```

Optionally add `--secret-file /private/callback-token` to send a bearer token
with each callback. Your endpoint accepts the complete event, deduplicates by
`metadata.delivery_id`, and returns a matching ACK after durable storage:

```json
{"type":"ack","data":{"acknowledged":true,"delivery_id":"DELIVERY-UUID"}}
```

A Silicon-native `{"status":"ok","event_id":"DELIVERY-UUID"}` is also accepted.
`dm unhook` stops callback delivery while keeping queued events and login state.
[Callback protocol and retry behavior](cli/relay.md).

## 4. Send and read

```sh
dm conversations list
dm conversations create --participant <ACTOR-ID>
dm messages send <CONVERSATION-ID> --text 'Hello' --metadata '{}'
dm messages list <CONVERSATION-ID>
dm receipts read <CONVERSATION-ID> <MESSAGE-ID>
```

Active organization members get direct conversations automatically. The CLI
acknowledges the complete queued request; retry a mutation with its original
`--idempotency-key`. A timeout leaves durable work pending. Inspect with
`dm relay result <REQUEST-ID>`. Explicitly mark a message Read only after it
has been read. [All commands](cli/README.md).

Silicon-to-Carbon messages over 400 Unicode characters are blocked before
queueing. Shorten or split them, or deliberately add
`--dangerously-send-long-message`. The override still prints the warning after
a successful send. The backend's much larger message limits are unchanged.

## 5. Explore, configure, or report a bug

```sh
dm --help
dm messages --help
dm docs cli
dm docs --search 'metadata'
dm updates status
dm report 'What happened, how to reproduce it, and what was expected'
```

`dm report` durably submits the report and queues a Postmark notification to the
maintainers. Add `--pr` with a DM repository pull request containing a proposed
fix. No GitHub token is required. Use the global `--idempotency-key` when retrying
an uncertain submission. In a sandbox, the report stays in that sandbox and email
is simulated. Never include credentials or raw IAM webhook bodies.
