# Ting delivery release — 23 September 2026

## Initial production cutover

DM backend and gateway 0.10.0 were deployed from immutable source `e2b0b5d487c2c0b0fb8f0154bd78033319f5af99` (`v0.10.0`). Native publication remained held for deployed acceptance, which identified the managed testing-generation and receiver organization-mapping follow-ups below.

The ARM64 image workflow [35793975290](https://github.com/teamofsilicons/silicon-dm/actions/runs/35793975290), six-platform native workflow [35793974725](https://github.com/teamofsilicons/silicon-dm/actions/runs/35793974725), and both source/tag CI runs passed. Archive SHA-256: `2b843414cf58409cf944a244f886fdb3fc59bd0a353449de570e8c8da4764642`.

Images use repository `234951665042.dkr.ecr.us-east-1.amazonaws.com/silicon-dm-production`:

| Component | Immutable digest |
| --- | --- |
| Backend 0.10.0 | `sha256:178642ab78dadc9c54f775998a1793de52748026f90b7599422dac6d445ffb4c` |
| Gateway 0.10.0 | `sha256:1b3ef84b4e29a061a44082c8a4cbc107c0d07f59ff4dfda1e8f6c913f1187e74` |
| Bootstrap 0.10.0 | `sha256:5380b75fa5c171aa6d0a4ded8cf1ac72296a6b4d166deb906d58fb743e8ce588` |

CloudFormation `silicon-dm-production` reached `UPDATE_COMPLETE`, API and worker definitions 20 each healthy at 1/1, bootstrap 17 completed with exit 0. Public `/live` and `/ready` returned 204; discovery/contracts returned 200 and service version 0.10.0 with HTTP contract 3. Retired `/api/v1/ws` and `/api/v1/ws/shared` returned 410 `delivery_moved_to_ting`.

## Database and credential continuity

Encrypted production and testing RDS snapshots `dm-production-pre-ting-0100-20260923-e2b0b5d` and `dm-testing-pre-ting-0100-20260923-e2b0b5d` were available before migration. Migrations 28–34 and runtime grants were rehearsed transactionally and rolled back; this was a SQL rollback rehearsal, not a restored-snapshot rehearsal. All prior migration checksums and table fingerprints remained unchanged by that rehearsal.

Both old tasks actually reached `STOPPED` before bootstrap ran. The API finished its ALB drain at 13:04:42 IST and reached STOPPED 13:05:05; bootstrap started 13:06:18. A preliminary baseline at 13:03:08 preceded the end of drain despite service counts reporting zero. The four auth/projection/session fingerprint differences were audited to13:03:45, during the old API drain. All 31 other original table fingerprints, including messages/history/receipts, were unchanged; contract retirement was the expected migration change. No old writer overlapped actual migration.

The migration ledger reached 34 with 33 rows (0009 is intentionally absent). Every checksum matched the released source. Restricted runtime role attributes and CRUD grants passed; `system_events` remained inaccessible. Runtime secret field hashes, database passwords, test encryption key, and Honeycomb service credential were unchanged.

## Gateway continuity and durable infrastructure

Gateway host `i-06b6670e5b2c53a3d`, state volume `vol-0fb520477e7c45cf6`, and filesystem UUID `5182cbdf-c219-4984-a21e-45554e187282` were retained. Its root and state volumes remained encrypted. The cold backup is `/var/lib/silicon-dm-gateway-backups/ting-0.10.0-20260923-e2b0b5d`, root-only, with state archive SHA-256 `858e4394dc95bcac1704076965c3acef4ad38c4076b3c528741c3609b8621ab5`. EBS snapshot `snap-0a5509d6489ee4232` completed.

The runtime environment SHA-256 remained `14a62b5120d84549501ae7d4ac77b60789e3b96fff54460164027bdf8627c6a7`; session storage and the exclusive process lock were preserved. SSM rollout `1621f677-b0fc-4a4f-b4b5-782a5b6ebf04` succeeded. Public gateway `/healthz` returned 200 and its ALB target was healthy.

CloudFormation `silicon-dm-web-production` reached `UPDATE_COMPLETE` with the gateway digest pinned. The template now accepts an explicit AMI ID, retaining `ami-07987a01dcdb011ef`; the previously configured moving SSM latest-AMI reference would have resolved to a different image during this update. Existing stack-policy denies on instance/state-volume replacement or deletion remained enforced. The reviewed update retained the original host, AMI, and both volumes.

## Managed generation follow-up

Backend 0.10.1 runtime source `82412e9a21ae6f1ee52f02ba9efbe9ddb14d77d5` (generation fix `af49924`, worker recovery `a099c63`) separates shared Honeycomb generation from DM's private `runtime_revision`. Same-generation credential/lifecycle changes invalidate cached request/worker authority. Verified discovery repairs earlier managed metadata drift without deleting sandbox data; clean advances generation. Migration 35 adds and initializes the private revision, and is excluded from per-sandbox business-data schemas. It does not add message or client protocol fields.

Idle sandbox worker polls and maintenance now retain the local lifecycle fence without making IAM requests. Each actual handoff checks live IAM readiness inside the lease deadline, before publication. IAM 429/503 leaves the immutable claim pending with backoff and the cached worker running; local epoch revocation still stops the worker.

The regression covers zero IAM calls while idle, outage and rate-limit recovery with identical request bytes/key, cross-process cache invalidation, stale request/worker rejection, import/rotate-key/disable/restore generation stability, retained data during supported repair, and clean advancing generation. The final full backend suite passed serially; workspace Clippy passed. A parallel run had two local Docker fixture port-allocation failures; the serial rerun passed those backend scenarios. The final native consumer checks are tracked separately with the receiver organization fix.

The final ARM64 image workflow [35836473223](https://github.com/teamofsilicons/silicon-dm/actions/runs/35836473223) passed. The complete artifact was verified against its published SHA-256, source/version labels, Docker config digest, and ARM64 platform before publication. The matching bootstrap retained identical `dm-api`, `dm-worker`, and `dm-migrate` binaries and exact reviewed bootstrap/grant scripts.

| Component | Immutable digest |
| --- | --- |
| Backend 0.10.1 | `sha256:73b158309b90da8d43811ad1f863c47b796350e04b168eb5a2d2982f9c1a861f` |
| Bootstrap 0.10.1 | `sha256:e9c24a0dbc33f95565eb900a838ea821933b961d73478d0550e291a65432494c` |

Migration 35 is additive, but a mixed semantic rollout was avoided because old backends can still advance the wrong managed generation. Both captured old task ARNs reached actual `STOPPED` before rehearsal: worker `1bdf0712ba564c5e97505481aeb67286` at 14:14:24 IST, API `ca0c610203a84112bd4dc3a5953411aa` at 14:18:01 IST. No old writer overlapped migration.

Encrypted RDS snapshots `dm-production-pre-generation-0101-20260923` and `dm-testing-pre-generation-0101-20260923` were available. Rehearsal task `ac54b528ef4743ce96f51f8ad2f0ad8f` applied migration 35 and grants in a transaction, rolled back, and captured the fully paused baseline with all original data unchanged. Exact bootstrap definition 18, task `d3e1d73d9c4542d0a36d857e0c5e76fa`, exited 0 and logged completion. Postflight task `e0c8c6e866194d2dba706090f807b244` exited 0: ledger head 35 with 34 rows, all source checksums matched, all 40 original production-table fingerprints unchanged after excluding the added internal column, every runtime-secret field hash unchanged, and restricted role/grant checks passed. Existing sandbox business-data schemas exclude migration 35; the new field belongs only to production control metadata.

CloudFormation `silicon-dm-production` reached `UPDATE_COMPLETE`. Its backend and bootstrap image parameters are the only changed parameter values; desired count is 1, and all other protected settings remain identical. New container definitions differ only in image from their previous definitions. API and worker each run definition 21 at 1/1 with no pending tasks and the exact backend digest above:

- API task `e8ad3f0ed9fc43dfa42839b18f518bbe`, ALB target `10.80.158.103` healthy.
- Worker task `1d83e06678074079ab617c037ecc7955`, running with the verified digest.

These definitions have no ECS container healthcheck, so ECS `UNKNOWN` is expected. Readiness is demonstrated by public `/live` and `/ready` returning 204, discovery/contracts returning 200 with service version 0.10.1 and HTTP contract 3, and the exact new API's healthy ALB target. Both old websocket routes continue returning 410. The gateway remains on its verified 0.10.0 image; this backend patch does not replace it.

Actual deployed DM/Ting acceptance passed with exit 0 using real Carbon and Silicon test identities at the shared generation 1. It verified bidirectional Ting websocket/inbox delivery and authorized DM hydration, message/enrollment idempotency, explicit read receipts without automatic DM reads, HTTP synchronization, cross-actor 409 rejection, presence, and retired websocket 410 responses. This is separate live delivery evidence beyond the health checks. Native publication and installed verification subsequently passed as recorded below. Opaque cursors and credentials from the private test report are not reproduced here.

Machine-readable image, task, database, gateway, and protected-setting proofs are retained under `/tmp/ting-rotation-release-20260923/dm-artifacts`; no secret values were logged or committed.


## Publication, native acceptance and final documentation

Protocol 0.10.0 and client/CLI 0.10.1 are published on crates.io. The immutable
[v0.10.1 release](https://github.com/teamofsilicons/silicon-dm/releases/tag/v0.10.1)
uses source `85eaf145a961462e5931e1040a267e32a17fa59f` (the backend source above
plus documentation updates). All six platform builds and tagged CI passed.
Honeycomb accepted release `8d00c1ea-ca5f-45cd-9d7a-c3456ae679bf`, archive SHA-256
`2320374cf8352ede719a3fe57cace81bf1a899062c0622ff51c10259c0cb7b77`.
All public crate and GitHub assets were independently hash-verified.

The fresh Honeycomb installation and existing global DM installation both run
0.10.1 with the expected binary hash. Supported global update reports current;
existing DM profiles, logical queue/cursor/inbox/generation rows and relay PID
were preserved. The published binary passed 17 native checks, including a
503-to-204 callback retry with identical Ting item/key, one durable consumer
effect, stable hook reconnect, and separate delivered/read receipts. The two
existing Ting bindings and daemon were unchanged. Running Ting daemon 0.1.2 is
distinct from installed Ting CLI 0.1.4; no shared-daemon restart was performed.
See the committed [32-check backend evidence](../../docs/ting-deployed-live-verification.json)
and [17-check native evidence](../../docs/ting-native-release-verification.json).
Both sanitized reports are also public GitHub release assets.

Task-owned callbacks, relays and hook processes were stopped; native DM/Ting
profiles and application sessions were logged out. Root's two additional DM refresh-family revocations
returned 204 and its two Ting session revocations returned 200. Existing user
profiles and processes were retained.

The existing Vercel project `silicon-dm-docs` published all 40 documentation pages
from documentation source `ba8b455` at
[docs.dm.teamofsilicons.com](https://docs.dm.teamofsilicons.com/), deployment
`silicon-dm-docs-2vkgcfckf-saketdev12-5675s-projects.vercel.app`. Build/link checks
passed; seven public documents/assets match local bytes, including the release
record, integration issues, IAM/Honeycomb issue records, OpenAPI and full text.
The static-site CSP was verified. See [documentation proof](dm-docs-live-2026-09-23.json).

DM and Interface websites are deployed with canonical Ting organization
resolution. The remaining production Bricks dependency is Ting's cross-organization
type lookup during sends. The current release proves live delivery in the
explicit task-owned test environment, not production Bricks delivery. No Ting
code or management-authorization change was made in this release.
