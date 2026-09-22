# Honeycomb issues found during DM–Ting E2E setup

Checked 2026-09-23. These are Honeycomb/IAM test-application administration
issues, not evidence of a failed Ting message delivery. No credentials are
included. The investigation below used source inspection and read-only checks;
it did not retry rotations, change lifecycle state, or deploy a repair.

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

Safe read-only reproduction of the current projection and operation:

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

The original environment is retained, and its rotation operation remains pending
and documented. Testing in a fresh environment does not repair that operation or
establish that the underlying Honeycomb issue is resolved. Real message E2E results must be reported
separately after the delivery flow is exercised.
