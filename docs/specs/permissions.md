# Permissions: asked once, at setup

## Contract

- RamWarden shows no OS permission prompt after setup, on any OS. Every need
  is listed by `GET /permissions` (`crates/ramwarden-daemon/src/permissions.rs`)
  with why, what happens without it, whether it is granted, and where to grant
  it. Setup shows that list once: the macOS setup app's "Allow once" step, the
  summary at the end of `scripts/install.ps1`, and the Linux package's
  `postinst` (which grants the page-out helper's capability itself).
- A need left ungranted makes its feature smaller. It never causes a prompt
  later.

## Policy

| OS | Need | Granted by | Without it |
|---|---|---|---|
| macOS | Full Disk Access (optional) | The user, in System Settings, from the setup step | Storage scans leave out the folders in `catalog::MAC_PRIVATE` (Desktop, Documents, Downloads, iCloud Drive, other apps' containers, Mail, Messages, Photos, the Music and TV libraries) and name them as not measured |
| macOS | `ramwarden-quit` in RamWarden.app | `macos/menubar/build.sh` | Idle apps are not asked to quit |
| Linux | Page-out helper capability | `packaging/deb/postinst` (`setcap`) | Per-process reclaim is skipped |
| Windows | RamWarden service | `scripts/install.ps1`, one UAC prompt | Privileged trims and debloat are unavailable |

What the daemon must never do at run time, because each makes macOS prompt:

- open, list or stat a folder in `MAC_PRIVATE` without Full Disk Access;
- send an Apple event (`osascript`, `tell application`). Apps are asked to
  quit with `ramwarden-quit`, which uses AppKit's
  `NSRunningApplication.terminate()`: the app still gets its own quit request
  and save prompt.

User-started actions that need an administrator (the ThreatWarden module
installer) show the OS's own password prompt when the user presses the
button. That is the user's action, not a mid-use prompt.

## Verification

- `permissions::tests`: each OS lists its needs and the Full Disk Access pane.
- `storage::tests::folders_macos_asks_about_are_never_opened_and_are_named_instead`.
- `appquit`: no `osascript` remains (`rg osascript crates/ramwarden-daemon`).

## Basis and limitations

- Reference Mac, 2026-11-19: three mid-use prompts (Documents and Downloads,
  Apple Music, controlling an app being quit). All three came from the daemon.
- Full Disk Access is detected by opening the TCC database, which fails
  quietly without it. macOS never prompts for Full Disk Access itself.
- An ad-hoc-signed build is a new program to macOS after each rebuild, so a
  Full Disk Access grant has to be given again after updating until releases
  are signed with a stable identity.
- That `NSRunningApplication.terminate()` needs no Automation grant is from
  Apple's documentation and common use. It has not been confirmed on the
  reference Mac yet.
