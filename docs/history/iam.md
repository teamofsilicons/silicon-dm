# IAM integration

Current delivery guidance: [HTTP and Ting contracts](contracts.md), [Ting integration status](ting-integration-issues.md), and [sandbox entry](testing-environments.md). The previous DM WebSocket contracts are retired.

Silicon IAM owns all Carbon and Silicon authentication, organization membership, organization roles, consent, application sessions, refresh rotation, and revocation. DM uses the official [`silicon-iam-client`](https://crates.io/crates/silicon-iam-client) Rust SDK (5.0.0) for every IAM request. Application integrations do not enable the SDK's `cli-session` feature. DM has no OBO login, OBO proof exchange, delegated endpoint catalog, or inbound OBO routes.

## Backend application registration

The production application is `dm`. Its registered callback is:

```text
POST https://backend.dm.teamofsilicons.com/webhook/
```

Use the exact trailing slash. Configure the following only in backend secret storage:

| Variable | Purpose |
| --- | --- |
| `DM_IAM_BASE_URL` | IAM origin, normally `https://backend.iam.teamofsilicons.com` |
| `DM_IAM_APP_ID` | Canonical IAM application ID, `dm` |
| `DM_IAM_APP_SECRET` | Current application credential issued by IAM |
| `DM_IAM_WEBHOOK_SECRET` | Caller-selected secret registered with IAM; 32–512 visible ASCII characters |
| `DM_IAM_WEBHOOK_KEY_VERSION` | Exact webhook signing version, initially `1` |
| `DM_IAM_REQUEST_TIMEOUT_SECONDS` | Per-request dependency timeout, subject to backend settings |

The webhook secret differs from the application secret. Production credentials stay in backend secret storage and are never included in ordinary client login or session responses. An authorized administrator may explicitly configure a dedicated test signer through the testing-environment creation input described below. Never commit `.env` or print it in command transcripts. When rotating a webhook secret/version, coordinate the backend configuration and IAM registration; deliveries with an unconfigured version are rejected. The current backend configuration accepts one signing version, so IAM's retained old deliveries require the corresponding old version/secret to be temporarily restored and replayed if a rotation crosses their delivery window.

DM declares `self.identity.read`, `self.profile.read`, `self.organizations.read`, `self.membership.read`, and `self.tags.read`. The application must be verified, its configuration accepted, and the relevant disclosures authorized by IAM. Email and phone scopes are unnecessary for DM identity resolution. Membership disclosures supply the live organization role; an undisclosed role grants no administrative privileges. External Ting permissions require a separate feature approval described below.

## Login without collecting credentials

A caller obtains an IAM short-lived token after explicitly selecting one account and one organization for the DM session. Browser login starts at IAM `/login` with only `app_id` and `redirect_uri`; DM supplies no `org_id` or organization picker. Existing IAM CLI users can obtain one through the IAM app-login/SLT command described by `iam --help` and `iam docs client/authentication`. The DM client asks only for this SLT and the local relay webhook URL. The local webhook URL belongs to the client/CLI and is never sent to DM or IAM by the DM login route.

```http
POST /api/v1/auth/login
Content-Type: application/json
Idempotency-Key: 98ac875d-e610-40a8-b431-f17919dba362

{"type":"login","data":{"slt":"oac_REPLACE_WITH_IAM_TOKEN"}}
```

DM calls the SDK's `oauth().login(app_id, slt, mutation)` using its server-side application credential. DM then introspects the returned access token and validates its complete live organization authorization snapshot before returning any session:

```json
{
  "type": "login",
  "data": {
    "access_token": "oat_REDACTED",
    "refresh_token": "ort_REDACTED",
    "token_type": "Bearer",
    "expires_in": 1800,
    "scope": "self.identity.read self.membership.read self.organizations.read self.profile.read self.tags.read",
    "actor": {
      "type": "carbon",
      "id": "c:alice"
    },
    "organization_id": "tos",
    "organization_ids": [
      "tos"
    ]
  }
}
```

The actor can also be `silicon`; its ID is IAM's canonical public Silicon ID. The sample lifetime and scopes are illustrative; use the returned values. Login and refresh responses carry `Cache-Control: no-store` and `Pragma: no-cache`. No password, OTP, Silicon credential, application secret, or user-supplied actor ID is accepted by this route. IAM 5 application sessions select exactly one account and one organization. DM calls `oauth().authorizations()` to retrieve IAM-selected active membership authority, rejects an empty set, and verifies the selected organization with live scoped introspection. The compatibility `organization_ids` array therefore contains the same single organization as `organization_id`. To work in another account or organization, obtain a separate session; the array does not authorize switching to an unselected organization.

## Requests and identity

Every normal authenticated API request supplies:

```http
Authorization: Bearer oat_REDACTED
X-Org-ID: tos
```

The organization must match the token's selected, active membership; every request introspects that specific organization. Direct IAM Carbon (`cat_`) and Silicon (`sat_`) session tokens are not DM application tokens. Refresh tokens are accepted only by the refresh/revoke session routes. Both legacy OBO proof headers and `X-IAM-OBO-Access-Token` are rejected on DM routes, including requests that also carry a bearer token. Authentication headers must occur once; duplicate, comma-separated, empty, or malformed values fail closed.

DM performs live IAM introspection on authenticated requests. It verifies:

- The token is active and unexpired, belongs to the configured application, and names the requested organization.
- The principal UUID, actor type, membership UUID, authorization epoch, scopes, organization, and audience agree between introspection and its authorization snapshot.
- The snapshot belongs exactly to the selected production or IAM testing environment.
- The returned public ID and Carbon/Silicon type are authoritative; request headers cannot replace them.

`GET /api/v1/auth/me` returns `actor`, `organization_id`, `principal_id` (a legacy DM alias of the canonical member ID), optional `session_id`, optional `org_role`, and the effective `capabilities` array. Unknown or undisclosed roles do not grant access. Organization owner/admin authority is taken only from the live role, never from the test key or a cached user profile.

Before creating a conversation or reading another actor's presence, DM resolves recipients from its scoped IAM membership projection. Every fresh, cross-validated application-token introspection records the caller's principal, organization, membership UUID, public identity, membership version, and authorization epoch. Verified IAM `current.members` webhooks maintain other members and removal tombstones atomically with event deduplication. Older membership versions cannot overwrite newer snapshots; equal-version removals take precedence. Ordinary message/presence writes cannot reactivate a removed member. The projection grants recipient discovery only: every acting account still requires fresh IAM authorization. Ambiguous public IDs across actor types fail closed. A token represents its verified actor; delivery handoffs remain bound to the typed account that initiated the mutation.

DM resolves recipients from its own membership projection populated by verified sign-ins and IAM webhooks; it does not query a directory endpoint for an arbitrary recipient. An offline recipient works after their membership has been supplied by sign-in or webhook. A recipient never supplied to DM returns 422 with an explanation to sign in; DM does not infer membership from a public identifier. Missing/delayed webhooks can leave a recipient projection stale, while fresh authentication still controls all actual senders and receivers. This limitation also applies immediately after cleaning a DM environment. See the [upstream member lookup proposal](iam-member-resolution-proposal.md).

## Refresh and logout

```http
POST /api/v1/auth/refresh
Content-Type: application/json
Idempotency-Key: e826d1d8-13f8-4d2a-a363-e83c3a0779fe

{"refresh_token":"ort_REDACTED"}
```

Refresh returns the same schema as login. The presented refresh token is consumed and replaced. Persist the new refresh and access tokens atomically before sending further work. Retry a failed or uncertain response with the **same body and same idempotency key**; choosing a new key after a response is lost can consume a one-time credential again. DM derives a stable operation-specific IAM idempotency key from the incoming DM key and forwards it through the SDK.

```http
POST /api/v1/auth/logout
Content-Type: application/json
Idempotency-Key: efdce86c-43c2-4102-acd3-cc4f14385935

{"token":"ort_REDACTED"}
```

Logout returns `204 No Content`. Supply the current refresh token to revoke the whole application token family; supplying an access token revokes that token only. Login, refresh, and logout do not require a separate Bearer or `X-Org-ID` header because their credential is their JSON input. They still use the same test-environment selection headers as the rest of DM.

IAM also revokes access tokens for the same parent IAM session and application
when a refresh family is revoked. Other families can retain valid refresh
tokens and recover by refreshing. Clients treat an HTTP 401 as a reason to
refresh the attempted access token, even before its locally recorded expiry.
If refresh itself is rejected, status changes to
`authentication_required` and a fresh IAM SLT is required. Granting application
consent from a different parent IAM session can invalidate older families'
refresh authority; reauthenticate those profiles rather than reusing rejected
credentials.

Revocation advances the backend's durable authorization revision. Every Ting publish attempt uses the originator's separately approved OBO credentials, refreshing the dedicated token when needed. Ting verifies current endpoint authority with IAM for each operation. An IAM dependency outage fails closed; DM never manufactures sessions or falls back to a mock identity. Backend errors expose stable DM error categories and redact IAM provider details and credentials.

## IAM webhook verification and delivery

IAM sends the exact raw JSON body with these headers:

```text
X-Silicon-IAM-Event-ID
X-Silicon-IAM-Timestamp
X-Silicon-IAM-Key-Version
X-Silicon-IAM-Signature
```

DM uses the official SDK `WebhookVerifier` before accepting or acting on the event. A bounded test-envelope key is parsed into redacted secret storage only as a candidate-routing hint; it grants no authority. Signatures and the SDK's exact environment-key binding are verified before any test runtime is initialized or state is written. It verifies the HMAC over `timestamp + "." + exact_body_bytes`, a five-minute timestamp tolerance, exact signing key version, signature syntax, event/header ID consistency, unique security headers, and a maximum one-mebibyte body. Malformed or unauthenticated deliveries return an authentication failure and change no state.

A verified event ID is persisted transactionally before returning `204`. Duplicate
IDs are safe to retry. DM stores normalized event metadata rather than the raw
envelope or its secrets, and advances the plane's authorization revision.
Membership projections retain current removals and authority. HTTP requests and
every delegated Ting operation independently revalidate with IAM, including when a
webhook is delayed.

## Separate Ting authorization

DM declares the external Ting endpoints `subscriptions.register` and `tings.send`.
Accepted application configuration permits DM to request these endpoints; it does
not grant user permission. After ordinary DM login, the account explicitly starts
feature authorization, reviews the IAM consent page, and completes the returned
one-time authorization code:

```sh
dm delivery authorize
dm delivery complete AUTHORIZATION_ID --code-file -
dm delivery authorization-status
```

For HTTP integrations, use `POST /api/v1/delivery/authorization`, then
`POST /api/v1/delivery/authorization/complete`; see the [API guide](api/README.md)
for envelopes and mutation keys. Completion stores the separately approved
credentials and registers the recipient. DM requires Ting's selected account type,
account ID and organization to match the originating DM identity. A different
provider account or organization is rejected.

The backend stores dedicated OBO access and refresh credentials encrypted and
bound to the account, organization, endpoint, application, testing environment and
data generation. Only these dedicated credentials are refreshed by the publisher.
Ordinary DM login, refresh and logout neither create nor destroy this permission;
ordinary application tokens are not retained as notification authority. Migration
0038 clears legacy cached login tokens and automatic enrollment state rather than
converting them into consent. Signing in again cannot substitute for approval.

Each handoff records its initiating actor transactionally. Publishing uses that
actor's approved `tings.send` token, and registration uses its separately approved
`subscriptions.register` token. Neither another account nor a sandbox credential
can supply substitute authority. Missing, revoked or unusable authorization leaves
the handoff pending and reports `ting_authorization_required` when applicable; it
does not change DM message status or fabricate a delivered/read receipt.

Use `dm delivery register` only to explicitly register with already approved
permission. It does not create a consent grant. Receiving also requires a separate
Ting session and destination: `dm delivery login --token-file -` accepts a
Ting-bound SLT, and `dm webhook URL --all-apps` configures the consumer endpoint.
A DM token cannot act as a Ting receiver login.

`dm delivery disconnect-authorization` deletes the dedicated credentials stored
by DM. Revoke the grant in IAM to disable it globally. The corresponding HTTP
routes are `GET /api/v1/delivery/authorization` for local status and
`POST /api/v1/delivery/authorization/disconnect` for local disconnection.

## IAM testing environment binding

A DM testing environment requires a real IAM testing environment and an imported copy of `dm`. Import the existing canonical app using the installed IAM CLI, which issues a fresh **test-only application secret** while preserving approved application configuration. Do not submit the production app secret as the test credential. The imported webhook configuration initially retains the registered callback and signing secret. To register a dedicated callback for an IAM test application, supply both optional `iam_webhook_secret` and `iam_webhook_key_version` in the DM environment creation body. The secret must contain 32–512 visible ASCII characters and the version must be positive. DM encrypts the override separately from the test application credential. Omit both to inherit the backend signer. This permits a local tunnel callback and independently rotated test signer without changing the production callback. Use IAM's returned webhook key version: changing a callback can advance it even when the secret is reused.

Passing an IAM test application's `app_secret` asks IAM's testing-context API
to discover its environment, canonical application ID, current credential version,
webhook key digest, and cleaned timestamp. DM encrypts the selector and lazily
creates an empty isolated schema. It rechecks IAM on selection, invalidates older
generations after a clean, and never accepts an unavailable or revoked secret as
production. The application selector grants no environment-administration authority.

Test webhooks authenticate the complete raw signed envelope first. DM matches the
SDK-verified envelope's testing key against IAM's freshly discovered digest, then
applies only normalized invalidation records in that sandbox. Neither raw payloads
nor root keys are stored in event receipts. Duplicate IDs are idempotent and all
unique invalidations trigger re-introspection, so out-of-order events cannot restore
revoked authority. [Legacy root-key pairings](testing-legacy.md) remain supported.
