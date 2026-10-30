#!/usr/bin/env bash
# Build RamWarden.app (menu-bar only, no Dock icon) into ~/Applications.
set -euo pipefail
cd "$(dirname "$0")"
APP="$HOME/Applications/RamWarden.app"
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
  <key>CFBundleVersion</key><string>0.2.1</string>
  <key>LSUIElement</key><true/>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
</dict></plist>
PL
codesign --force --sign - "$APP" >/dev/null
echo "built $APP"
