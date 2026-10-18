#!/usr/bin/env bash
# The chat stack's health as JSON (exit 0 when every required part answers, 1 otherwise).
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec node "$here/stack.mjs" status "$@"
