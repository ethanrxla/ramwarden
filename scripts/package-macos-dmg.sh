#!/usr/bin/env bash
# A self-contained, drag-to-Applications installer. No API keys enter the image.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${RAMWARDEN_PACKAGE_DIR:-$ROOT/target/macos-package}"
IDENTITY="${RAMWARDEN_SIGN_IDENTITY:-Developer ID Application: ETHAN JAMES RISDEN (KC39RL5C7W)}"
# A release without either trust key breaks existing plans or disables updates.
: "${RAMWARDEN_ENTITLEMENT_PUBLIC_KEY:?pass verification public key required}"
: "${RAMWARDEN_UPDATE_PUBLIC_KEY:?update verification public key required}"
python3 - "$ROOT/target/release/ramwarden-daemon" "$ROOT/target/release/ramwarden-launcher" <<'CHECK_KEYS'
import os,pathlib,sys
for path,key in zip(sys.argv[1:],["RAMWARDEN_ENTITLEMENT_PUBLIC_KEY","RAMWARDEN_UPDATE_PUBLIC_KEY"]):
    if os.environ[key].encode() not in pathlib.Path(path).read_bytes():
        raise SystemExit("Rebuild release binaries with "+key+" before packaging")
CHECK_KEYS
mkdir -p "$OUT"
SIGN=(--force --sign "$IDENTITY")
if [ "$IDENTITY" != '-' ]; then SIGN+=(--options runtime --timestamp); fi
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
APP="$STAGE/RamWarden Installer.app"
RAMWARDEN_SETUP_DEST="$APP" bash "$ROOT/macos/setup/build.sh"
RES="$APP/Contents/Resources"
mkdir -p "$RES/payload"
RAMWARDEN_APP_DEST="$RES/payload/RamWarden.app" bash "$ROOT/macos/menubar/build.sh"
codesign "${SIGN[@]}" "$RES/payload/RamWarden.app"
ditto "$RES/payload/RamWarden.app" "$STAGE/RamWarden.app"
for ITEM in ramwarden-daemon ramwarden-usage-sync ramwarden ramwarden-launcher; do
  test -x "$ROOT/target/release/$ITEM"
  install -m 755 "$ROOT/target/release/$ITEM" "$RES/payload/$ITEM"
  codesign "${SIGN[@]}" "$RES/payload/$ITEM"
done
ditto "$ROOT/agent/skills/ramwarden" "$RES/agent-skill"
cp "$ROOT/packaging/macos/config.toml" "$RES/payload/config.toml"
ditto "$ROOT/extension" "$RES/payload/extension"
install -m 755 "$ROOT/macos/setup/install-payload.sh" "$RES/install-payload.sh"
bash "$RES/install-payload.sh" --verify
codesign "${SIGN[@]}" "$APP"
codesign --verify --deep --strict --verbose=2 "$APP"
ln -s /Applications "$STAGE/Applications"
printf '%s\n' 'Open RamWarden Installer for the license, account connection and bundled component installation. RamWarden.app is the separate everyday companion app; only that app belongs in Applications. The installer preserves existing settings and credentials.' > "$STAGE/READ ME.txt"
hdiutil create -volname 'RamWarden' -srcfolder "$STAGE" -ov -format UDZO "$OUT/RamWarden.dmg"
codesign "${SIGN[@]}" "$OUT/RamWarden.dmg"
printf 'Built %s\n' "$OUT/RamWarden.dmg"
# Public distribution still requires notarization and stapling with the owner’s
# configured notarytool keychain profile. Never bypass Gatekeeper checks.
if [ -n "${RAMWARDEN_NOTARY_PROFILE:-}" ]; then
  xcrun notarytool submit "$OUT/RamWarden.dmg" --keychain-profile "$RAMWARDEN_NOTARY_PROFILE" --wait
  xcrun stapler staple "$OUT/RamWarden.dmg"
  spctl --assess --type open --context context:primary-signature --verbose=2 "$OUT/RamWarden.dmg"
fi

# Stapling changes the disk image; checksum the final distributable bytes.
shasum -a 256 "$OUT/RamWarden.dmg" > "$OUT/RamWarden.dmg.sha256"
