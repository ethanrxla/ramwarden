#!/usr/bin/env bash
# Build before replacing the existing app. No credentials enter the app bundle.
set -euo pipefail
cd "$(dirname "$0")"
APP="${RAMWARDEN_APP_DEST:-$HOME/Applications/RamWarden.app}"
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
mkdir -p "$STAGE/RamWarden.app/Contents/MacOS" "$STAGE/RamWarden.app/Contents/Resources"
cp ../../LICENSE "$STAGE/RamWarden.app/Contents/Resources/LICENSE.txt"
cp ../../assets/audio/*.wav "$STAGE/RamWarden.app/Contents/Resources/"
cp ../../assets/brand/RamWarden.icns "$STAGE/RamWarden.app/Contents/Resources/"
cp ../../web/assets/pets/threat.svg "$STAGE/RamWarden.app/Contents/Resources/ThreatWarden.svg"
cp ../../assets/brand/icon128.png "$STAGE/RamWarden.app/Contents/Resources/RamWarden.png"
swiftc -O -target "$(uname -m)-apple-macosx13.0" -o "$STAGE/RamWarden.app/Contents/MacOS/RamWarden" main.swift Updates.swift Settings.swift Companion.swift WardenCompanion.swift Voice.swift ThreatCompanion.swift Speech.swift PetSizing.swift
cat > "$STAGE/RamWarden.app/Contents/Info.plist" <<'PL'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleIconFile</key><string>RamWarden.icns</string>
  <key>CFBundleName</key><string>RamWarden</string>
  <key>CFBundleIdentifier</key><string>com.ramwarden.menubar</string>
  <key>CFBundleExecutable</key><string>RamWarden</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleVersion</key><string>0.3.3</string>
  <key>LSUIElement</key><false/>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
</dict></plist>
PL
codesign --force --sign - "$STAGE/RamWarden.app" >/dev/null
mkdir -p "$(dirname "$APP")"
# ditto preserves bundle permissions and metadata; compilation failed => no install.
ditto "$STAGE/RamWarden.app" "$APP"
echo "built $APP"
