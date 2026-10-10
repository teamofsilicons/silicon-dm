# Test in an isolated environment

Use the same DM API, Rust client, CLI, and website against an empty sandbox.
Honeycomb owns creation and lifecycle. IAM owns identities and authentication;
DM owns its isolated message data.

## Create the world in Honeycomb

Create a shared testing environment in Honeycomb and include `dm`. Wait for
shared readiness, then use DM's test `app_secret`. IAM provides the sandbox
Carbon/Silicon identities, memberships, authentication and signed webhooks.
See [Honeycomb](https://docs.honeycomb.teamofsilicons.com/) for environment management.

Honeycomb prepares DM through its protected participant API. The shared
`environment_id` is also DM's ID. No user needs to pair a DM environment or
enter an environment root key. DM validates the app secret against IAM's current
state. When the participant integration is configured, missing preparation is
an error; runtime discovery cannot create an uncoordinated sandbox.

## Enter with the CLI

```sh
dm --app-secret-file - login <TEST-SLT-OR-PUBLIC-ID>
dm --test <ENVIRONMENT-ID> login status --json
dm --test <ENVIRONMENT-ID> conversations list
dm --test <ENVIRONMENT-ID> messages send <CONVERSATION-ID> --text 'Sandbox only'
```

Use the app secret at the hidden prompt. Alternatively set `DM_TEST_APP_SECRET`
in the environment of the process. `--app-secret` is supported, but a private
file or environment injection avoids placing credentials in command history.
The selected environment's name and UUID appear at the end on **stderr**, on
success and failure. JSON remains on stdout. Help and parser failures also
identify that testing was selected without printing the secret.

Production and sandbox profiles are separate. Omitting `--test` and unsetting
`DM_TEST_APP_SECRET` selects production. A supplied invalid, revoked, mismatched,
or unavailable test credential produces an error; it never selects production.

## Enter from the website

From the sign-in screen, open **Use a testing environment**. Enter the app secret
and a test SLT or existing test identity's public ID. The same form is available
through **Account → Add an account**. The banner displays the environment name
and current test identity. **Exit testing mode** restores a saved production
profile; if none exists, you return to sign-in.

Secrets and authentication tokens stay in the gateway's private server-side
session store, not browser storage or URLs. The gateway validates the environment
with DM before exchanging an identity and selects the profile only after success.

## Use the API and Rust client

Pass `X-Testing-Environment-Key: <test-app-secret>` with every HTTP request,
including `/api/v1/iam`, login, refresh, and user operations. The historical
header name is retained for compatibility. `/api/v1/iam` returns the selected
UUID, generation, and non-secret metadata. The secret is never returned there.
Bind `X-Testing-Environment-Generation` to that observed generation on mutations.
Missing or stale generations fail closed; login/refresh do not require it.

```rust
let client = silicon_dm_client::Client::new("https://backend.dm.teamofsilicons.com")?
    .with_test_key(test_app_secret)?;
let environment = client.iam().await?;
let tokens = client.login(test_slt_or_public_id, "stable-test-login-key").await?;
let authenticated = client
    .with_testing_generation(environment.testing_generation.ok_or("missing generation")?)?
    .with_auth(tokens.access_token, tokens.organization_id);
```

The optional runtime also provides
`select_testing_application(base_url, secret)` to discover and store a selection.
The default client remains stateless and never reads environment variables.

## Identity and permission rules

A test app secret selects storage; it does not identify a user or bypass
permissions. Login goes through IAM, which accepts a test SLT or the public ID
of an existing active sandbox Carbon/Silicon. Unknown or inactive identities are
rejected. Production login follows the ordinary SLT exchange; the test shortcut
never crosses into production. Token introspection must match the selected
environment, application, actor, membership, and organization.

## Lifecycle and isolation

Each environment uses a separate schema in a testing database distinct from
production. Message data, drafts, receipts, versions, directory projections,
webhook receipts, background jobs, contract counters, and realtime hubs are
scoped to it. Local queues and caches include the environment and generation.
Honeycomb clean blocks requests and deliveries, invalidates open connections,
and removes all DM-owned test records before reporting completion. This includes
messages, drafts, groups, receipts, presence, queued deliveries and directory
projections. Attachment links are references; DM never deletes another app's files.
Completed retries return the original receipt without cleaning subsequent data.
A failed cleanup stays fenced until the same operation succeeds.

Disable blocks runtime access while retaining data. Restore requires IAM readiness
and does not undo a clean. Control operations still work with test sessions disabled.
Purge retains only a secret-free lifecycle tombstone and receipts to reject stale
recreation and recover lost responses. DM reports activity to Honeycomb and no longer
runs independent environment retirement. See [participant operations](honeycomb-lifecycle.md).

## Webhooks and external actions

IAM's full raw signed body is verified before acting on it. The validated
`testing_key` digest must match the currently authenticated IAM environment.
Only the corresponding sandbox receives the event. Deduplication and aggregate
versions protect projections against repeated or out-of-order deliveries. Stored
webhook records omit the root key and the raw secret-bearing envelope.

DM sends messages to registered test identities. Your sandbox callback must use
a test or simulated destination; DM does not infer whether arbitrary callback
code sends email, SMS, payments, or other effects. Configure separate callback
endpoints when exercising integrations with such systems.

## Existing manually paired environments

Older DM root-key environments retain their original isolated credentials and
controls. They are separate from automatically discovered IAM environments.
[Legacy administration](testing-legacy.md) describes that compatibility path;
new users should follow the Honeycomb app-secret flow above. New manual pairing
is disabled when Honeycomb participant authentication is configured.
