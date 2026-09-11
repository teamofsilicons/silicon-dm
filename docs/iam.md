# IAM integration

Silicon IAM owns all Carbon and Silicon authentication, organization membership, organization roles, consent, application sessions, refresh rotation, and revocation. DM uses the official [`silicon-iam-client`](https://crates.io/crates/silicon-iam-client) Rust SDK (1.4.0 or a compatible newer release) for every IAM request. Application integrations do not enable the SDK's `cli-session` feature. DM has no OBO login, OBO proof exchange, delegated endpoint catalog, or inbound OBO routes.

## Backend application registration

The production application is `tos>dm`. Its registered callback is:

```text
POST https://backend.dm.teamofsilicons.com/webhook/
```

Use the exact trailing slash. Configure the following only in backend secret storage:

| Variable | Purpose |
| --- | --- |
| `DM_IAM_BASE_URL` | IAM origin, normally `https://backend.iam.teamofsilicons.com` |
| `DM_IAM_APP_ID` | Canonical IAM application ID, `tos>dm` |
| `DM_IAM_APP_SECRET` | Current application credential issued by IAM |
| `DM_IAM_WEBHOOK_SECRET` | Caller-selected secret registered with IAM; 32–512 visible ASCII characters |
| `DM_IAM_WEBHOOK_KEY_VERSION` | Exact webhook signing version, initially `1` |
| `DM_IAM_REQUEST_TIMEOUT_SECONDS` | Per-request dependency timeout, subject to backend settings |

The webhook secret differs from the application secret. Production credentials stay in backend secret storage and are never included in ordinary client login or session responses. An authorized administrator may explicitly configure a dedicated test signer through the testing-environment creation input described below. Never commit `.env` or print it in command transcripts. When rotating a webhook secret/version, coordinate the backend configuration and IAM registration; deliveries with an unconfigured version are rejected. The current backend configuration accepts one signing version, so IAM's retained old deliveries require the corresponding old version/secret to be temporarily restored and replayed if a rotation crosses their delivery window.

DM needs the IAM application to be verified and approved for `profile`, `memberships.read`, `organizations.read`, `roles.read`, and `offline_access`. The effective grant also depends on the user's consent and organization authority. Email and phone scopes are unnecessary for DM identity resolution. `roles.read` supplies organization administration authority; an undisclosed role grants no administrative privileges.

## Login without collecting credentials

A caller obtains an IAM short-lived token after explicitly selecting the organizations DM may access. Browser login starts at IAM `/login` with only `app_id` and `redirect_uri`; DM supplies no `org_id` or organization picker. Existing IAM CLI users can obtain one through the IAM app-login/SLT command described by `iam --help` and `iam docs client/authentication`. The DM client asks only for this SLT and the local relay webhook URL. The local webhook URL belongs to the client/CLI and is never sent to DM or IAM by the DM login route.

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
    "scope": "memberships.read offline_access organizations.read profile roles.read",
    "actor": {
      "type": "carbon",
      "id": "alice"
    },
    "organization_id": "tos",
    "organization_ids": [
      "tos"
    ]
  }
}
```

The actor can also be `silicon`; its ID is IAM's canonical public Silicon ID. The sample lifetime and scopes are illustrative; use the returned values. Login and refresh responses carry `Cache-Control: no-store` and `Pragma: no-cache`. No password, OTP, Silicon credential, application secret, or user-supplied actor ID is accepted by this route. Unscoped SLTs are supported. DM calls `oauth().authorizations()` to retrieve IAM-selected active memberships, rejects an empty set, and verifies the initial organization with live scoped introspection. The additive `organization_ids` response field lists selected organizations; `organization_id` is the initial workspace (first handle in sorted order) for existing clients. It grants no authority beyond live IAM consent.

## Requests and identity

Every normal authenticated API request supplies:

```http
Authorization: Bearer oat_REDACTED
X-Org-ID: tos
```

The organization must be among the token's selected, active memberships; every request introspects that specific organization. Direct IAM Carbon (`cat_`) and Silicon (`sat_`) session tokens are not DM application tokens. Refresh tokens are accepted only by the refresh/revoke session routes. OBO proof headers are rejected, including requests that also carry a bearer token. Authentication headers must occur once; duplicate, comma-separated, empty, or malformed values fail closed.

DM performs live IAM introspection on authenticated requests. It verifies:

- The token is active and unexpired, belongs to the configured application, and names the requested organization.
- The principal UUID, actor type, membership UUID, authorization epoch, scopes, organization, and audience agree between introspection and its authorization snapshot.
- The snapshot belongs exactly to the selected production or IAM testing environment.
- The returned public ID and Carbon/Silicon type are authoritative; request headers cannot replace them.

`GET /api/v1/auth/me` returns `actor`, `organization_id`, `principal_id`, optional `session_id`, optional `org_role`, and the effective `capabilities` array. Unknown or undisclosed roles do not grant access. Organization owner/admin authority is taken only from the live role, never from the test key or a cached user profile.

Before creating a conversation or reading another actor's presence, DM resolves recipients from its scoped IAM membership projection. Every fresh, cross-validated application-token introspection records the caller's principal, organization, membership UUID, public identity, membership version, and authorization epoch. Verified IAM `current.members` webhooks maintain other members and removal tombstones atomically with event deduplication. Older membership versions cannot overwrite newer snapshots; equal-version removals take precedence. Ordinary message/presence writes cannot reactivate a removed member. The projection grants recipient discovery only: every acting account still requires fresh IAM authorization. Ambiguous public IDs across actor types fail closed. A token represents its own authenticated actor on a WebSocket; distinct accounts use independently authenticated connections.

IAM 1.2.1 intentionally rejects application sessions on its first-party member and directory endpoints. The SDK currently offers a caller authorization snapshot and consent-filtered signed events, but no application-scoped lookup of an arbitrary organization member. Consequently, an offline recipient works after their membership has been supplied by sign-in or webhook. A recipient never supplied to DM returns 422 with an explanation to sign in; DM does not infer membership from a public identifier. Missing/delayed webhooks can leave a recipient projection stale, while fresh authentication still controls all actual senders and receivers. This limitation also applies immediately after cleaning a DM environment. See the [upstream member lookup proposal](iam-member-resolution-proposal.md).

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
tokens and recover by refreshing. The local runtime treats a WebSocket
handshake 401 as a reason to refresh the attempted access token, even before its
locally recorded expiry. If refresh itself is rejected, status changes to
`authentication_required` and a fresh IAM SLT is required. Granting application
consent from a different parent IAM session can invalidate older families'
refresh authority; reauthenticate those profiles rather than reusing rejected
credentials.

Revocation signals the backend's durable authorization revision and local socket revalidation. An IAM dependency outage fails closed; DM never manufactures sessions or falls back to a mock identity. Backend errors expose stable DM error categories and redact IAM provider details and credentials.

## IAM webhook verification and delivery

IAM sends the exact raw JSON body with these headers:

```text
X-Silicon-IAM-Event-ID
X-Silicon-IAM-Timestamp
X-Silicon-IAM-Key-Version
X-Silicon-IAM-Signature
```

DM uses the official SDK `WebhookVerifier` before accepting or acting on the event. A bounded test-envelope key is parsed into redacted secret storage only as a candidate-routing hint; it grants no authority. Signatures and the SDK's exact environment-key binding are verified before any test runtime is initialized or state is written. It verifies the HMAC over `timestamp + "." + exact_body_bytes`, a five-minute timestamp tolerance, exact signing key version, signature syntax, event/header ID consistency, unique security headers, and a maximum one-mebibyte body. Malformed or unauthenticated deliveries return an authentication failure and change no state.

A verified event ID is persisted transactionally before returning `204`. Duplicate IDs are safe to retry. Only normalized event identity, type, occurrence timestamp, and receipt timestamp are stored, never the raw envelope or its secrets. Each unique event advances a per-plane authorization revision in the same transaction. Local sockets re-introspect immediately; other backend processes observe that durable revision during their one-second replay tick. Every connection also revalidates on its heartbeat and before inbound application frames. Invalid/revoked sessions close with WebSocket code `4001`, reason `authorization-revoked`; unavailable authority closes with `1013`, reason `authorization-unavailable`. Heartbeat timeout remains independently `4000`, `heartbeat-timeout`.

This handles Carbon/Silicon logout, organization removal, membership suspension, consent changes, role changes, credential revocation, and other authorization changes without trusting stale webhook snapshots. An unaffected session remains connected when IAM confirms its authority. If a webhook is delayed, the next incoming frame or heartbeat still checks live IAM. IAM is the authority for authorization; webhook receipt is a prompt to recheck that authority.

## IAM testing environment binding

A DM testing environment requires a real IAM testing environment and an imported copy of `tos>dm`. Import the existing canonical app using the installed IAM CLI, which issues a fresh **test-only application secret** while preserving approved application configuration. Do not submit the production app secret as the test credential. The imported webhook configuration initially retains the registered callback and signing secret. To register a dedicated callback for an IAM test application, supply both optional `iam_webhook_secret` and `iam_webhook_key_version` in the DM environment creation body. The secret must contain 32–512 visible ASCII characters and the version must be positive. DM encrypts the override separately from the test application credential. Omit both to inherit the backend signer. This permits a local tunnel callback and independently rotated test signer without changing the production callback. Use IAM's returned webhook key version: changing a callback can advance it even when the secret is reused.

DM retains the IAM test environment ID, root key, canonical application ID, and fresh test secret encrypted in its testing registry. `IamClient::for_environment` permanently attaches the IAM root key using the SDK's `with_environment` equivalent. Its application credential is exclusively the imported test credential. At creation, DM calls the SDK's current-environment and application-directory operations to prove the key names the expected environment and the test credential works. It cannot retry a failed test request against production.

IAM test webhooks use a signed outer testing envelope containing the IAM environment key and a nested normal event. DM verifies the **outer bytes**, then uses the SDK's constant-time environment-key verification to identify the authenticated IAM testing environment. The SDK does not expose the received root key. Production rejects a testing envelope; test adapters reject production events and other environments' envelopes. After a configured test signer authenticates the envelope and its key, every active DM environment paired with that authenticated IAM environment receives the normalized invalidation, including pairings whose own configured signer is an older inherited version. Unrelated IAM environments and production receive no invalidation. Each target persists its event receipt and advances its own realtime revision; a partial database failure returns an error so IAM can retry the same event safely. Deleted/inactive environments cannot receive mutations. Do not log, copy into bug reports, or persist a raw IAM test webhook body.

See [testing environments](testing-environments.md) for DM environment creation, key rotation, cleaning, deletion, and recovery. IAM's complete external contract is in its [client documentation](https://github.com/teamofsilicons/silicon-iam/tree/main/docs/client).
