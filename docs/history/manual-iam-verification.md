# Manual IAM verification

These checks were chosen and executed individually against the locally running DM backend and the real Silicon IAM testing service. No automated scenario suite or mock identity provider was used. Private credentials, raw signed envelopes, and session files remain outside the repository.

## Real webhook delivery

A temporary tunnel forwarded only the test application's callback to local DM. Production webhook configuration was unchanged. The imported IAM test application used a dedicated callback signer at key version 2; one DM pairing stored that signer, while other pairings retained inherited signing settings.

An actual IAM Silicon profile mutation emitted `organization.silicon.updated.v1` with a complete membership projection at membership version 2. DM returned 204. Database inspection confirmed the active principal projection and one receipt in both the primary and dedicated-signer DM pairings, with no matching production receipt. A third pairing had been cleaned after the original callback, explaining its initially empty receipt/projection state.

The original captured bytes and signature were manually replayed while within the five-minute signature window. DM returned 204. Existing pairings retained exactly one receipt and unchanged authorization revisions. The cleaned pairing accepted the event once for its new generation, gained the active version-2 projection, and advanced its revision once. This demonstrated fanout to all three matching active pairings and idempotent processing in each database plane.

| Individually selected request | Observed result |
| --- | --- |
| Exact real signed delivery replay | 204; existing receipt count and revision unchanged |
| Append one whitespace byte without resigning | 401 |
| Change one hexadecimal signature digit | 401 |
| Correct HMAC over an expired timestamp, 301 seconds old | 401 |
| Correct HMAC over an envelope carrying an unknown IAM testing key | 401 |

The expired-timestamp and unknown-key requests were manually signed using the dedicated test signer. Its key was first confirmed internally against the actual captured IAM signature. No secret or raw testing key was printed. After all rejected requests, every inspected receipt, authorization revision, and membership projection remained unchanged; the production receipt count remained zero.

## Application-session boundaries

| Individually selected request | Observed result |
| --- | --- |
| Valid organization-bound Carbon app session on `/auth/me` | 200; exact saved actor and current owner role |
| Valid Bearer plus `X-IAM-OBO-Access-Proof` | 401 |
| OBO proof without Bearer | 401 |
| IAM test-plane access token against production DM | 401 |
| Production access token against a DM testing plane | 401 |
| Valid token with a different organization header | 401 |
| Valid token with forged actor-ID and actor-type headers | 200; identity remains the IAM token subject |

IAM may disclose an `obo.issue` scope in a token's effective scope list. DM does not interpret that scope as an inbound authentication method and exposes no OBO endpoints.

## Live session revocation

The installed IAM CLI minted a fresh Silicon application SLT using an isolated `SILICON_IAM_HOME` and a hidden Silicon-token prompt. The shared Carbon profile and existing DM session files were preserved. DM exchanged this new SLT successfully (200), and the public Rust client opened a WebSocket in the dedicated-signer DM environment, receiving protocol version 2 and the current testing generation.

Logging out only this new family's refresh token returned 204. Its connected WebSocket closed with code 4001 and reason `authorization-revoked`. A subsequent `/auth/me` request using the same access token returned 401. No existing Alice, Bob, or Silicon session family was revoked for this check.

## Integration findings and fixes

The first live login exposed an incorrect local assumption that the SLT's wire prefix was `slt_`. Official IAM returns a short-lived authorization code with the `oac_` prefix. DM now bounds SLTs as opaque non-whitespace ASCII strings and delegates their validity to IAM. Access and refresh tokens retain the official `oat_` and `ort_` contracts.

Live conversation creation exposed that IAM's first-party member and directory routes intentionally reject application sessions. DM now persists complete caller membership bindings from fresh introspection and applies verified member webhooks, preserving versions and removal tombstones. Known recipients can be offline. A member never supplied to DM fails closed with an explanation to sign in; the missing upstream lookup capability and required disclosure policy are documented in the [member-resolution proposal](iam-member-resolution-proposal.md).

