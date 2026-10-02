#!/usr/bin/env bash
#
# Build a .deb containing the three RamWarden binaries.
#
# v1 shipped a Python tree plus a pip install into /usr/lib/ramwarden, which meant
# the package depended on the host's Python and on PyPI being reachable at install
# time. This ships three static-ish binaries and nothing else.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION="$(grep -m1 '^version' "$ROOT/Cargo.toml" | sed 's/.*"\(.*\)".*/\1/')"
ARCH="$(dpkg --print-architecture)"
STAGE="$(mktemp -d)"
OUT="$ROOT/dist"
trap 'rm -rf "$STAGE"' EXIT

echo "building ramwarden $VERSION for $ARCH"

# The GUI needs GTK4 headers; the daemon and helper do not. Build them separately
# so a headless machine can still produce a usable package.
cargo build --release --manifest-path "$ROOT/Cargo.toml" \
    -p ramwarden-daemon -p ramwarden-helper
if pkg-config --exists gtk4 2>/dev/null; then
    cargo build --release --manifest-path "$ROOT/Cargo.toml" \
        -p ramwarden-ui --features gui --bin ramwarden
    WITH_GUI=1
else
    echo "WARNING: libgtk-4-dev not found — packaging without the window." >&2
    WITH_GUI=0
fi

mkdir -p "$STAGE"/{DEBIAN,usr/bin,usr/share/applications,usr/lib/systemd/user,etc/ramwarden}
mkdir -p "$STAGE"/usr/share/icons/hicolor/{16x16,48x48,128x128}/apps
mkdir -p "$STAGE"/usr/share/doc/ramwarden

install -m 0755 "$ROOT/target/release/ramwarden-daemon" "$STAGE/usr/bin/ramwarden-daemon"
install -m 0755 "$ROOT/target/release/ramwarden-helper" "$STAGE/usr/bin/ramwarden-helper"
install -m 0644 "$ROOT/packaging/deb/ramwarden.service"        "$STAGE/usr/lib/systemd/user/ramwarden.service"
install -m 0644 "$ROOT/packaging/deb/ramwarden-helper.service" "$STAGE/usr/lib/systemd/user/ramwarden-helper.service"
install -m 0644 "$ROOT/ramwarden.toml" "$STAGE/etc/ramwarden/config.toml"
install -m 0644 "$ROOT/README.md"      "$STAGE/usr/share/doc/ramwarden/README.md"

if [ "$WITH_GUI" = 1 ]; then
    install -m 0755 "$ROOT/target/release/ramwarden" "$STAGE/usr/bin/ramwarden"
    install -m 0644 "$ROOT/packaging/deb/ramwarden.desktop" \
        "$STAGE/usr/share/applications/ramwarden.desktop"
fi

for size in 16 48 128; do
    src="$ROOT/extension/icons/icon${size}.png"
    [ -f "$src" ] && install -m 0644 "$src" \
        "$STAGE/usr/share/icons/hicolor/${size}x${size}/apps/ramwarden.png"
done

install -m 0755 "$ROOT/packaging/deb/postinst" "$STAGE/DEBIAN/postinst"
install -m 0755 "$ROOT/packaging/deb/prerm"    "$STAGE/DEBIAN/prerm"

# Only the runtime libraries the binaries actually link, plus libcap2-bin so the
# postinst can grant the helper its capability.
DEPENDS="libc6, libsqlite3-0 | libc6, libcap2-bin"
if [ "$WITH_GUI" = 1 ]; then
    DEPENDS="$DEPENDS, libgtk-4-1 (>= 4.10)"
fi

cat > "$STAGE/DEBIAN/control" <<CONTROL
Package: ramwarden
Version: $VERSION
Section: utils
Priority: optional
Architecture: $ARCH
Depends: $DEPENDS
Recommends: wmctrl, pulseaudio-utils
Maintainer: Ethan Risden <ethanrisden97@gmail.com>
Description: Memory manager for Linux desktops
 RamWarden watches memory pressure, works out what you are actually using, and
 reclaims the rest. It asks the kernel to page an application's cold memory to
 zram before it will consider freezing or closing anything, so most pressure is
 handled without the user noticing.
 .
 Accounting is proportional set size and cgroup charges rather than summed RSS,
 which on a browser overstates usage by more than twofold.
 .
 Decisions can be refined by a local Nemotron model via Ollama. Nothing leaves
 the machine unless the optional NVIDIA tier is explicitly enabled.
CONTROL

mkdir -p "$OUT"
DEB="$OUT/ramwarden_${VERSION}_${ARCH}.deb"
dpkg-deb --build --root-owner-group "$STAGE" "$DEB" >/dev/null
echo
echo "built $DEB ($(du -h "$DEB" | cut -f1))"
dpkg-deb --contents "$DEB" | awk '{print "  "$6, $7, $8}'
cat <<'NEXT'

Install and enable:
    sudo dpkg -i dist/ramwarden_*.deb
    systemctl --user daemon-reload
    systemctl --user enable --now ramwarden
    systemctl --user enable --now ramwarden-helper   # optional, for page-out

Then launch the window from your application menu, or run `ramwarden`.
NEXT
