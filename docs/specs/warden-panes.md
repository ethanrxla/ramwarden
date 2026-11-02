# Warden panes

Every RamWarden client shows one pane per warden: **RAM** (the memory view
each platform already had) and **ThreatWarden**. The API they read is in
`wardens.md` (`GET /wardens/{id}`, `POST /wardens/{id}/setup`,
`GET /wardens/{id}/icon.svg`); what ThreatWarden is made of is in
`threatwarden.md`.

| Platform | Where | Switch |
|---|---|---|
| Linux | GTK window (`ramwarden-ui`, `threat.rs`) | linked RAM / ThreatWarden toggles above the window, with ThreatWarden's level dot |
| Windows | egui window (`tray/views.rs::threat`), tray menu item "ThreatWarden" | RAM / ThreatWarden selector above the RAM tabs, with the cyborg icon and level dot |
| macOS | menu bar (`macos/menubar/Wardens.swift`) | segmented control at the top of the menu; the status title adds ⚠︎ when ThreatWarden says "Act now" |

## Contract

- What the ThreatWarden pane says is decided once, in
  `ramwarden_ui::wardens::ThreatPane`, and both Rust GUIs only lay it out. The
  Swift menu bar mirrors the same rules (it is not a Rust program).
- **Level words and colours**: clear "All clear" green, watch "Worth a look"
  amber, alert "Act now" red, setup "Setting up" blue, off "Off" grey, locked
  "Locked" brand crimson. An unknown level from a newer daemon shows as
  "Unknown", never as an error.
- **One button at most**:
  - not unlocked: "Get RamWarden Pro" opens `upgrade_url`, with `{price}`
    under it (ThreatWarden is part of RamWarden Pro; fallbacks
    `In RamWarden Pro, from $5 a month`, `https://ramwarden.app/#pricing`). The
    locked pane lists the engines the pass turns on and nothing else, because
    a locked warden does no work. Any stale report is ignored while locked.
  - unlocked, enabled, level `off` and the agent not installed: "Turn on"
    posts `/wardens/threat/setup`. An installed agent that is off is not
    offered a reinstall.
  - Setup errors are shown to the user. On macOS, setup has a 20-second
    timeout and the button is disabled while the request is pending.
- **Engines**: the daemon's `report.engines` when present; otherwise Wazuh's
  line from agent health and the others from the alerts their groups raised.
- **Alerts**: up to 8, each tagged with the AI verdict (`threat`,
  `suspicious`, `benign`) or else the Wazuh rule level, the engine (from the
  rule groups: `clamav` → ClamAV, `suricata`/`ids` → Suricata, `syscheck` →
  File integrity, …), the time, the explanation and the advice.
- The icon is the 8-bit cyborg from `/wardens/threat/icon.svg`, read as pixel
  rects and drawn without smoothing. No client needs an SVG renderer.
- **Degrade, don't fail**: a daemon without `/wardens` (404) shows "This
  RamWarden is older than ThreatWarden"; an unreachable daemon names the error.
  The RAM pane is unaffected either way.
- The pane never acts on its own: no engine dashboard is opened, and nothing
  is deleted, quarantined or blocked from it.

## Verification

- `ramwarden-ui` `wardens` tests: locked offers only the upgrade (and the
  daemon's own price and page win); unlocked-not-installed offers Turn on;
  installed-but-off does not; alerts carry engine and verdict; daemon engine
  lines win; unknown levels parse; the sprite parser reads the real icon.
- `app.rs` live GTK test (xvfb): the ThreatWarden pane shows the upgrade
  button and the icon against a fake daemon.
- CI enables the Windows `tray` feature for clippy and Linux `gui` tests under
  xvfb. macOS runs `bash macos/menubar/test.sh`: compiles the app into a
  temporary directory and tests locked reports, setup replies and alert counts.
  `macos/menubar/build.sh` remains the local install command.
