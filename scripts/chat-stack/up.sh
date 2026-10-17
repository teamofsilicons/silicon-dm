#!/usr/bin/env bash
# Start the local chat stack (idempotent): Postgres on 5500, Silicon Accounts on 9840/9839, dm-api on 4521, the
# Deepgram and Briefcase stand-ins on 4540/4541 and the identity pool; prints the URLs. Options: --help.
#   scripts/chat-stack/up.sh                 start whatever is not running
#   scripts/chat-stack/up.sh --restart-dm    rebuild DM, re-run its migrations and restart dm-api (new routes)
#   DM_DEEPGRAM_REAL=1 scripts/chat-stack/up.sh   dm-api speaks through the real Deepgram (key never printed)
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if [ "${1:-}" = "-h" ] || [ "${1:-}" = "--help" ]; then exec node "$here/stack.mjs" help; fi
exec node "$here/stack.mjs" up "$@"
