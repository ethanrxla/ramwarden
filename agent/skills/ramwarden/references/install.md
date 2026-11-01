# Installing RamWarden

Installing runs software as the user and starts a background service, so tell
the user what you are about to install and get a yes first. The free RAM Warden
needs no account, no payment and no network after install.

Releases live at `https://github.com/ethanrxla/ramwarden/releases/latest`.
Asset names carry no version, so these links stay stable:

| OS | Asset | Notes |
| --- | --- | --- |
| Windows 11 | `ramwarden-windows-x86_64.exe` | Installer; Windows shows a UAC prompt the user approves |
| macOS (Apple silicon) | `RamWarden.pkg` | Installer; macOS shows its own dialogs |
| Debian / Ubuntu | `ramwarden-linux-x86_64.deb` | System-wide, includes the page-out helper |
| Other systemd Linux | `ramwarden-linux-x86_64.tar.gz` | Per-user, no root |

Each release also carries `SHA256SUMS`. Always check the download against it
before running anything.

## Steps

`BASE=https://github.com/ethanrxla/ramwarden/releases/latest/download`

1. Download the asset for this machine and `SHA256SUMS` into a new, empty
   temporary folder.
2. Verify the checksum, and stop if it does not match:
   - Linux: `sha256sum --check --ignore-missing SHA256SUMS`
   - macOS: `shasum -a 256 --check --ignore-missing SHA256SUMS`
   - Windows (PowerShell): `(Get-FileHash .\ramwarden-windows-x86_64.exe -Algorithm SHA256).Hash`
     and compare it with that file's line in `SHA256SUMS` (case-insensitive).
3. Install:
   - Windows: run the `.exe`. The user clicks through the installer.
   - macOS: `open RamWarden.pkg`. The user clicks through the installer.
   - Debian / Ubuntu: `sudo apt install ./ramwarden-linux-x86_64.deb`, then
     `systemctl --user daemon-reload && systemctl --user enable --now ramwarden`.
   - Other Linux: `mkdir rw && tar xzf ramwarden-linux-x86_64.tar.gz -C rw && (cd rw && sh install.sh)`.
4. Check it is running: `curl -s http://127.0.0.1:7823/health` answers
   `"ok": true` within a few seconds.

Every installer also installs `ramwarden-launcher`, which checks for signed
updates every six hours. Never disable it to save memory; it exits between
checks.

If the release or an asset is missing (a 404), there is no published build for
this platform yet. Tell the user, and point them at the build-from-source steps
in the repository's `README.md` instead of guessing another URL.

## The browser extension

The daemon unloads tabs through the RamWarden extension (Chrome, Brave, Edge
and other Chromium browsers). Until it is in the Chrome Web Store it is
installed unpacked: the user opens `chrome://extensions`, turns on Developer
mode, chooses **Load unpacked** and picks the `extension/` folder from the
repository or release. You cannot do these clicks for them; give the steps.
`GET /stats` shows `browsers_connected` once it is talking to the daemon.

## Configuration

The config file is `~/.config/ramwarden/config.toml` on Linux and macOS and
`%APPDATA%\ramwarden\config.toml` on Windows. Never read or copy
`secrets.toml` or `.env` beside it: they hold the user's keys. The one setting worth raising with the
user is `[processes].watchlist`: the names RamWarden may freeze or close. It
ships empty on macOS and short elsewhere, so suspend and kill do nothing until
the user adds apps. Suggest names from `GET /apps` (big, idle, and safe to
lose state in), but let the user decide and edit it; restart the service
afterwards.

## Uninstall

- Windows: Settings, Apps, RamWarden, Uninstall.
- macOS and per-user Linux: `sh install.sh uninstall` from the unpacked
  tarball (keeps the config).
- Debian / Ubuntu: `sudo apt remove ramwarden`.
