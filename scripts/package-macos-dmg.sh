#!/usr/bin/env bash
# A self-contained, drag-to-Applications installer. No API keys enter the image.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${RAMWARDEN_PACKAGE_DIR:-$ROOT/target/macos-package}"
IDENTITY="${RAMWARDEN_SIGN_IDENTITY:-Developer ID Application: ETHAN JAMES RISDEN (KC39RL5C7W)}"
mkdir -p "$OUT"
SIGN=(--force --sign "$IDENTITY")
if [ "$IDENTITY" != '-' ]; then SIGN+=(--options runtime --timestamp); fi
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
APP="$STAGE/RamWarden Setup.app"
RAMWARDEN_SETUP_DEST="$APP" bash "$ROOT/macos/setup/build.sh"
RES="$APP/Contents/Resources"
mkdir -p "$RES/payload"
for ITEM in ramwarden-daemon ramwarden-usage-sync; do
  test -x "$ROOT/target/release/$ITEM"
  install -m 755 "$ROOT/target/release/$ITEM" "$RES/payload/$ITEM"
  codesign "${SIGN[@]}" "$RES/payload/$ITEM"
done
RAMWARDEN_APP_DEST="$RES/payload/RamWarden.app" bash "$ROOT/macos/menubar/build.sh"
codesign "${SIGN[@]}" "$RES/payload/RamWarden.app"
cp "$ROOT/packaging/macos/config.toml" "$RES/payload/config.toml"
ditto "$ROOT/extension" "$RES/payload/extension"
install -m 755 "$ROOT/macos/setup/install-payload.sh" "$RES/install-payload.sh"
bash "$RES/install-payload.sh" --verify
codesign "${SIGN[@]}" "$APP"
codesign --verify --deep --strict --verbose=2 "$APP"
ln -s /Applications "$STAGE/Applications"
printf '%s\n' 'Drag RamWarden Setup to Applications, then open it. It installs the bundled daemon and companion, preserves your settings, and guides each computer’s account approval.' > "$STAGE/READ ME.txt"
hdiutil create -volname 'RamWarden' -srcfolder "$STAGE" -ov -format UDZO "$OUT/RamWarden.dmg"
codesign "${SIGN[@]}" "$OUT/RamWarden.dmg"
shasum -a 256 "$OUT/RamWarden.dmg" > "$OUT/RamWarden.dmg.sha256"
printf 'Built %s\n' "$OUT/RamWarden.dmg"
# Public distribution still requires notarization and stapling with the owner’s
# configured notarytool keychain profile. Never bypass Gatekeeper checks.
if [ -n "${RAMWARDEN_NOTARY_PROFILE:-}" ]; then
  xcrun notarytool submit "$OUT/RamWarden.dmg" --keychain-profile "$RAMWARDEN_NOTARY_PROFILE" --wait
  xcrun stapler staple "$OUT/RamWarden.dmg"
  spctl --assess --type open --context context:primary-signature --verbose=2 "$OUT/RamWarden.dmg"
fi
