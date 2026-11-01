#!/usr/bin/env bash
# Build RamWarden and install it as a per-user launchd agent. No sudo needed.
#   scripts/install-macos.sh            install / reinstall
#   scripts/install-macos.sh uninstall  remove the agent and binary (keeps config)
set -euo pipefail

LABEL=com.ramwarden.daemon
BIN="$HOME/.local/bin/ramwarden-daemon"
CONF="$HOME/.config/ramwarden/config.toml"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
DOMAIN="gui/$(id -u)"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"

if [ "${1:-}" = "uninstall" ]; then
  launchctl bootout "$DOMAIN/com.ramwarden.menubar" 2>/dev/null || true
  rm -f "$HOME/Library/LaunchAgents/com.ramwarden.menubar.plist"
  launchctl bootout "$DOMAIN/$LABEL" 2>/dev/null || true
  rm -f "$PLIST" "$BIN"
  echo "removed $LABEL (config left in $CONF)"
  exit 0
fi

[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
cargo build --release -p ramwarden-daemon --manifest-path "$ROOT/Cargo.toml"

mkdir -p "$(dirname "$BIN")" "$(dirname "$CONF")" "$HOME/Library/LaunchAgents" "$HOME/Library/Logs"
install -m 755 "$ROOT/target/release/ramwarden-daemon" "$BIN"
# Provider keys live in the repo-local, git-ignored .env; the launchd daemon
# reads the per-user copy (mode 600), since it never sources a shell env.
if [ -f "$ROOT/.env" ]; then install -m 600 "$ROOT/.env" "$(dirname "$CONF")/.env"; fi
[ -f "$CONF" ] || install -m 644 "$ROOT/packaging/macos/config.toml" "$CONF"
sed "s|__HOME__|$HOME|g" "$ROOT/packaging/macos/com.ramwarden.daemon.plist" > "$PLIST"

launchctl bootout "$DOMAIN/$LABEL" 2>/dev/null || true
# bootout is asynchronous: bootstrapping before it finishes fails with EIO.
for _ in $(seq 1 50); do launchctl print "$DOMAIN/$LABEL" >/dev/null 2>&1 || break; sleep 0.2; done
launchctl bootstrap "$DOMAIN" "$PLIST"
# Preserve the existing status item and install the optional companion alongside it.
bash "$ROOT/macos/menubar/build.sh"
MENU_PLIST="$HOME/Library/LaunchAgents/com.ramwarden.menubar.plist"
python3 - "$ROOT/packaging/macos/com.ramwarden.menubar.plist" "$MENU_PLIST" <<'PYPLIST'
import os, plistlib, sys
with open(sys.argv[1], 'rb') as f: p = plistlib.load(f)
p['ProgramArguments'] = [os.path.expanduser('~/Applications/RamWarden.app/Contents/MacOS/RamWarden')]
for k in ('StandardOutPath', 'StandardErrorPath'): p[k] = p[k].replace('__HOME__', os.path.expanduser('~'))
with open(sys.argv[2], 'wb') as f: plistlib.dump(p, f)
PYPLIST
launchctl bootout "$DOMAIN/com.ramwarden.menubar" 2>/dev/null || true
pkill -x RamWarden 2>/dev/null || true
for _ in $(seq 1 50); do launchctl print "$DOMAIN/com.ramwarden.menubar" >/dev/null 2>&1 || break; sleep 0.2; done
launchctl bootstrap "$DOMAIN" "$MENU_PLIST"
echo "installed. status:  launchctl list | grep ramwarden"
echo "logs:              tail -f ~/Library/Logs/ramwarden.log"
echo "api:               curl -s localhost:7823/actions | jq"
