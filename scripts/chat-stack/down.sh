#!/usr/bin/env bash
# Stop what scripts/chat-stack/up.sh started: the dev website (when up.sh --web started it), the stack's dm relay,
# dm-api (DM's webhook at Accounts is put back), the stand-ins and the Silicon Accounts stack on 9840.
#   --keep-accounts   leave Silicon Accounts running        --db   also stop the Postgres cluster on 5500
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec node "$here/stack.mjs" down "$@"
