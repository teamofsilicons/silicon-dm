# DM client and CLI 0.4.1

DM's local relay can now deliver webhooks directly to Silicon 3.5 without an adapter. Callbacks include the required root `metadata` object, while caller-owned message metadata remains in `data.metadata`. The relay accepts Silicon's HTTP 2xx `status: ok` response with a non-nil event UUID, in addition to the existing enveloped DM acknowledgement.

Silicon's reserved `*.localhost` callback hosts are accepted and pinned to loopback with their Host header preserved. These local requests bypass DNS and proxies. Backend endpoint validation and REST/WebSocket envelopes are unchanged.

The CLI supports `SILICON_DM_TEST` as the default for `--test`. A dedicated runtime can select its DM environment once in service configuration and use ordinary `dm` commands without a shell wrapper. An explicit `--test` overrides it; unset the variable for production lifecycle management.

Configure native Silicon `login` and `webhook` entries with `dm`. Its flow should ignore receipt events, sender copies, and deleted messages, and include conversation/message IDs when prompting the assistant to reply. Silicon's acknowledgement confirms event-flow acceptance, not completion of inference. Lost acknowledgements can replay model work; use stable delivery-derived idempotency keys for replies.

## Release scope

Publish `silicon-dm-client` 0.4.1, then `silicon-dm-cli` 0.4.1. The protocol crate and backend remain at 0.4.0. No database migration or backend rollout is required.

## Validation

Regression tests cover the native callback payload, both acknowledgement formats, retry and delivered-receipt behavior, rejection of malformed acknowledgements, reserved localhost routing, callback URL validation, and CLI environment selection/flag precedence.
