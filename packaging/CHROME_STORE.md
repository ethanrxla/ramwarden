# Chrome Web Store submission

Build the upload with:

```bash
./scripts/build-ext.sh      # → dist/ramwarden-chrome-<version>.zip
```

Upload that zip at <https://chrome.google.com/webstore/devconsole>.

---

## What the store build changes, and why

`extension/manifest.json` is the development manifest — it loads unpacked in both
Chrome and Firefox and is deliberately permissive. The store build is generated from
it by `scripts/build-ext.sh`. The differences are all things that would have drawn a
rejection or a warning:

| Dev manifest | Chrome build | Reason |
|---|---|---|
| `permissions: [tabs, storage, notifications, alarms]` | `[tabs, storage]` | `chrome.notifications` is never called, and the `alarms` path is the Firefox polling fallback guarded by `IS_FIREFOX`. Requesting permissions the code does not use violates the minimum-permissions policy and is a standard rejection reason. |
| `host_permissions` includes `http://*/*` | `optional_host_permissions: [http://*/*]` | A broad host permission puts the item into extended review and needs a justification the extension cannot really make — it only ever talks to one daemon. It is now requested at runtime, from the popup, only if the user points RamWarden at a non-local daemon. |
| `host_permissions` includes `ws://…` | removed | `ws` is not a valid match-pattern scheme (only `http`, `https`, `file`, and `*` are). Chrome was dropping these entries with a warning. WebSocket connections from a service worker are not gated by host permissions, so nothing is lost. |
| `http://100.64.0.0/10` | removed | CIDR notation is not supported in match patterns. Chrome parsed this as host `100.64.0.0` with path `/10`, which matches nothing useful — it never covered the Tailscale range it was meant to. That case is now handled by the optional permission. |
| `background: {service_worker, scripts}` | `{service_worker}` | `scripts` is the Firefox MV3 event-page key; Chrome flags it as unrecognised. |
| `browser_specific_settings` | removed | Firefox-only key. |
| no top-level `icons` | `icons: {16,48,128}` | The store uses the 128px icon for the listing. |
| — | `minimum_chrome_version: "116"` | WebSockets in service workers, which the transport depends on, are only reliable from 116. |

The JavaScript is byte-identical across both builds; it branches on `IS_FIREFOX` at
runtime. Only the manifest is generated.

---

## Before you can submit

- [ ] **Developer account** — one-time $5 USD registration fee.
- [ ] **Privacy policy URL.** Required for any item that declares `tabs`. It must be
      publicly reachable and describe what is collected. RamWarden's honest answer is
      short: tab URLs, titles, and last-active timestamps are sent to a daemon on the
      user's own machine, and nothing leaves it.
- [ ] **Screenshots** — at least one, 1280×800 or 640×400 PNG. The popup with a few
      tabs listed and the daemon connected is the obvious shot.
- [ ] **Single-purpose statement** — see below.
- [ ] Decide whether the listing is **public or unlisted**. Unlisted is the sane choice
      while the daemon still has to be installed separately.

---

## Answers to paste into the dashboard

**Single purpose:**
> RamWarden reports tab activity to a RAM-management daemon running on the same
> machine, and closes tabs the user has approved for closing, so the browser gives
> memory back before the system runs out.

**Why `tabs`:**
> The extension reads tab URLs, titles, and last-active times so the local daemon can
> identify which tabs have been idle long enough to be worth closing, and closes the
> specific tabs the user confirms.

**Why `storage`:**
> Stores the per-browser identifier used to route close commands back to the correct
> browser, the tab activity timestamps, and the daemon address setting.

**Why the host permission:**
> The extension connects to the RamWarden daemon on the user's own computer at
> localhost:7823. The optional broader permission is requested only when a user
> explicitly configures a different daemon address, such as a machine on their private
> network.

**Remote code:** No. Everything executes from the package; nothing is fetched and run.

---

## Expect a question about the daemon

The extension is inert without the native RamWarden daemon, which is not distributed
through the store. Reviewers sometimes flag this pattern. Two things make it
defensible, and both are already true:

- the extension fails soft — with no daemon it shows a red `!` badge and does nothing;
  it never breaks browsing.
- it talks only to loopback by default, and the broad permission is opt-in.

Link the daemon's install instructions from the listing description so a reviewer can
see what the other half is.
