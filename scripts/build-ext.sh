#!/usr/bin/env bash
# Build the browser extension packages.
#
#   scripts/build-ext.sh
#
# Produces:
#   dist/ramwarden-chrome-<version>.zip    Chrome Web Store upload
#   dist/ramwarden-firefox-<version>.zip   addons.mozilla.org / temporary load
#
# extension/ stays the single source of truth. Only the manifest differs between
# targets — the JS is byte-identical and branches on IS_FIREFOX at runtime.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC="$ROOT/extension"
BUILD="$ROOT/build"
DIST="$ROOT/dist"

VERSION=$(python3 -c "import json;print(json.load(open('$SRC/manifest.json'))['version'])")

rm -rf "$BUILD"
mkdir -p "$BUILD/chrome" "$BUILD/firefox" "$DIST"

for target in chrome firefox; do
    cp "$SRC"/*.js "$SRC"/*.html "$BUILD/$target/"
    cp -r "$SRC/icons" "$BUILD/$target/"
done

python3 - "$SRC/manifest.json" "$BUILD" <<'PY'
import json, sys, pathlib

src, build = sys.argv[1], pathlib.Path(sys.argv[2])
base = json.load(open(src))

icons = {"16": "icons/icon16.png", "48": "icons/icon48.png", "128": "icons/icon128.png"}

# ── Chrome ────────────────────────────────────────────────────────────────────
# Every permission here has to be one the code actually calls; the Web Store
# rejects unused permissions under its minimum-permissions policy.
#   - "notifications" is dropped: chrome.notifications is never called.
#   - "alarms" is dropped: the alarm path is the Firefox polling fallback, guarded
#     by IS_FIREFOX. Chrome uses the WebSocket instead.
#   - ws:// is dropped from host_permissions: it is not a valid match-pattern
#     scheme, and WebSockets from a service worker are not gated by host
#     permissions anyway.
#   - the broad http://*/* moves to optional_host_permissions, requested at
#     runtime only if the user points the extension at a non-local daemon.
chrome = {
    "manifest_version": 3,
    "name": base["name"],
    "version": base["version"],
    "description": base["description"],
    "minimum_chrome_version": "116",   # WebSockets in service workers
    "icons": icons,
    "permissions": ["tabs", "storage"],
    "host_permissions": [
        "http://localhost:7823/*",
        "http://127.0.0.1:7823/*",
    ],
    "optional_host_permissions": ["http://*/*"],
    "background": {"service_worker": "background.js"},
    "action": base["action"],
}

# ── Firefox ───────────────────────────────────────────────────────────────────
# Firefox MV3 uses an event page ("scripts"), not a service worker, and needs
# "alarms" for the polling transport.
firefox = {
    "manifest_version": 3,
    "name": base["name"],
    "version": base["version"],
    "description": base["description"],
    "icons": icons,
    "permissions": ["tabs", "storage", "alarms"],
    "host_permissions": [
        "http://localhost:7823/*",
        "http://127.0.0.1:7823/*",
    ],
    "optional_host_permissions": ["http://*/*"],
    "background": {"scripts": ["background.js"]},
    "action": base["action"],
    "browser_specific_settings": base["browser_specific_settings"],
}

for name, manifest in (("chrome", chrome), ("firefox", firefox)):
    out = build / name / "manifest.json"
    out.write_text(json.dumps(manifest, indent=2) + "\n")
    print(f"  {name}: {len(manifest['permissions'])} permissions, "
          f"{len(manifest['host_permissions'])} host permissions")
PY

for target in chrome firefox; do
    zip_out="$DIST/ramwarden-${target}-${VERSION}.zip"
    rm -f "$zip_out"
    ( cd "$BUILD/$target" && zip -qr "$zip_out" . -x '.*' )
    echo "$zip_out"
done
