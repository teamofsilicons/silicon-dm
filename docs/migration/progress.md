# DM migration to Silicon Accounts + Silicon Apps: progress log

Branch `migrate/accounts-apps-20261010` (based on `origin/main` e175aa4, DM 0.14.1). Each stage appends a dated
section: what it did, commits, test commands and results, what is left, gotchas. Decisions live in
[decisions.md](decisions.md).

## 2026-10-10 — Stage 1 (service): baseline before any change

Environment: Rust 1.98.0 (rust-toolchain.toml), `CARGO_TARGET_DIR=target/mig`, Postgres 16.11 on 127.0.0.1:5460 (trust),
no Docker (the daemon hangs). 96 GB free on /System/Volumes/Data.

| command | result |
|---|---|
| `cargo test --locked --workspace --all-features --no-run` | builds (vendored `silicon-iam-client` is a workspace member through its path dependency) |
| `cargo test --locked --workspace --all-features --lib --bins` | pass: silicon_dm 52, dm (CLI main) 4, silicon_dm_client 46, silicon_dm_protocol 3, vendored silicon_iam_client 47 |
| `cargo test --locked -p silicon-dm-cli -p silicon-dm-client -p silicon-dm-protocol --all-features --all-targets` | pass: CLI 4 + delivery 3 + discovery 1 + em_dash 1 + home 2 + message_length 1; client 46 + ting_http 4; protocol 3 |
| `cargo test --locked -p silicon-dm --all-features --test wire_contract --test ting_transport` | pass: 7 + 7 |
| DB suites, one fresh database per test function (`DM_TEST_DATABASE_URL=postgres://postgres@127.0.0.1:5460/dm_t_…`, runner script in the stage scratchpad) | 13/13 pass: http_delivery_routes (1), http_sync_presence (1), postgres_durable_flow (4), ting_authorization (1), ting_credentials (1), ting_handoffs (1), ting_publisher (1), ting_worker (1), understanding_contract::{bundle_commands_and_history_are_atomic_and_retry_safe, managed_generation_is_independent_of_credential_and_lifecycle_revisions} |
| Docker-only (call testcontainers directly, ignore `DM_TEST_DATABASE_URL`) | 10 fail without Docker after testcontainers' 120 s start timeout (pre-existing, environmental): group_id_migration::upgrade_keeps_existing_groups_and_assigns_unique_stable_addresses, identifier_schema_migration::cutover_preserves_conversation_and_exact_replay_receipt, message_history_migration::upgrade_preserves_edits_and_keeps_deletion_out_of_history, ting_cutover_hold::held_handoffs_preserve_exact_rows_and_fail_closed, understanding_contract::{app_secret_discovery_is_isolated_revalidated_and_resets_generation, groups_work_through_sdk_and_http_with_admin_and_retry_guards, honeycomb_lifecycle_is_authenticated_fenced_and_retry_safe, recipient_addressed_messages_use_stable_local_codes_and_server_owned_replies, reports_retry_postmark_and_deduplicate_without_sending_test_email, retired_sockets_leave_http_actor_scope_and_contract_lifecycle_intact} |

So every regression after this point is a test that passed above and fails later.
