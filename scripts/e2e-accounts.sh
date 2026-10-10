#!/usr/bin/env bash
# End-to-end scenarios against a local Silicon Accounts stack, with real tokens and the real binaries:
#
#   DM_TEST_STACK=/path/to/test-stack.json SILICON_ACCOUNTS_DIR=/path/to/silicon-accounts \
#     scripts/e2e-accounts.sh [--only 1,2,...] [--keep-running] [--run N]
#
# Starts scripts/dev-accounts.sh when dm-api is not running (and stops it afterwards). Scenarios and requirements:
# tests/e2e/README.md. Results: .mig/e2e/run-<N>/result.json and transcript.txt (secrets redacted).
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec "${PYTHON:-python3}" -I "$here/../tests/e2e/accounts_e2e.py" "$@"
