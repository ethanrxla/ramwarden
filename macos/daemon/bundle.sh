#!/usr/bin/env bash
# Wrap the ramwarden-daemon binary in its own app bundle.
#   macos/daemon/bundle.sh <ramwarden-daemon binary> <destination .app> [codesign identity]
#
# Why a bundle: macOS lists a bare executable in Privacy & Security (Full Disk
# Access) by its file name with the generic black "exec" icon. A bundle's main
# executable is listed by the bundle's name and icon instead, so the row reads
# "RamWarden" with the crimson ram. TCC then also records the grant against the
# bundle identifier and signing identity, not a path, so a Developer ID signed
# update keeps the grant; an ad-hoc signed rebuild is still a new program.
#
# The bundle is background-only: no Dock icon, no menu bar, nothing to click.
# Identity "-" (the default) signs ad hoc; release packaging passes its own.
set -euo pipefail
BIN="${1:?ramwarden-daemon binary required}"
DEST="${2:?destination .app required}"
IDENTITY="${3:--}"
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
case "$DEST" in *.app) ;; *) echo 'Destination must end in .app'; exit 2 ;; esac
test -x "$BIN"
VERSION="$(sed -n 's/^version *= *"\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -n 1)"
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
APP="$STAGE/RamWarden.app"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
install -m 755 "$BIN" "$APP/Contents/MacOS/ramwarden-daemon"
cp "$ROOT/assets/brand/RamWardenDaemon.icns" "$APP/Contents/Resources/RamWarden.icns"
cat > "$APP/Contents/Info.plist" <<PL
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleIdentifier</key><string>com.ramwarden.daemon</string>
  <key>CFBundleName</key><string>RamWarden</string>
  <key>CFBundleDisplayName</key><string>RamWarden</string>
  <key>CFBundleExecutable</key><string>ramwarden-daemon</string>
  <key>CFBundleIconFile</key><string>RamWarden.icns</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>LSBackgroundOnly</key><true/>
  <key>LSUIElement</key><true/>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
</dict></plist>
PL
SIGN=(--force --sign "$IDENTITY")
if [ "$IDENTITY" != '-' ]; then SIGN+=(--options runtime --timestamp); fi
if command -v codesign >/dev/null 2>&1; then
  codesign "${SIGN[@]}" "$APP"
  codesign --verify --strict "$APP"
fi
mkdir -p "$(dirname "$DEST")"
rm -rf "$DEST"
# ditto keeps the signature's extended attributes; cp -R is for tests off macOS.
if command -v ditto >/dev/null 2>&1; then ditto "$APP" "$DEST"; else cp -R "$APP" "$DEST"; fi
echo "bundled $DEST"
