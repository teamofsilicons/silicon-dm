# DM 0.3.0

- Optional ISI sender and recipient addresses persist through message history, delivery, edits, and bundles. Authorization remains tied to canonical IAM accounts.
- Login accepts an SLT without a webhook; configure callbacks afterward with `dm webhook URL`. `dm unhook` retains authentication and queued events.
- `dm iam --json` returns public application discovery; `dm login status --json` verifies the saved session and reports its actor.
- `SILICON_HOME` selects the default home directory.

## Rust migration

`Client::login` now accepts `(slt, idempotency_key)`. Remove the old webhook argument. `LoginOptions.webhook_url` is `Option<&Url>` and saved `Profile.webhook_url` is `Option<String>`. Use `LocalRuntime::webhook` after login to configure or remove callbacks. Explicit `MessageCreate` struct literals must include `recipient_id` or use `..Default::default()`.

## Backend deployment

Apply migration `0015_message_isi_routing.sql` through `dm-migrate` before rolling out API and worker images. It adds nullable routing fields without changing existing account identities or message content. Existing test schemas upgrade through the normal DM environment lifecycle.
