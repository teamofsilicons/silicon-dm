# Session reliability release — 22 September 2026

Final CLI release: **0.9.6**. The production backend and browser deployment revisions below remain unchanged by this CLI follow-up.

DM 0.9.5 is deployed and published. Source: `7fffb88c5de349a7970232023ebd9710bab5e32f`, tag `v0.9.5`.

## Deployment

CloudFormation `silicon-dm-production` reached `UPDATE_COMPLETE`. API and worker task definitions 18 completed their rolling deployments, with no failed tasks. Both running tasks were checked against:

`234951665042.dkr.ecr.us-east-1.amazonaws.com/silicon-dm-production@sha256:0b1ed1a133594a518c44d6206835efa743aaa9447cbff6a3d8a42cb841997b36`

The gateway on `i-06b6670e5b2c53a3d` runs:

`234951665042.dkr.ecr.us-east-1.amazonaws.com/silicon-dm-production@sha256:35d07d210b6e498f00e8301f4dda497efe28e6cce836d5e22de559658727bc9f`

Gateway source was built from the isolated release tag and added to its existing immutable runtime; dependency manifests and lockfile were unchanged. Its existing session volume, runtime environment hash, process-owner lock and service configuration were retained. The service pins the new digest and the previous unit is backed up as `.before-session-20260922`. The static Vercel frontend had no source change in this release.

The reviewed change set modified only the API/worker task definitions and services. No database migration, credential rotation or data rewrite was required. Existing images remain available for rollback. The gateway's current systemd unit survives reboot; future gateway reprovisioning must select the new digest rather than the older image in historical infrastructure parameters.

## Verification

- Public `/live` and `/ready`: 204; gateway `/healthz`: 200; ALB target healthy.
- `/api/v1/contracts` reports service version 0.9.5 and the existing compatible contracts.
- [ARM64 deployment build](https://github.com/teamofsilicons/silicon-dm/actions/runs/35662257632) and [six-platform CLI build/tests](https://github.com/teamofsilicons/silicon-dm/actions/runs/35662250868) passed.
- Full CI exposed one test fixture missing the newly added refresh timestamp field. Test-only follow-up `e7c077e` fixed the fixture; [full CI](https://github.com/teamofsilicons/silicon-dm/actions/runs/35663436837) passed. Runtime source is identical to the released tag.
- Maharaj's existing session remained authenticated. After the supported daemon stop/start, runtime 0.9.5 reconnected the same identity and returned the same three complete conversation records. Pending queue counts were unchanged. No messages were sent.

## Publication

[GitHub release 0.9.5](https://github.com/teamofsilicons/silicon-dm/releases/tag/v0.9.5) is public. Honeycomb accepted `tos>dm` 0.9.5 as release `f469824c-18cc-4e2e-87ff-f291ea1583a3`. Archive SHA-256: `7d4438a78a59ba256760b67a8cb26c701f7492acd56465dad6cfa2e65a913b43`. All six platform binaries and the manifest were validated; GitHub asset sizes and digests match the locally verified archive. A fresh anonymous default-latest installation resolved 0.9.5 and passed native macOS ARM64 version/help checks.

Registry packages `silicon-dm-protocol`, `silicon-dm-client`, `silicon-dm-cli` 0.9.5 were downloaded from crates.io and checked for the exact clean tagged source revision.

Maharaj's CLI and daemon now run 0.9.5. The old daemon was PID 19436; the activated daemon was PID 90012. Successful CLI replacement alone did not reload the old daemon.

## Early access rejection follow-up

CLI 0.9.6, source `1e275d35c57f1e6325bfedb1fdce0691fc68bb9c`, recovers access rejected before its saved expiry with one forced refresh and one status retry. Only live inactive/401 responses trigger this recovery; permission denials and provider outages remain errors and retain saved credentials. Successors are saved before use.

DM invalidates only the rejected access generation and renews under the existing profile lock. Its relay already retries unauthorized operations using the same durable request.

Targeted regressions and Clippy with warnings denied passed. Final Maharaj activation is pending because reading its retained session file blocks at the filesystem level. A bounded Hook activation attempt stopped at its first read-only daemon-status command, before package update or daemon stop; existing daemons were left running. No session was deleted, rewritten, logged out or replaced.

[Final CLI full CI](https://github.com/teamofsilicons/silicon-dm/actions/runs/35666138449) passed. All six native builds in [release CI](https://github.com/teamofsilicons/silicon-dm/actions/runs/35666138518) passed at the tagged source revision. The archive contains only the manifest and six native executables.

[GitHub 0.9.6](https://github.com/teamofsilicons/silicon-dm/releases/tag/v0.9.6) is public; every release asset size and SHA-256 matches its locally verified file. Honeycomb accepted `tos>dm` 0.9.6 in the production channel, release `3aa449f3-9390-48d5-aa0f-465588cea65f`. Archive SHA-256:

`29cadae43c1115e884e28220c94282dd387d8b1261224059169465c7b5c502c4`

A fresh anonymous default-latest install resolved 0.9.6 and passed native macOS ARM64 version/help checks. `HONEYCOMB_NO_SERVICE=1` prevented installing a background service in the disposable verification home.

New registry archives were downloaded independently and verified against the clean tagged source:

- `silicon-dm-client` 0.9.6: SHA-256 `86708786a32f36ff0184a90c25b314cfff910d964ff016171418e342a5206e92`.
- `silicon-dm-cli` 0.9.6: SHA-256 `add34a36683b8a08b700cf088c18970e91c8b27ecf62fa0e2cd1e63fc78af5e5`.

Machine-readable final CLI release evidence: `/tmp/session-deploy-dhr-20260922/dm-final-release-proof.json`. The retained Maharaj package rollout is coordinated separately so its existing session and package homes stay intact.
