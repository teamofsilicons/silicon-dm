#!/usr/bin/env bash
# Start DM on this machine against a local Silicon Accounts stack (idempotent):
#
#   DM_TEST_STACK=/path/to/test-stack.json scripts/dev-accounts.sh [--build] [--fresh] [--ting]
#
# Migrates the database (default dm_e2e on 127.0.0.1:5460), starts dm-api on 127.0.0.1:4121 as a role with
# only the runtime grants, points DM's webhook at Silicon Accounts to it and proves a test delivery. --ting adds
# a local Ting stand-in on 127.0.0.1:4122. Stop with scripts/dev-accounts-stop.sh.
# Configuration: python3 scripts/dev_accounts.py --help.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec "${PYTHON:-python3}" -I "$here/dev_accounts.py" up "$@"
