#!/usr/bin/env bash
# Build the Linux release assets into dist/:
#   ramwarden-linux-<arch>.deb      system-wide (apt), helper gets its capability
#   ramwarden-linux-<arch>.tar.gz   per-user (~/.local/bin), any distro with systemd
# Names follow <name>-<os>-<arch>.<format>, which `ramwarden-launcher release
# manifest` reads to know which machine each file is for.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION="$(grep -m1 '^version' "$ROOT/Cargo.toml" | sed 's/.*"\(.*\)".*/\1/')"
ARCH="$(uname -m)"                      # x86_64 | aarch64, as the manifest spells it
OUT="$ROOT/dist"
mkdir -p "$OUT"

# The .deb (builds daemon, helper, launcher and, with GTK headers, the window).
"$ROOT/scripts/build-deb.sh"
mv -f "$OUT/ramwarden_${VERSION}_$(dpkg --print-architecture).deb" "$OUT/ramwarden-linux-$ARCH.deb"

# The tarball, from the same binaries.
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
mkdir -p "$STAGE/bin" "$STAGE/units"
for b in ramwarden-daemon ramwarden-launcher ramwarden; do
    [ -f "$ROOT/target/release/$b" ] && install -m 0755 "$ROOT/target/release/$b" "$STAGE/bin/$b"
done
install -m 0644 "$ROOT/packaging/deb/ramwarden.service"        "$STAGE/units/"
install -m 0644 "$ROOT/packaging/deb/ramwarden-update.service" "$STAGE/units/"
install -m 0644 "$ROOT/packaging/deb/ramwarden-update.timer"   "$STAGE/units/"
install -m 0644 "$ROOT/packaging/deb/ramwarden.desktop"        "$STAGE/"
install -m 0644 "$ROOT/ramwarden.toml"                         "$STAGE/config.toml"
install -m 0755 "$ROOT/packaging/linux/install.sh"             "$STAGE/install.sh"
TGZ="$OUT/ramwarden-linux-$ARCH.tar.gz"
tar -C "$STAGE" --owner=0 --group=0 -czf "$TGZ" .
echo "built $TGZ"
