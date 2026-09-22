# Honeycomb lifecycle participant

Configure `DM_HONEYCOMB_SERVICE_TOKEN` from your deployment secret store and register
DM's HTTPS backend with Honeycomb's participant registry for the configured
`DM_IAM_APP_ID`. The same dedicated service credential must be available to both
services. It must contain at least 32 visible ASCII characters and must be separate
from application secrets, root keys and user tokens. Provision it once and reuse it
across deployment retries. `DM_HONEYCOMB_BASE_URL` selects Honeycomb's origin for
activity reporting. Neither value is entered by end users when creating a sandbox.

Apply migration 0022 to production before starting the new API. It is excluded from
test schema migrations. Keep the existing separate testing database and encryption
key. Runtime grants include the new production control tables.

The coordinator uses:

```text
PUT /internal/honeycomb/organizations/{org_id}/testing-environments/{environment_id}/operations/{operation_id}
GET /internal/honeycomb/organizations/{org_id}/testing-environments/{environment_id}/operations/{operation_id}
Authorization: Bearer <service credential>
```

PUT accepts Honeycomb's plain JSON instruction: `operation_id`, `environment_id`,
`org_id`, `app_id`, positive `environment_revision`, `generation`, `key_version`,
`action`, and `testing_key`. Optional fields are `name`, `description`, `snapshot`,
`reason`, and `retired_apps`. Route identities must match the body. Supported actions
are prepare, import, refresh-import, rotate-key, clean, disable, restore, purge and
retire-applications. No runtime IAM login is used to authenticate these routes.

Responses are plain JSON receipts with the exact operation/environment/app identity,
revision, generation, key version, retirement selection, and `state` of pending,
completed or failed. GET recovers a lost response. Internal participant transport
uses this contract instead of the public DM type/data envelope. Receipts omit keys,
configuration snapshots and arbitrary error details.

Replay the identical instruction with the same operation ID after a timeout or
failure. Changed requests conflict. New instructions require a newer revision;
clean advances generation and rotation advances key version. A pending operation
must finish before another operation can supersede it. Cleaning or rotating a
disabled environment preserves its disabled state. Reimport may prepare a retired
participant; purge is irreversible and cannot be rediscovered through runtime auth.

DM persists a pending control record before performing destructive work. An
exclusive lock in the testing database serializes lifecycle effects against shared
HTTP request and Ting handoff attempt fences. The control record remains linked during clean.
Only after the test database commits does the production receipt become completed.
Crashes on either side of that commit can safely retry while access stays blocked.

Every managed runtime request fence checks IAM's current testing context. IAM is
responsible for enforcing shared readiness. DM revalidates lifecycle context
before requests and publisher attempts. Generation changes invalidate local
queued writes, sync cursors and cached data. Ting owns receiver-session lifecycle;
its context must match the selected DM environment and generation.

Activity is durably recorded and periodically sent to
`POST /api/v1/environments/{id}/apps/{app_id}/activity` with the shared testing-key
header, generation, key version and a stable idempotency key. Unacknowledged reports
retry. Clean resets the prior activity generation. DM never independently retires
an environment, and never deletes external attachment files.
