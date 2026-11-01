#!/usr/bin/env bash
# Build RamWarden.app (menu-bar only, no Dock icon) into ~/Applications, or
# into the directory given as $1 (scripts/package-macos.sh stages it there).
set -euo pipefail
cd "$(dirname "$0")"
VERSION="$(grep -m1 '^version' ../../Cargo.toml | sed 's/.*"\(.*\)".*/\1/')"
APP="${1:-$HOME/Applications}/RamWarden.app"
rm -rf "$APP"; mkdir -p "$APP/Contents/MacOS"
swiftc -O -o "$APP/Contents/MacOS/RamWarden" main.swift
cat > "$APP/Contents/Info.plist" <<PL
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>RamWarden</string>
  <key>CFBundleIdentifier</key><string>com.ramwarden.menubar</string>
  <key>CFBundleExecutable</key><string>RamWarden</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>LSUIElement</key><true/>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
</dict></plist>
PL
# Ad hoc unless a Developer ID is given (release builds; see package-macos.sh),
# which notarization needs with the hardened runtime and a secure timestamp.
if [ -n "${MACOS_SIGN_IDENTITY:-}" ]; then
  codesign --force --options runtime --timestamp --sign "$MACOS_SIGN_IDENTITY" "$APP" >/dev/null
else
  codesign --force --sign - "$APP" >/dev/null
fi
echo "built $APP"
