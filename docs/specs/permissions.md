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
- Full Disk Access is the one grant the user gives by hand, so RamWarden opens
  its pane for them exactly once: the setup step opens it as soon as it finds
  the grant off, and if setup never ran (a script install) the menu-bar app
  asks once at its first run. A marker file,
  `~/Library/Application Support/RamWarden/full-disk-access-asked`, shared by
  both apps, records that it was asked. After that only the setup step's own
  button opens the pane.
- In that list the daemon reads "RamWarden" with the ram icon, not
  `ramwarden-daemon` with the generic "exec" icon: it is installed as
  `~/Library/Application Support/RamWarden/RamWarden.app`
  (`macos/daemon/bundle.sh`, background-only, bundle id
  `com.ramwarden.daemon`), its launchd job runs the bundle's executable, and
  `~/.local/bin/ramwarden-daemon` is a link to it. `GET /permissions` names the
  bundle as the program to add. The bundle icon is the bare ram edge to edge
  (`assets/brand/RamWardenDaemon.icns`), the largest it can be drawn in a row.
- On Windows the daemon and service executables carry the ram icon and a
  readable name (`RamWarden Warden`, `RamWarden Service`) for Task Manager,
  Startup apps and the firewall prompt. Linux has no per-program permission
  list; the desktop entry already uses the ram icon.

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

- `permissions::tests`: each OS lists its needs and the Full Disk Access pane,
  and the program named is the daemon's bundle when it runs from one.
- `scripts/tests/macos-install-rollback.py`: installing puts the daemon bundle
  in place with the `~/.local/bin` link; a failed install restores the old
  daemon and leaves no bundle.
- `storage::tests::folders_macos_asks_about_are_never_opened_and_are_named_instead`.
- `appquit`: no `osascript` remains (`rg osascript crates/ramwarden-daemon`).

## Basis and limitations

- Reference Mac, 2026-11-19: three mid-use prompts (Documents and Downloads,
  Apple Music, controlling an app being quit). All three came from the daemon.
- Full Disk Access is detected by opening the TCC database, which fails
  quietly without it. macOS never prompts for Full Disk Access itself.
- An ad-hoc-signed build is a new program to macOS after each rebuild, so a
  Full Disk Access grant has to be given again after updating until releases
  are signed with a stable identity. `scripts/install-macos.sh` signs the
  bundle with `RAMWARDEN_SIGN_IDENTITY`, or the first Developer ID Application
  identity in the keychain, before falling back to ad hoc.
- A grant given to the old bare `ramwarden-daemon` does not carry over to the
  bundle; its row stays in the list until removed with the minus button.
- That a bundle's main executable started by launchd is listed by bundle name
  and icon is how macOS treats bundled programs generally; it has not been
  confirmed on the reference Mac yet.
- That `NSRunningApplication.terminate()` needs no Automation grant is from
  Apple's documentation and common use. It has not been confirmed on the
  reference Mac yet.
