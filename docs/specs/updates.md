# Installers, the launcher and updates

How RamWarden gets onto a machine, and how every installed copy is moved to a
new release. Code: `crates/ramwarden-launcher`, `packaging/`, `scripts/package-*`,
`.github/workflows/release.yml`.

## Contract

### Wardens

A warden is one agent the user runs. A warden that ships as its own binary has
a stable id, a tier and an installer per platform in the manifest:

| id | Name | Tier | Ships as |
| --- | --- | --- | --- |
| `ram` | RAM Warden | free | the RamWarden installer (daemon, tray or menu-bar app, launcher) |

Wardens built into the daemon need no manifest entry of their own: the Wazuh
warden ships inside `ramwarden-daemon` and is unlocked at run time by its pass
(see `GET /wardens` on the daemon), so it updates with the RAM Warden. The
Wazuh agent itself comes from Wazuh's official installers (`docs/wazuh-setup.md`).

A warden that ships as its own binary gets its own manifest entry and
installers; the launcher has no list of wardens beyond the manifest. A `pro`
entry is downloaded only with the device's pass (below).

### Release assets

Every GitHub release `v<version>` carries:

| File | For |
| --- | --- |
| `ramwarden-windows-x86_64.exe` | Windows install and update (Inno Setup, runs `install.ps1`) |
| `ramwarden-macos-aarch64.tar.gz` | macOS update (runs `install.sh` as the user) |
| `RamWarden.pkg` | macOS first install (runs the same tarball's `install.sh` as the console user) |
| `ramwarden-linux-x86_64.deb` | Debian/Ubuntu install and update |
| `ramwarden-linux-x86_64.tar.gz` | per-user install on any systemd distro |
| `manifest.json`, `manifest.json.sig` | what installed launchers read |
| `SHA256SUMS` | for people checking by hand |

Update assets are named `<name>-<os>-<arch>.<exe|deb|tar.gz>`; the name alone
tells `release manifest` which machine each is for. Names carry no version, so
`releases/latest/download/<name>` is a stable link for the website's download
buttons (`web/assets/config.js`); the version is in the release's path.

### Manifest

`https://github.com/ethanrxla/ramwarden/releases/latest/download/manifest.json`
(`[updates].manifest_url`), signed by `manifest.json.sig`: base64 Ed25519 over
the file's exact bytes. Fields: `schema` (1), `sequence` (rises with every
publish), `published_at`, and `wardens[]` of `{id, name, tier, version, icon?,
summary?, health?, artifacts[{target, format, url, sha256, size}]}`. See
`manifest.rs` for an example.

### Launcher

```
ramwarden-launcher                 status: each warden's installed and latest version, running or not
ramwarden-launcher check           check now
ramwarden-launcher update [<id>]   install newer versions (RAM Warden first)
ramwarden-launcher install <id>    add a warden
```

A scheduled `check --background` runs every six hours (Windows task
`RamWarden updates`, launchd `com.ramwarden.updater`, systemd
`ramwarden-update.timer`). State, including what the last check found, is in
`state.json` under `%LOCALAPPDATA%\ramwarden\launcher` or
`~/.local/share/ramwarden/launcher`, for the tray and menu-bar app to show.

Config (`config.toml`):

```toml
[updates]
mode = "notify"        # off | notify | install
manifest_url = "https://github.com/ethanrxla/ramwarden/releases/latest/download/manifest.json"
check_hours = 6
```

### Paid wardens

A `pro` warden's artifact URLs point at the hosted API, which serves them to
`Authorization: Bearer <device token>`. The launcher reads the token from
`<config dir>/ramwarden/device-token`, written by the app after device sign-in
(`docs/specs/website.md`). No token: the warden is listed as locked and never
fetched. A 401 or 403 is reported as "needs a Pro pass".

## Policy

- **The signature is the trust, not HTTPS.** Manifest keys are compiled in from
  `crates/ramwarden-launcher/update-keys.txt`; nothing on disk can add one.
  Installers are trusted only by size and SHA-256 from the signed manifest, so
  the download host can withhold an update but not substitute one.
- **No replays.** The launcher stores the highest `sequence` accepted and
  refuses a lower one, so an old genuine manifest cannot pin users on an old
  build.
- **Never downgrade.** A manifest version at or below the installed one is
  "up to date".
- **One install path per platform.** The launcher runs the platform installer
  (`install.ps1` via the setup exe, `apt-get` on the `.deb`, `install.sh` in the
  tarball). A fresh install and an update cannot drift apart.
- **Nobody is asked anything unattended.** `mode = "install"` installs in the
  background only where no prompt is needed (the macOS and Linux tarballs).
  Windows (UAC) and the `.deb` (pkexec) install only when the user runs
  `update` or clicks Update.
- **Report what happened.** A Windows RAM update replaces the launcher, so the
  launcher starts the installer and exits; that is reported as *started*, and
  confirmed by the next launcher run seeing its own new version.
- **Degrade, don't fail.** A build with no key, no network or no manifest keeps
  running the installed version and says why in `status`.
- **Downloads are confined.** File names come from the URL's last segment and
  must be plain; tarball entries that are absolute, contain `..` or are links
  are refused before anything is written.
- **Keys and releases never touch the user's API keys.** Release tarballs carry
  no `.env`; `scripts/install-macos.sh` copies one only for source installs.

## Verification

- `cargo test -p ramwarden-launcher`: signature accept, tamper, rotation, no-key;
  manifest validation (HTTPS only, hash shape, path climb, duplicate id, remote
  health URL, future schema); replay refusal; never-downgrade; hash mismatch
  deletes the download; Pro needs and sends the token; a tarball add-on installs
  and is recorded; a pending Windows update is confirmed; installer command
  lines per format; tarball path safety, including what `tar -C stage .` builds.
- `ramwarden-launcher release verify dist/manifest.json` runs in the release
  workflow, so a manifest signed by a key the launcher does not ship never
  publishes.
- Not covered by CI: the Inno Setup, `.pkg` and `.deb` installers themselves.
  Test each by hand on a clean machine before announcing a release.

## Basis and limitations

- Single channel. Prereleases (`v0.3.0-beta.1`) publish but `releases/latest`
  skips them, so no launcher sees them unless its `manifest_url` is changed.
- The Windows installer registers the daemon task for the user who approves
  UAC. On a standard account elevated by a different admin, that is the admin;
  install from the account that will use RamWarden.
- The Linux release is built on Ubuntu 24.04 (glibc 2.39). Older distros need
  a source build.
- macOS builds are Apple silicon only.
- Rollback is reinstalling an older release's installer from the releases page;
  the launcher keeps no previous copy.
