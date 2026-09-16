#!/bin/sh
# Compatibility entry point. Honeycomb owns installation and updates.
set -eu
if ! command -v honeycomb >/dev/null 2>&1; then
  printf '%s\n' 'Install Honeycomb first: https://docs.honeycomb.teamofsilicons.com/' >&2
  exit 1
fi
exec honeycomb install 'tos>dm'
