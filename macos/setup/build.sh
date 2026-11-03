#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"
DEST="${RAMWARDEN_SETUP_DEST:-../../target/RamWarden Setup.app}"
mkdir -p "$DEST/Contents/MacOS" "$DEST/Contents/Resources"
swiftc -parse-as-library -O -target "$(uname -m)-apple-macosx13.0" main.swift -o "$DEST/Contents/MacOS/RamWardenSetup"
cp ../../assets/brand/RamWarden.icns "$DEST/Contents/Resources/"
cat > "$DEST/Contents/Info.plist" <<'PL'
<?xml version="1.0" encoding="UTF-8"?><!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd"><plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>RamWardenSetup</string><key>CFBundleIdentifier</key><string>com.ramwarden.setup</string><key>CFBundleName</key><string>RamWarden Setup</string><key>CFBundlePackageType</key><string>APPL</string><key>CFBundleIconFile</key><string>RamWarden.icns</string><key>CFBundleVersion</key><string>0.2.2</string><key>LSMinimumSystemVersion</key><string>13.0</string>
</dict></plist>
PL
codesign --force --sign - "$DEST"
printf 'Built %s\n' "$DEST"
