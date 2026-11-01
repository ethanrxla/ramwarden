#!/bin/sh
# Install RamWarden for the current user from an unpacked release tarball.
# No sudo. This is what the launcher runs for updates, what the .pkg runs on
# first install (as the logged-in user), and what scripts/install-macos.sh runs
# after building from source.
#
#   sh install.sh             install or update
#   sh install.sh uninstall   remove agents, binaries and the app (keeps config)
set -eu

HERE="$(cd "$(dirname "$0")" && pwd)"
BIN="$HOME/.local/bin"
CONF="$HOME/.config/ramwarden/config.toml"
AGENTS="$HOME/Library/LaunchAgents"
DOMAIN="gui/$(id -u)"
# The menu-bar agent keeps RamWarden.app running and restarts it if it crashes.
LABELS="com.ramwarden.daemon com.ramwarden.updater com.ramwarden.menubar"

stop_agents() {
    for l in $LABELS; do launchctl bootout "$DOMAIN/$l" 2>/dev/null || true; done
    # bootout is asynchronous: bootstrapping before it finishes fails with EIO.
    for l in $LABELS; do
        for _ in $(seq 1 50); do launchctl print "$DOMAIN/$l" >/dev/null 2>&1 || break; sleep 0.2; done
    done
}

if [ "${1:-}" = "uninstall" ]; then
    stop_agents
    for l in $LABELS; do rm -f "$AGENTS/$l.plist"; done
    rm -f "$BIN/ramwarden-daemon" "$BIN/ramwarden-launcher"
    rm -rf "$HOME/Applications/RamWarden.app"
    echo "RamWarden removed (config left in $(dirname "$CONF"))"
    exit 0
fi

mkdir -p "$BIN" "$(dirname "$CONF")" "$AGENTS" "$HOME/Library/Logs" "$HOME/Applications"
stop_agents
# Rename over the old binaries rather than writing into them: a process still
# running the old inode keeps working, and the code signature stays valid.
for b in ramwarden-daemon ramwarden-launcher; do
    if [ -f "$HERE/bin/$b" ]; then
        install -m 0755 "$HERE/bin/$b" "$BIN/.$b.new"
        mv -f "$BIN/.$b.new" "$BIN/$b"
    fi
done
if [ -d "$HERE/RamWarden.app" ]; then
    pkill -x RamWarden 2>/dev/null || true
    rm -rf "$HOME/Applications/RamWarden.app"
    # ditto keeps the bundle's permissions, metadata and signature intact.
    ditto "$HERE/RamWarden.app" "$HOME/Applications/RamWarden.app"
fi
[ -f "$CONF" ] || install -m 0644 "$HERE/config.toml" "$CONF"
for l in $LABELS; do
    [ -f "$HERE/$l.plist" ] && sed "s|__HOME__|$HOME|g" "$HERE/$l.plist" > "$AGENTS/$l.plist"
done
for l in $LABELS; do
    [ -f "$AGENTS/$l.plist" ] && launchctl bootstrap "$DOMAIN" "$AGENTS/$l.plist"
done
# A personal install line leaves a link token; claim it now so this Mac is
# signed in before the user opens anything. Harmless when there is none.
"$BIN/ramwarden-launcher" link || true
echo "RamWarden installed. API: http://127.0.0.1:7823   log: ~/Library/Logs/ramwarden.log"