These results do not claim production webhook activation, production deployment, or first-contact discovery of members whom IAM has never disclosed to DM. Sender and receiver authorization still require fresh IAM token introspection.

After the callback exercise, the temporary Cloudflare tunnel and its restricted
local proxy were stopped. The IAM test application's temporary callback URL
therefore needs replacement before another live callback run. The production
application's registered endpoint and supplied signing secret were preserved.

Code review also confirmed that IAM permanently reserves public identity handles and reactivates the same durable organization/principal membership on rejoin. The projection refuses changed UUID bindings, decreasing membership versions, and decreasing disclosed epochs; equal-version removals win. A late introspection response must still match the stored membership version and epoch before its request context is accepted, preventing a newer webhook's known authorization state from being overwritten by an older in-flight response.

## Expired CLI family and recovery

The original Alice CLI family eventually returned 401 on refresh while the
newer independent SDK login remained usable. IAM source inspection identified
a matching mechanism: granting the same application consent from a different
parent IAM session retargets the shared consent record; an older family's
refresh then fails its parent-session check, although access introspection can
remain valid until expiry. The historical parent IDs were not available, so
this mechanism is a supported explanation rather than a proven incident cause.

The relay now reports `authentication_required`, a safe stage/status/code, and
a fresh-login recovery instruction. The installed IAM CLI minted a new SLT;
logging into the existing Alice profile succeeded, explicit refresh succeeded,
and all four enabled CLI profiles returned to `connected`. Alice retained her
device ID and queue contents. No IAM authorization checks were bypassed.

## Sibling access revocation and automatic recovery

The later delayed-logout check exposed a separate, confirmed IAM behavior:
revoking one refresh family also revokes access tokens for the same parent
session and application. IAM's refresh-family revoke path invokes its
session/client access-token revocation query; sibling refresh families remain
active. This differs from the consent-parent refresh failure above.

The independent SDK profile then returned 401 with its current access token,
although its saved expiry was still in the future. Before the fix, the relay
kept retrying the rejected WebSocket handshake until a refresh was otherwise
triggered. It now expires only the exact attempted access token on handshake
401, then refreshes under the existing profile lock. A newer concurrent login
is protected by the token comparison; 403 does not trigger this invalidation.

Restarting the patched SDK relay with those same saved credentials recovered
without another SLT: its access token changed, its device ID was preserved,
status became `connected`, `/auth/me` returned 200, and both pending queue counts
were zero. The old local expiry was still in the future at verification. Safe
handshake status/code diagnostics are now available without exposing response
headers, tokens, or URLs.

## Public AWS callback verification — 2026-09-06

The production webhook was approved through the installed IAM CLI using the
user-provided email step-up. IAM reports the exact trailing-slash production
URL active with signing version 1 and no OBO endpoints. The paired IAM test
environment uses the same public path with `?environment=testing`, signing
version 3. The query is not authority; the signed envelope selects the plane.
An earlier retired URL in that IAM test plane prevented reusing the identical
URL, so this distinct query preserved endpoint history without changing routing.

A manual test Silicon display-name update and exact restoration generated four
real IAM events. The AWS testing database committed both
`organization.membership.updated.v1` and `organization.silicon.updated.v1`
events for each change within about two seconds. Production retained zero
receipts for these signed testing envelopes, confirming plane isolation.
Application display-name update/restoration was also attempted: IAM configuration
control events do not belong to the Application data-projection vocabulary, so
this is not evidence of production callback delivery.

The production verifier was separately exercised over public HTTPS using a
locally constructed `dm.manual_probe.v1` event signed with the configured
production key: valid request 204, identical replay 204, altered signature 401.
This is a manual cryptographic/receiver check, not an IAM-originated production
data event. Real IAM-originated delivery was verified through the paired testing
plane above; no real user's production profile or permissions were changed.

A read-only RDS inspection after the manual signed replay confirmed exactly one
production probe receipt and exactly four test-plane receipts. The duplicate
probe and invalid signature did not create additional receipts or cross planes.
