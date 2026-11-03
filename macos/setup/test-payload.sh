#!/usr/bin/env bash
set -euo pipefail
RES="${1:?mounted installer Resources directory}"
STAGE="$(mktemp -d /tmp/warden-payload-test.XXXXXX)"
trap 'rm -rf "$STAGE"' EXIT
mkdir -p "$STAGE/.config/ramwarden" "$STAGE/.local/bin"
printf '# preserve my config\n' > "$STAGE/.config/ramwarden/config.toml"
printf 'private test fixture\n' > "$STAGE/.config/ramwarden/secrets.toml"
printf 'history fixture\n' > "$STAGE/.config/ramwarden/history.db"
printf 'previous binary\n' > "$STAGE/.local/bin/ramwarden-daemon"
bash "$RES/install-payload.sh" --test-root "$STAGE"
test -x "$STAGE/.local/bin/ramwarden-daemon"
test -x "$STAGE/.local/bin/ramwarden-usage-sync"
test -d "$STAGE/Applications/RamWarden.app"
test -f "$STAGE/Library/Application Support/RamWarden/extension/manifest.json"
test "$(cat "$STAGE/.config/ramwarden/config.toml")" = '# preserve my config'
test "$(cat "$STAGE/.config/ramwarden/secrets.toml")" = 'private test fixture'
test "$(cat "$STAGE/.config/ramwarden/history.db")" = 'history fixture'
find "$STAGE/Library/Application Support/RamWarden/Backups" -name ramwarden-daemon -type f | while IFS= read -r FILE; do test "$(cat "$FILE")" = 'previous binary'; done
echo 'Payload installation preserves existing configuration, secrets and history; stages both binaries, app and extension.'
