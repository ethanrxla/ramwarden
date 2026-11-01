#!/usr/bin/env bash
# Build RamWarden from source and install it as per-user launchd agents. No sudo.
#   scripts/install-macos.sh            install / reinstall
#   scripts/install-macos.sh uninstall  remove agents, binaries and app (keeps config)
#
# Release installs use the same packaging/macos/install.sh, from the tarball
# that scripts/package-macos.sh builds.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
if [ "${1:-}" = "uninstall" ]; then
    exec /bin/sh "$ROOT/packaging/macos/install.sh" uninstall
fi

[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
cargo build --release -p ramwarden-daemon -p ramwarden-launcher --manifest-path "$ROOT/Cargo.toml"

STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
mkdir -p "$STAGE/bin"
install -m 0755 "$ROOT/target/release/ramwarden-daemon"   "$STAGE/bin/"
install -m 0755 "$ROOT/target/release/ramwarden-launcher" "$STAGE/bin/"
cp "$ROOT/packaging/macos/"{install.sh,config.toml,com.ramwarden.daemon.plist,com.ramwarden.updater.plist} "$STAGE/"

# Provider keys live in the repo-local, git-ignored .env; the launchd daemon
# reads the per-user copy (mode 600), since it never sources a shell env.
# Source installs only: release tarballs never carry keys.
CONF_DIR="$HOME/.config/ramwarden"
mkdir -p "$CONF_DIR"
if [ -f "$ROOT/.env" ]; then install -m 600 "$ROOT/.env" "$CONF_DIR/.env"; fi

/bin/sh "$STAGE/install.sh"
echo "status:  launchctl list | grep ramwarden"
echo "logs:    tail -f ~/Library/Logs/ramwarden.log"
echo "api:     curl -s localhost:7823/actions | jq"
