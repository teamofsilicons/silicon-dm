#!/bin/sh
# Install DM and its independent hourly-update daemon; authentication is separate.
set -eu
DM_VERSION="${DM_VERSION:-}"
case "$DM_VERSION" in *[!0-9.]*) printf '%s\n' 'DM_VERSION must be a release version.' >&2; exit 1;; esac
if ! command -v cargo >/dev/null 2>&1; then
  curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal
  . "${CARGO_HOME:-$HOME/.cargo}/env"
fi
if [ -n "$DM_VERSION" ]; then
  cargo install silicon-dm-cli --version "$DM_VERSION" --locked
else
  cargo install silicon-dm-cli --locked
fi
DM_EXEC="${CARGO_HOME:-$HOME/.cargo}/bin/dm"
[ -x "$DM_EXEC" ] || { printf '%s\n' 'Cannot locate installed dm binary.' >&2; exit 1; }
DM_STATE_HOME="${SILICON_HOME:-$HOME}"
case "$(uname -s)" in
  Darwin)
    mkdir -p "$HOME/Library/LaunchAgents" "$DM_STATE_HOME/.silicon-dm"
    DM_PLIST="$HOME/Library/LaunchAgents/com.teamofsilicons.dm.plist"
    # XML escaping through the standard Python runtime avoids path interpolation.
    if command -v python3 >/dev/null 2>&1; then
      python3 - "$DM_PLIST" "$DM_EXEC" "$DM_STATE_HOME" <<'PY'
import plistlib,sys,os
path,executable,home=sys.argv[1:]
with open(path,'wb') as f:
    plistlib.dump({'Label':'com.teamofsilicons.dm','ProgramArguments':[executable,'daemon','run'],'EnvironmentVariables':{'SILICON_HOME':home,'PATH':os.environ.get('PATH','/usr/bin:/bin')},'RunAtLoad':True,'KeepAlive':True,'StandardOutPath':home+'/.silicon-dm/service.log','StandardErrorPath':home+'/.silicon-dm/service.log'},f)
os.chmod(path,0o600)
PY
      "$DM_EXEC" daemon stop >/dev/null 2>&1 || true
      launchctl bootout "gui/$(id -u)" "$DM_PLIST" >/dev/null 2>&1 || true
      launchctl bootstrap "gui/$(id -u)" "$DM_PLIST"
    else
      "$DM_EXEC" daemon start
      printf '%s\n' 'Install Python 3 and rerun to enable startup at login.' >&2
    fi
    ;;
  Linux)
    if command -v systemctl >/dev/null 2>&1 && systemctl --user show-environment >/dev/null 2>&1; then
      mkdir -p "$HOME/.config/systemd/user"
      # Restrict interpolated service paths to safe systemd values.
      case "$DM_EXEC$DM_STATE_HOME" in *[!a-zA-Z0-9_./-]*) printf '%s\n' 'For paths containing spaces, configure the user service using the documentation.' >&2; "$DM_EXEC" daemon start; exit 0;; esac
      cat > "$HOME/.config/systemd/user/silicon-dm.service" <<UNIT
[Unit]
Description=Silicon DM relay and hourly updater
After=network-online.target
[Service]
ExecStart=$DM_EXEC daemon run
Environment=SILICON_HOME=$DM_STATE_HOME
Environment=PATH=${CARGO_HOME:-$HOME/.cargo}/bin:/usr/local/bin:/usr/bin:/bin
Restart=always
RestartSec=5
[Install]
WantedBy=default.target
UNIT
      "$DM_EXEC" daemon stop >/dev/null 2>&1 || true
      systemctl --user daemon-reload
      systemctl --user enable --now silicon-dm.service
    else
      "$DM_EXEC" daemon start
      printf '%s\n' 'DM is running. Configure a process supervisor to restart it after reboot.' >&2
    fi
    ;;
  *) "$DM_EXEC" daemon start ;;
esac
printf '\n%s\n' 'DM installed. Next: dm iam --json; dm login <IAM-SLT>; dm webhook <callback-url>'
