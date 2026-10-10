# DM 0.6 — groups

DM 0.6 implements organization groups across storage, authorization, HTTP,
WebSocket delivery, the Rust SDK, local runtime, CLI, browser and diagnostics.

- Named groups have descriptions, explicit invitations and IAM tag policies.
- Public groups include organization Carbons; Silicons require invitations.
- IAM-disclosed administrators and owners manage groups and invitations.
- New members can read full prior history. Removing access stops future reads,
  writes, private-draft access and delivery replay.
- Existing direct conversations retain their sealed participant sets.
- HTTP v1, WebSocket v3 and shared transport v1 remain unchanged. Group metadata
  and group endpoints are additive.

Use [the groups guide](groups.md) for commands, SDK methods and access rules.
The migration is `0020_groups.sql`; rerun `deploy/runtime-grants.sql` before
rolling out API and worker binaries. Deploy the gateway route allowlist together
with the browser release. CLI and Rust packages use version 0.6.0.
