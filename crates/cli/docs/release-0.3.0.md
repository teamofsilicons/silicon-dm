# DM 0.3.0

- Optional ISI sender and recipient addresses persist through message history, delivery, edits, and bundles. Authorization remains tied to canonical IAM accounts.
- Login accepts an SLT without a webhook; configure callbacks afterward with `dm webhook URL`. `dm unhook` retains authentication and queued events.
- `dm iam --json` returns public application discovery; `dm login status --json` verifies the saved session and reports its actor.
- `SILICON_HOME` selects the default home directory.

## Rust migration

`Client::login` now accepts `(slt, idempotency_key)`. Remove the old webhook argument. `LoginOptions.webhook_url` is `Option<&Url>` and saved `Profile.webhook_url` is `Option<String>`. Use `LocalRuntime::webhook` after login to configure or remove callbacks. Explicit `MessageCreate` struct literals must include `recipient_id` or use `..Default::default()`.

## Backend deployment

Apply migration `0015_message_isi_routing.sql` through `dm-migrate` before rolling out API and worker images. It adds nullable routing fields without changing existing account identities or message content. Existing test schemas upgrade through the normal DM environment lifecycle.

## Production verification — 2026-09-09

Production rollout completed at 09:49 UTC from source commit
`782a28a52b6a15250a9748f4c346b8479bc75c2b` (tag `v0.3.0`).

- Both `silicon-dm-client` and `silicon-dm-cli` 0.3.0 are published on crates.io.
- [Release CI](https://github.com/teamofsilicons/silicon-dm/actions/runs/34334697475) passed Rust quality, dependency policy, and container smoke checks. All 53 local workspace tests also passed.
- ARM64 runtime image: `sha256:b5481e61d16fb1d5528fcce4a7211af6eac8e4d09b7cb0a9c16a3fab2d2b1b33`.
- ARM64 bootstrap image: `sha256:df05e77cb0b81c750197dc4d11bfffa575d1a71809ff5817deef7a8f7dd356a9`.
- Pre-migration RDS snapshot `silicon-dm-pre-0-3-0-20260909094006` reached `available` before migration started.
- Bootstrap task `4470fd00d19b4b858fce7340d96894c9` (definition `silicon-dm-bootstrap:4`) exited 0. Logs confirmed credential continuity, production migrations, runtime grants, and testing-role configuration.
- Migration 0015 was applied before rolling the runtime. Existing test schemas continue to migrate through the normal environment lifecycle.
- CloudFormation stack `silicon-dm-production` in `us-east-1` reached `UPDATE_COMPLETE`.
- API and worker task definitions are revision 5, each with one running task, zero pending tasks, and a single `COMPLETED` deployment. Both running image digests match the runtime digest above.
- The ALB target is healthy. Public `/live` and `/ready` returned 204; `/api/v1/iam` returned 200 with `app_id: "tos>dm"`. The 0.3.0 CLI's `iam --json` command confirmed the same production response.

The rollout used the deployed CloudFormation template and preserved existing
parameters except the two image references. No frontend or browser gateway
change was required. Authenticated live messaging was not exercised during
this rollout; ISI persistence, authorization boundaries, replay, edits and
bundling were verified in the local PostgreSQL integration test.
