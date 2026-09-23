# IAM/Ting test credential mismatch observed during DM E2E

## Current status — 2026-09-23

IAM 3.0.3 and Honeycomb 0.3.3 are deployed. The initial IAM 3.0.2 fix
addressed the stale audience credential, which was traced to the encrypted import snapshot update running under a context that
could rotate the authentication digest but could not update that snapshot. IAM
now updates both atomically through a narrowly scoped database function; an
unavailable or malformed snapshot fails the rotation instead of leaving two
credentials out of sync. Restricted-role PostgreSQL regression tests cover the
successful update and rejected cross-environment or unauthorized writes.

In environment `1d32b4c6-dc84-4c44-b7b7-a16be7a31d06`, supported rotation
`3b01b022-ca4b-4610-92e2-abdd49f8b81f` was accepted as credential version 3.
A fresh official `silicon-iam-client = 3.1.0` signed OBO exchange returned that
exact new Ting credential and the matching IAM environment key. The returned
credential then authenticated IAM's testing-context endpoint for this environment
and `tos>ting`. No alternate secret was substituted; the diagnostic proof was
neither consumed nor persisted.

IAM 3.0.3 is now verified live on the main and scoped APIs with a healthy
worker. In original environment `d70c8674-6d2e-41d4-bf8d-96ddd882edbd`, supported
recovery definitively rejected old rotation
`a2b700ff-90dd-4795-b6c1-27e98871ba9c` before any credential mutation. After
reconciliation, new rotation `344d03bc-1d3d-45f7-a363-42b1464088ce` was accepted
as credential version 2. A fresh official SDK 3.1.0 signed OBO exchange returned
that exact credential and the matching IAM key; the returned credential passed
IAM testing-context validation for the original environment and `tos>ting`.
The saved old request was not edited, and no lifecycle fence was bypassed.
See [the supported recovery sequence](honeycomb-ting-e2e-issues.md).

These credential checks alone do not establish message delivery. Current release
and deployed delivery evidence are recorded in [the release record](release-0.10.0.md).

## Historical investigation

Observed 2026-09-23 IST (2026-09-22 UTC). The sections below preserve the original
failure, evidence and then-unverified hypotheses. That investigation itself did
not change production configuration or credentials.

## Fresh environment: proof succeeds, audience credential fails

Environment `1d32b4c6-dc84-4c44-b7b7-a16be7a31d06`, generation 1, accepted
DM and Ting configuration revision 2 before Ting credential rotation to version
2. DM's verified IAM testing context declared both `subscriptions.register` and
`tings.send`; the Carbon DM session contained both scopes and
`self.identity.read`. Ting's live OBO catalog returned the expected paths, empty
metadata, `critical: true`, and a 60-second TTL. These reads returned 200.

A fresh signed `subscriptions.register` exchange through the official
`silicon-iam-client = 3.0.0` succeeded. However, its returned
`testing_context.app_secret` differed from the newly rotated Ting credential.
The returned IAM environment key matched the selected environment. Authenticating
that returned audience credential against IAM's testing-context endpoint failed
with **401 `invalid_client`**. DM therefore returned **503
`dependency_unavailable` / `dependency iam is unavailable`** on explicit Carbon
and Silicon recipient registration attempts. The publisher likewise retained
handoffs with `ting_authority_unavailable`.

Safe correlation evidence:

- Root-key selector reproduction: IAM request
  `01a0cacc-6cef-70e4-bc9c-8c3e6990a96c` failed audience validation.
- Exact DM selector reproduction: IAM request
  `01a0cacd-11e3-74f2-8ddf-f25a4ccd9e7c` failed identically. Catalog retrieval took
  748 ms, proof issuance 512 ms, and audience validation 245 ms with a five-second
  request timeout. This attempt did not fail from timeout.

## Reproduction boundary

1. Build the official IAM client with DM application credentials and the same
   five-second timeout as the local DM candidate. Select testing using
   `with_testing_application("tos>dm", dm_test_secret)`.
2. Fetch `obo().endpoints("tos>ting")`. Prepare exact registration JSON bytes
   containing `org_id`, `app_id` and `for`; retain their SHA-256 binding.
3. Call `obo().exchange_signed(...)` with the verified recipient's DM access
   token, endpoint `subscriptions.register`, metadata `{}`, method `POST`, that
   body digest, and a new mutation idempotency key.
4. Use **the credential returned by this fresh exchange** with
   `with_credential(Credential::Application { app_id: "tos>ting", ... })`, then
   `with_environment(EnvironmentKey::new(returned_iam_test_key))`. Call
   `applications().testing_context()` and require the selected UUID and `tos>ting`.
5. Stop when this validation fails. Do not submit the proof to Ting, substitute
   another secret, relax validation, or treat the failure as successful enrollment.

The diagnostic proof was neither consumed nor persisted. No access tokens,
refresh tokens, application secrets, environment keys or proof bytes belong in
this report. Only status, error code, timing, equality results and request IDs
were emitted by the diagnostic helper.

## Source paths to investigate; deployment identity unverified

Local IAM source inspected at `176ece3fb87c1f58f6b233a3ae951e13676bf31c`:

- `src/features/applications/obo.rs:355` obtains the response's testing context
  through `testing_environments::obo_context`.
- `src/features/testing_environments/graph.rs:448` reads and decrypts the audience
  import credential via `iam_private.get_testing_application_secret`.
- `src/features/testing_environments/honeycomb/testing_apps.rs:564` rotates the
  test credential and already calls `record_rotated_application_secret` at line
  586. That helper invokes `iam_private.update_testing_application_secret` in
  `src/features/testing_environments/graph.rs:435`.

Investigate the deployed revision and consistency between the rotated credential
and the encrypted import credential returned to OBO callers. The inspected source
already contains a snapshot-update call; these observations do **not** establish
that it is absent, defective, or deployed on the live server. The confirmed
failure is the returned credential mismatch and its subsequent IAM rejection.
DM's relevant guard is `src/infrastructure/ting_enrollment.rs`:
`validate_testing_context`, also used by `src/infrastructure/ting_proof.rs`.

## Original environment: independently verified control

In original environment `d70c8674-6d2e-41d4-bf8d-96ddd882edbd`, generation 1,
a new official-SDK proof exchange returned audience credentials that passed IAM
testing-context validation. Using only that newly returned, verified context,
actual Ting Carbon and Silicon logins succeeded. Both authenticated `/v1/me`
responses returned the exact typed actor, environment UUID and generation 1.
This was **not a cached-secret fallback** and did not consume the diagnostic proof.

At the time of this control, the original environment's Honeycomb
credential-rotation operation remained a separate pending issue documented in
[honeycomb-ting-e2e-issues.md](honeycomb-ting-e2e-issues.md). No pending operation
was manually cleared and no IAM or Ting lifecycle fence was bypassed. Successful
login/context checks establish usable authority for further testing; they do not
by themselves prove message delivery, restart recovery or revocation recovery.
