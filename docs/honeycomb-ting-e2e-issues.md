# Honeycomb issues found during DM–Ting E2E setup

## Current status — 2026-09-23

Honeycomb 0.3.3 and IAM 3.0.2 are deployed. Honeycomb now preserves the distinction
between its positive local configuration revision and IAM's accepted imported
revision 0. New rotations bind the accepted IAM revision before saving the exact
request; reconciliation recognizes the proven import mapping. A definitive
configuration-revision rejection can terminate an old pending rotation without
rewriting its immutable saved request or fabricating a success receipt.

A fresh authorized Ting rotation in environment
`1d32b4c6-dc84-4c44-b7b7-a16be7a31d06` was accepted as credential version 3,
and fresh official SDK OBO validation passed with the returned audience credential.
See [the credential repair evidence](iam-ting-e2e-issues.md).

IAM 3.0.3 is now verified live on the main and scoped APIs with a healthy
worker. The original operation was recovered through supported Honeycomb commands
under its original `dm-ting-tester` actor:

1. Exact operation `a2b700ff-90dd-4795-b6c1-27e98871ba9c` recovery returned
   `rejected` with `testing_configuration_revision_conflict`; a fresh operation
   read confirmed that terminal state. IAM confirmed rejection before changing
   the credential, despite the old request's environment revision 19 and the
   current revision 21.
2. Application reconciliation was accepted. The imported local configuration
   revision remained 1; IAM's accepted import mapping was preserved.
3. A new rotation with a new saved idempotency key,
   `344d03bc-1d3d-45f7-a363-42b1464088ce`, was accepted as credential version 2.
4. A fresh official SDK 3.1.0 signed OBO exchange returned that exact new Ting
   credential and the matching IAM key. The returned credential authenticated
   IAM's testing-context endpoint for `d70c8674-6d2e-41d4-bf8d-96ddd882edbd`
   and `tos>ting`.

No saved request or database state was manually changed. The diagnostic OBO
proof was neither consumed nor persisted, and no alternate secret was supplied.
Both the original pending-operation recovery and the fresh-environment audience
credential mismatch are resolved. Message delivery is validated separately.

Current release and delivery status are in [the release record](release-0.10.0.md).
The historical observations below explain the original fault and are not claims
that every described limitation remains in the deployed versions.

## Historical investigation

Checked 2026-09-23. These Honeycomb/IAM test-application administration issues
were observed independently of Ting message delivery. The investigation below
used source inspection and read-only checks; it did not itself retry rotations,
change lifecycle state, or deploy a repair. No credentials are included.

## Affected operation and observed state

- Shared testing environment: `d70c8674-6d2e-41d4-bf8d-96ddd882edbd`.
- Application: `tos>ting`.
- Secret rotation operation: `a2b700ff-90dd-4795-b6c1-27e98871ba9c`.
- The rotation response reported `state: pending` and
  `error_code: revision_conflict`.
- A fresh read of that operation confirmed `kind: secret.rotate`, `revision: 1`,
  `state: pending`, and `error: revision_conflict`.
- A fresh application read returned `revision: 1`, `effective_revision: 1`,
  `iam_revision: 1`, and `state: active`. Active application state does not mean
  the pending rotation completed.
- The earlier reconciliation attempt returned `state: pending` with
  `error: IAM omitted a positive lifecycle version`.

Safe read-only reproduction of the projection and operation:

```sh
honeycomb --test d70c8674-6d2e-41d4-bf8d-96ddd882edbd --json apps get 'tos>ting'
honeycomb --test d70c8674-6d2e-41d4-bf8d-96ddd882edbd --json operations get a2b700ff-90dd-4795-b6c1-27e98871ba9c
```

Do not run `apps reconcile`, `rotate-secret`, or operation recovery merely to
inspect state: these issue management mutations or replay an existing mutation.

## Cause supported by source inspection

IAM's imported applications begin with `honeycomb_configuration_revision: 0`.
The import insert leaves that column at its default. Honeycomb deliberately maps
the accepted import to its own isolated configuration revision, initially `1`.
The import-mapping test explicitly covers an IAM revision `0` mapped to a positive
Honeycomb configuration revision.

The rotation path then sends Honeycomb's configuration revision `1`. IAM requires
the submitted revision to equal the imported application's stored configuration
revision and rejects a mismatch with `configuration_revision_conflict` before
generating a replacement secret. Honeycomb records the mapped
`revision_conflict` error and leaves its operation pending.

Reconciliation fails separately: Honeycomb's `testing_configuration` calls
`positive(record, "configuration_revision")`, which rejects imported revision
`0` with the observed lifecycle-version error.

This explains the live errors, but the affected application's protected IAM
record was not read directly with service credentials. Its current IAM
configuration revision `0` is a source-supported inference, not a claimed direct
database observation. `iam_revision` is a different counter and remains `1` in
Honeycomb's observed projection.

Relevant source locations:

- IAM: `migrations/0078_app_testing_layer_and_cross_org_obo.sql` import insert;
  `migrations/0097_honeycomb_management.sql` revision default;
  `src/features/testing_environments/honeycomb/testing_apps.rs`,
  `rotate_in_test` and mutation validation.
- Honeycomb: `crates/server/src/iam_management/testing.rs`, `map_imports`;
  `crates/server/src/iam_management/application.rs`, `testing_configuration`,
  `testing_mutate`, and `testing_operation_result`;
  `crates/server/src/secrets.rs`, `rotate` and `recover_result`.

## Recovery limits

Same-key rotation retries and operation-result recovery replay the immutable
saved configuration revision `1`; they do not repair its revision precondition.
There is no public cancellation, abandonment, or rebase endpoint for the pending
rotation in the inspected implementation.

The pending operation also prevents changing Ting's test configuration, importing
applications (including an unchanged application attachment), and starting
environment lifecycle actions. These guards are in Honeycomb's `api.rs`,
`imports.rs`, and `control.rs`. Reconciliation does not provide an escape: it
currently rejects the imported revision and does not cancel a secret rotation.

IAM has a separate credential-recovery contract for an application's existing
test secret. It requires legitimate Honeycomb service authentication plus that
same application's production authentication. A test user or environment root
key alone cannot use it. Even legitimate recovery would not clear this pending
Honeycomb operation; the public Honeycomb attachment path is blocked before
reaching that recovery contract.

## Remediation ownership and supported testing workaround

Honeycomb's test-application adapter owns the revision mapping, reconciliation,
and pending-operation recovery issue. IAM owns the imported-revision and rotation
contract, including its rejection of nonpositive configuration revisions. A
coordinated fix must preserve distinct revision counters and exact operation
identity, support valid imported configuration state, and provide an explicit
recovery outcome for existing pending operations. It must not fabricate a
receipt, edit database state manually, or silently replace operation authority.
No Honeycomb/IAM repair or deployment was made by this investigation.

For delivery validation, use a fresh task-owned environment and submit a supported
test application configuration for Ting before requesting any secret rotation.
Wait for that configuration and its participant activation to be accepted, then
rotate against the current accepted revision. Apply the same order to other
imported applications whose credentials need rotation. This establishes a
positive accepted configuration revision through the normal APIs.

At the end of the historical investigation, the original environment was
retained and its rotation operation remained pending and documented. Testing in a fresh environment does not repair that operation or
establish that the underlying Honeycomb issue is resolved. Real message E2E results must be reported
separately after the delivery flow is exercised.
