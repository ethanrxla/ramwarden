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
  launchctl bootout "$DOMAIN/com.ramwarden.usage" 2>/dev/null || true
  rm -f "$HOME/Library/LaunchAgents/com.ramwarden.usage.plist" "$HOME/.local/bin/ramwarden-usage-sync"
  launchctl bootout "$DOMAIN/com.ramwarden.menubar" 2>/dev/null || true
  rm -f "$HOME/Library/LaunchAgents/com.ramwarden.menubar.plist"
  launchctl bootout "$DOMAIN/$LABEL" 2>/dev/null || true
  rm -f "$PLIST" "$BIN"
  rm -f "$HOME/.local/bin/ramwarden" "$HOME/.local/bin/ramwarden-doctor"
  echo "removed $LABEL (config left in $CONF)"
  exit 0
fi

[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
cargo build --release -p ramwarden-daemon -p ramwarden-cloud -p ramwarden-cli --bin ramwarden-daemon --bin ramwarden-usage-sync --bin ramwarden --manifest-path "$ROOT/Cargo.toml"

mkdir -p "$(dirname "$BIN")" "$(dirname "$CONF")" "$HOME/Library/LaunchAgents" "$HOME/Library/Logs"
install -m 755 "$ROOT/target/release/ramwarden-daemon" "$BIN"
install -m 755 "$ROOT/target/release/ramwarden-usage-sync" "$HOME/.local/bin/ramwarden-usage-sync"
# Never copy the operator environment into a client installation.
# Existing per-user provider credentials remain untouched.
install -m 755 "$ROOT/target/release/ramwarden" "$HOME/.local/bin/ramwarden"
ln -sf ramwarden "$HOME/.local/bin/ramwarden-doctor"
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

# The optional numeric collector stays inert until device sign-in AND account opt-in.
USAGE_PLIST="$HOME/Library/LaunchAgents/com.ramwarden.usage.plist"
sed "s|__HOME__|$HOME|g" "$ROOT/packaging/macos/com.ramwarden.usage.plist" > "$USAGE_PLIST"
launchctl bootout "$DOMAIN/com.ramwarden.usage" 2>/dev/null || true
for _ in $(seq 1 50); do launchctl print "$DOMAIN/com.ramwarden.usage" >/dev/null 2>&1 || break; sleep 0.2; done
launchctl bootstrap "$DOMAIN" "$USAGE_PLIST"

printf "\nContinue with: ramwarden setup\nRead-only checks: ramwarden doctor\n"
