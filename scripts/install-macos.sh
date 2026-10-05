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
  launchctl bootout "$DOMAIN/$LABEL" 2>/dev/null || true
  rm -f "$PLIST" "$BIN"
  echo "removed $LABEL (config left in $CONF)"
  exit 0
fi

[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
cargo build --release -p ramwarden-daemon --manifest-path "$ROOT/Cargo.toml"

mkdir -p "$(dirname "$BIN")" "$(dirname "$CONF")" "$HOME/Library/LaunchAgents" "$HOME/Library/Logs"
install -m 755 "$ROOT/target/release/ramwarden-daemon" "$BIN"
[ -f "$CONF" ] || install -m 644 "$ROOT/packaging/macos/config.toml" "$CONF"
sed "s|__HOME__|$HOME|g" "$ROOT/packaging/macos/com.ramwarden.daemon.plist" > "$PLIST"

launchctl bootout "$DOMAIN/$LABEL" 2>/dev/null || true
launchctl bootstrap "$DOMAIN" "$PLIST"
echo "installed. status:  launchctl list | grep ramwarden"
echo "logs:              tail -f ~/Library/Logs/ramwarden.log"
echo "api:               curl -s localhost:7823/actions | jq"
