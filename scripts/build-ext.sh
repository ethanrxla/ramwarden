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

mkdir -p "$BUILD"
rm -rf "$BUILD/chrome" "$BUILD/firefox"
mkdir -p "$BUILD/chrome" "$BUILD/firefox" "$DIST"

for target in chrome firefox; do
    for file in activity.js background.js popup.js warden.js popup.html; do
        cp "$SRC/$file" "$BUILD/$target/"
    done
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
#   - "tabGroups" maintains the reversible Idle group.
#   - "alarms" drives the opted-in standalone Lite browser cleanup.
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
    "content_scripts": base["content_scripts"],
    "permissions": ["tabs", "storage", "alarms", "tabGroups"],
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
    "content_scripts": base["content_scripts"],
    "permissions": ["tabs", "storage", "alarms"],
    "host_permissions": [
        "http://localhost:7823/*",
        "http://127.0.0.1:7823/*",
    ],
    "optional_host_permissions": ["http://*/*"],
    "background": {"scripts": ["background.js"]},
    "action": base["action"],
    "browser_specific_settings": {"gecko":{"id":"ramwarden@local","strict_min_version":"109.0"}},
}

for name, manifest in (("chrome", chrome), ("firefox", firefox)):
    out = build / name / "manifest.json"
    out.write_text(json.dumps(manifest, indent=2) + "\n")
    print(f"  {name}: {len(manifest['permissions'])} permissions, "
          f"{len(manifest['host_permissions'])} host permissions")
PY

python3 - "$BUILD" "$DIST" "$VERSION" <<'ZIP'
import pathlib,sys,zipfile,hashlib
build,dist,version=pathlib.Path(sys.argv[1]),pathlib.Path(sys.argv[2]),sys.argv[3]
for target in ['chrome','firefox']:
    output=dist/f'ramwarden-{target}-{version}.zip'
    with zipfile.ZipFile(output,'w',compression=zipfile.ZIP_DEFLATED,compresslevel=9) as archive:
        for file in sorted((build/target).rglob('*')):
            if file.is_file():
                info=zipfile.ZipInfo(file.relative_to(build/target).as_posix(),(1980,1,1,0,0,0))
                info.compress_type=zipfile.ZIP_DEFLATED;info.external_attr=0o100644<<16
                archive.writestr(info,file.read_bytes())
    output.with_suffix(output.suffix+'.sha256').write_text(hashlib.sha256(output.read_bytes()).hexdigest()+'  '+output.name+'\n')
    print(output)
ZIP
python3 "$ROOT/scripts/check-store-package.py" "$DIST/ramwarden-chrome-$VERSION.zip"
