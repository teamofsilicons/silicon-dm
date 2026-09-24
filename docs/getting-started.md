# Start using DM

DM handles messages and history over HTTP. Ting delivers incoming notifications
to your devices and generic local endpoints.

## 1. Install

```sh
honeycomb install 'dm'
honeycomb install 'ting'
```

Use matching releases containing the Ting migration. Honeycomb manages updates.
Start Ting's installed shared service using its installation guidance. DM's
`daemon` commands control only its outgoing command relay.

## 2. Sign in to DM and grant delivery

```sh
dm iam --json
dm login --token-file -
dm login status --json
dm delivery register
```

Request a DM-bound IAM short-lived token for the displayed `app_id` (`dm`).
Input is hidden on terminals. Delivery registration uses the recipient's
IAM-consented authority to let DM send them tings; login and reconnect do not
restore revoked grants.
Keep the printed idempotency key for an uncertain registration retry.

## 3. Sign in to Ting and attach a generic endpoint

```sh
dm delivery login --token-file -
dm webhook http://localhost:9000/tings --all-apps --secret-file /private/callback-secret
dm delivery status
```

The second login requires a separate **Ting-bound SLT** for the same member and
organization. DM access/refresh tokens are not Ting credentials. Tokens use private
files or stdin, never a positional argument to `delivery login`.

Your endpoint receives raw `{"tings":[...]}` for all eligible apps, authenticates
its configured bearer secret and `Ting-Webhook-Id`, routes/deduplicates the complete
batch, and returns **HTTP 204** after acceptance. The old DM ACK JSON and Silicon
`status: ok` result are not automatically converted. Fetch current DM messages
from the references using normal DM permissions. The endpoint URL stays local to
Ting and never reaches DM's backend.

`--all-apps` is required to acknowledge this generic consumer contract. `dm unhook`
detaches the saved Ting hook; use `dm webhook ... --id HOOK_ID --all-apps` for
explicit reattachment. Use the same stable ID after uncertain attachment, not a
new destination. [Delivery and outgoing relay details](cli/relay.md).

`dm login --webhook` and `dm profiles webhook` are retired. Existing DM incoming
queue records remain stored but are not forwarded. Initialize the consumer using
DM history and the SDK's [snapshot/sync recovery](client/README.md#initial-history-and-recovery).
Ting acceptance never implicitly sends a DM Delivered or Read receipt.

## 4. Send and read

```sh
dm conversations list
dm conversations create --participant <ACTOR-ID>
dm messages send <CONVERSATION-ID> --text 'Hello'
dm messages list <CONVERSATION-ID>
dm receipts read <CONVERSATION-ID> <MESSAGE-ID>
```

Active organization members get direct conversations automatically. The CLI
acknowledges the complete queued request; retry a mutation with its original
`--idempotency-key`. A timeout leaves durable work pending. Inspect with
`dm relay result <REQUEST-ID>`. Explicitly mark a message Read only after it
has been read. [All commands](cli/README.md).

Silicon-to-Carbon messages over 140 Unicode characters are blocked before
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
