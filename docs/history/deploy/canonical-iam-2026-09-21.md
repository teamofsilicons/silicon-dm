# Canonical IAM compatibility deployment, 2026-09-21

Source: `c58729f000a5648472236f0e08ad1500e42359f7`.

The API and worker now authenticate canonical IAM actors and resolve their
existing DM membership rows through public membership handles. Migration 27 adds
these bindings without replacing DM's independent resource identifiers. Existing
messages, conversations, memberships, and installed 0.9.4 clients remain usable.

Both ECS services were paused for encrypted native RDS snapshots:

- `silicon-dm-before-canonical-20260921`
- `silicon-dm-testing-before-canonical-20260921`

Both snapshots became available before migration. The production schema and all
12 retained testing schemas passed a transactional rollback rehearsal with their
actual database owner roles and historical migration checksums. The final apply
task `a3b0462b9f8c4a9cb51017ae63884417` exited successfully. All 36 production
business-table fingerprints were unchanged, excluding the newly added bindings.
Older test schemas also received their already-committed pending migrations.

CloudFormation `silicon-dm-production` completed its reviewed update. API and
worker task definitions 17 run this immutable image:

```text
234951665042.dkr.ecr.us-east-1.amazonaws.com/silicon-dm-production@sha256:382f0dcd059285b236c2428ad002bda2fa3faef423d0fee067e73e33feded90e
```

The bootstrap image contains the exact new migration binary:

```text
234951665042.dkr.ecr.us-east-1.amazonaws.com/silicon-dm-production@sha256:bd6883e221ca9ff18237a77f6abca5adeecaeb3deca54303635b879398232de9
```

Validation included 52 unit tests, real PostgreSQL group/ownership/tombstone
coverage, strict Clippy, CI and ARM64 build, healthy ALB targets, and `/live` and
`/ready` returning 204. Maharaj's existing 0.9.4 saved profile authenticated and
returned its three existing conversations. The actual Interface Messages view
loaded the existing history. No customer message was sent as a release probe.

After IAM's canonical database switch, Maharaj's installed 0.9.4 CLI again
authenticated through its retained saved session and returned exactly the same
three conversation IDs. The actual Interface Messages view loaded the complete
existing Maharaj history after a full page reload, with no unreadable-response
or reconnect banner. Public readiness remained 204. No customer message was
sent as a release probe.
