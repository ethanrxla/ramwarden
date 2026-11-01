#!/usr/bin/env bash
# Build the macOS release assets into dist/:
#   ramwarden-macos-<arch>.tar.gz   what the launcher downloads to update
#   RamWarden.pkg                       first install, double-clickable
#
# Signing and notarization happen only when their variables are set, so the
# script also works on a laptop with no Apple account:
#   MACOS_SIGN_IDENTITY       "Developer ID Application: Name (TEAMID)"
#   MACOS_INSTALLER_IDENTITY  "Developer ID Installer: Name (TEAMID)"
#   APPLE_ID, APPLE_TEAM_ID, APPLE_APP_PASSWORD   notarytool credentials
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION="$(grep -m1 '^version' "$ROOT/Cargo.toml" | sed 's/.*"\(.*\)".*/\1/')"
ARCH="$(uname -m)"; [ "$ARCH" = arm64 ] && ARCH=aarch64
OUT="$ROOT/dist"
STAGE="$(mktemp -d)"; PKGSCRIPTS="$(mktemp -d)"
trap 'rm -rf "$STAGE" "$PKGSCRIPTS"' EXIT
mkdir -p "$OUT" "$STAGE/bin"

cargo build --release --manifest-path "$ROOT/Cargo.toml" -p ramwarden-daemon -p ramwarden-launcher
for b in ramwarden-daemon ramwarden-launcher; do
    install -m 0755 "$ROOT/target/release/$b" "$STAGE/bin/$b"
    if [ -n "${MACOS_SIGN_IDENTITY:-}" ]; then
        codesign --force --options runtime --timestamp --sign "$MACOS_SIGN_IDENTITY" "$STAGE/bin/$b"
    fi
done
"$ROOT/macos/menubar/build.sh" "$STAGE"
install -m 0755 "$ROOT/packaging/macos/install.sh"                   "$STAGE/install.sh"
install -m 0644 "$ROOT/packaging/macos/config.toml"                  "$STAGE/config.toml"
install -m 0644 "$ROOT/packaging/macos/com.ramwarden.daemon.plist"   "$STAGE/"
install -m 0644 "$ROOT/packaging/macos/com.ramwarden.updater.plist"  "$STAGE/"

TGZ="$OUT/ramwarden-macos-$ARCH.tar.gz"
COPYFILE_DISABLE=1 tar -C "$STAGE" -czf "$TGZ" .
echo "built $TGZ"

# The .pkg installs nothing itself: its postinstall unpacks the same tarball
# as the logged-in user, so first install and update share one install.sh.
install -m 0755 "$ROOT/packaging/macos/pkg/postinstall" "$PKGSCRIPTS/postinstall"
cp "$TGZ" "$PKGSCRIPTS/payload.tar.gz"
PKG="$OUT/RamWarden.pkg"
UNSIGNED="$(mktemp -d)/RamWarden.pkg"
pkgbuild --nopayload --scripts "$PKGSCRIPTS" --identifier com.ramwarden.installer \
    --version "$VERSION" "$UNSIGNED"
if [ -n "${MACOS_INSTALLER_IDENTITY:-}" ]; then
    productsign --sign "$MACOS_INSTALLER_IDENTITY" "$UNSIGNED" "$PKG"
else
    cp "$UNSIGNED" "$PKG"
    echo "note: $PKG is unsigned; Gatekeeper will refuse it until it is signed and notarized" >&2
fi

if [ -n "${APPLE_ID:-}" ] && [ -n "${APPLE_TEAM_ID:-}" ] && [ -n "${APPLE_APP_PASSWORD:-}" ]; then
    xcrun notarytool submit "$PKG" --apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" \
        --password "$APPLE_APP_PASSWORD" --wait
    xcrun stapler staple "$PKG"
fi
echo "built $PKG"
