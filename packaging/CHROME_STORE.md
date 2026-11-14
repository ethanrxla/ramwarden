# Chrome Web Store preparation

Build `scripts/build-ext.sh`, then run `python3 scripts/tests/store-package.py`.
The deterministic Chrome ZIP is `dist/ramwarden-chrome-0.3.3.zip` with an adjacent
SHA-256 checksum. The ZIP validator rejects unexpected files, likely credentials,
remote/dynamic code, expanded required hosts and incorrect icon dimensions.

The source manifest is now valid Chrome MV3. The Firefox build has its separate
background and identity fields. Chrome needs tabs, storage, alarms and tabGroups;
all are used. Normal daemon access is loopback; remote HTTP host access is opt-in.
Activity content scripts protect work across HTTP/HTTPS pages without reading
page bodies or form values. The developer manifest no longer requests invalid
WebSocket/CIDR patterns or unused notifications.

Current listing, privacy answers and reviewer steps:
[packaging/stores/chrome/listing.md](stores/chrome/listing.md).
Older docs/releases/chrome-store-0.3.0.md is historical, not the upload checklist.

Local test commands:

    node --test extension/tests/discard.cjs extension/tests/warden.cjs
    node extension/tests/popup.cjs
    node extension/tests/live.cjs

Browser tests use a disposable profile and synthetic pages/reports, never the
owner's browser. Generated screenshots go to /tmp/warden-store-review by default.
Install Playwright separately for these tests; it is not shipped in the ZIP.

Before submitting, verify live public privacy/setup URLs, final disclosures,
publisher identity, screenshots and a clean-machine desktop installer for any
connected feature being reviewed. The owner already has a publisher dashboard.
No developer fee, agreement or publication is performed by package generation.

Official requirements checked 2026-11-05:
https://developer.chrome.com/docs/webstore/cws-dashboard-privacy
https://developer.chrome.com/docs/webstore/images
