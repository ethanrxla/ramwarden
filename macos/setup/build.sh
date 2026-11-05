#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"
DEST="${RAMWARDEN_SETUP_DEST:-../../target/RamWarden Installer.app}"
mkdir -p "$DEST/Contents/MacOS" "$DEST/Contents/Resources"
swiftc -parse-as-library -O -target "$(uname -m)-apple-macosx13.0" main.swift SetupView.swift SetupEvents.swift RequiredModules.swift AgentPairing.swift -o "$DEST/Contents/MacOS/RamWardenSetup"
cp ../../LICENSE "$DEST/Contents/Resources/LICENSE.txt"
cp ../../assets/brand/RamWarden.icns "$DEST/Contents/Resources/"
ditto ../../scripts/threatwarden-engines "$DEST/Contents/Resources/security-modules"
ditto ../../agent/skills/ramwarden "$DEST/Contents/Resources/agent-skill"
cat > "$DEST/Contents/Info.plist" <<'PL'
<?xml version="1.0" encoding="UTF-8"?><!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd"><plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>RamWardenSetup</string><key>CFBundleIdentifier</key><string>com.ramwarden.setup</string><key>CFBundleName</key><string>RamWarden Installer</string><key>CFBundlePackageType</key><string>APPL</string><key>CFBundleIconFile</key><string>RamWarden.icns</string><key>CFBundleVersion</key><string>0.3.3</string><key>LSMinimumSystemVersion</key><string>13.0</string>
</dict></plist>
PL
SETUP_SITE="${RAMWARDEN_SETUP_SITE:-https://ramwarden.com}"
case "$SETUP_SITE" in https://ramwarden.com|http://localhost:8000) ;; *) echo 'Unexpected setup site'; exit 2 ;; esac
/usr/libexec/PlistBuddy -c "Add :WardenSiteURL string $SETUP_SITE" "$DEST/Contents/Info.plist"
codesign --force --sign - "$DEST"
printf 'Built %s\n' "$DEST"
