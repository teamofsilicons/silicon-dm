#!/bin/sh
# Install the dm command. Silicon Apps installs DM and keeps it updated; this
# script only hands over to it (the docs site serves it at /install.sh).
set -eu
apps=$(command -v silicon-apps 2>/dev/null || true)
if [ -z "$apps" ] && [ -x "${SILICON_HOME:-$HOME}/.apps/bin/silicon-apps" ]; then
  apps="${SILICON_HOME:-$HOME}/.apps/bin/silicon-apps"
fi
if [ -z "$apps" ]; then
  cat >&2 <<'EOF'
dm is installed and updated by Silicon Apps, which is not installed here.
Install it, then run this script again (or `silicon-apps install dm`):

  curl -fsSL https://apps.teamofsilicons.com/install.sh -o install-apps.sh
  bash install-apps.sh --server https://apps.teamofsilicons.com

More: https://developers.teamofsilicons.com/docs/apps/start/install
EOF
  exit 1
fi
exec "$apps" install dm
