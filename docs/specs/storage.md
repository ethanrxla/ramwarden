# Storage: what is using the disk, a warning before it is full, and help

The warden watches free disk space, baas before a disk fills, answers "what's
eating my storage?", and walks the user through getting space back. It clears
only regenerable files, and only after the user approves. Agents (Warp,
Claude Code, Cursor) reach the same abilities through `ramwarden mcp`.

Code: `crates/ramwarden-platform/src/storage.rs` (trait),
`ramwarden-kernel`, `ramwarden-mac`, `ramwarden-nt` `storage.rs` (free space),
`crates/ramwarden-core/src/storage.rs` (levels and rationing),
`crates/ramwarden-daemon/src/storage/` (scan, catalogue, clean-up, routes),
`crates/ramwarden-cli/src/{storage,mcp}.rs`.

## Contract

- **Free space** comes from one call per volume: `statvfs` on `/proc/self/mounts`
  disks (Linux), `statfs` on `/` and `/Volumes` (macOS), `GetDiskFreeSpaceExW`
  on fixed and removable drives (Windows). The figure is space available to
  the user. Pseudo filesystems, images, loop devices and network drives are
  not listed. A platform without it reports nothing and the watcher is silent.
- **Levels.** A volume is `low` when free space is under both 20 GB and 10%,
  and `critical` under both 5 GB and 3% (`[storage]` in the config). It drops
  back a level only once free space is 10% past the line.
- **Alerts.** The monitor reads free space every 60 s, off its own thread. It
  alerts when a volume gets worse; at the same level it repeats at most once a
  day (low) or every 4 hours (critical). One alert at a time, the worst volume
  first. The alert is a storage bubble with `sound: "baa"` or `"baa_critical"`
  and a "Free N GB" button when something can be cleared. Clients play the baa
  once per bubble: macOS through `GoatAlerts` (its own 15-minute sound gap and
  mute), Windows through `audio::bleat` (honours the companion's audio mute),
  Linux through `pw-play`, `paplay` or `aplay` (none installed: silent). The
  voice being off or muted silences the bubble; the alert is still logged.
  Storage bubbles skip the voice's gap and daily cap, because the watcher
  rations them.
- **Explain.** A scan measures the catalogue's places and the top folders of
  the home folder within a time budget (8 s by default, at most 120 s, 20
  million entries), with four walker threads. It counts space on disk, follows
  no links, stays on one filesystem, and marks sizes it could not finish (out of time, or a folder it could not read) as
  minimums. Folders with a valid `CACHEDIR.TAG` (Cargo's `target/`, among
  others) are reported as build output. A scan under 10 minutes old is reused.
  Scans run only when asked or when an alert fires.
- **Kinds.** `clear`: regenerable (app and browser caches, temp files older
  than a day, crash dumps and shader caches, Xcode DerivedData, npm, pip,
  Cargo and Gradle caches, tagged build output). `review`: the user's own files
  (Downloads, iPhone backups); never touched. `guide`: needs the owning app or
  an administrator (Trash and Recycle Bin, page file, hibernation file,
  Windows Update leftovers, Docker, package caches, the systemd journal,
  Homebrew, Steam games); RamWarden shows the size and the steps.
- **Free.** A proposal lists `clear` places from the last scan, fixed at that
  moment, and expires after 15 minutes. It is applied only by the user: the
  bubble's Free button (`POST /warden/answer {"choice":"free"}`), or a typed
  "y" to `ramwarden storage free` at an interactive terminal. A proposal
  applies once. Applying re-checks each folder: a real folder below the home,
  temp or local-data folder (not `Documents`, `Desktop`, `Downloads` and the
  like wholesale), reached through no link, and still tagged if it was found
  by its tag. Only the contents go; links inside are unlinked, never followed;
  files in use are skipped and counted. The walk goes file by file: in temp
  folders every file is age-checked, at any depth, and a folder is removed
  only once it is empty. The result reports bytes of files actually removed,
  and zero is a refusal.
- **This computer only.** The `/storage` routes and `/ask/probe` are served
  only when the daemon listens on loopback, and a Free answer arriving on any
  other listener is refused.
- **Alert after easing.** A disk that eased from critical to low and then
  fills to critical again alerts at once; the 4-hour repeat applies only to
  a level that has not changed.
- **Asking.** The chat's `storage` probe (picked by words like disk, storage,
  space, drive, "free up") answers from the same scan, and when something can
  be cleared the offer appears in the bubble, without a baa.

## Agents: `ramwarden mcp`

A Model Context Protocol server over stdio, for Warp, Claude Code, Cursor or
any MCP client. Configure it with command `ramwarden`, args `["mcp"]`.

| Tool | Does |
|---|---|
| `storage_status` | Free space per disk and its level |
| `storage_usage` | What is using the disk, with kinds, sizes and steps |
| `storage_propose_cleanup` | Prepares a clean-up and shows it in the warden's bubble. Deletes nothing |
| `storage_cleanup_status` | Pending, applied (with bytes freed), declined or expired |
| `computer_probe` | One read-only probe: overview, app, gpu, models, history, storage, hardware |

There is no tool that deletes, approves or runs a command. Every argument is
validated before it becomes a daemon request on `127.0.0.1:7823`.

## Policy: what an agent or model may see

User decision, 2026-11-14: models may see app names and figures; window
titles and raw paths stay on the computer. Storage follows it: models and
agents get labels, folder names and sizes, never paths, unless the user sets
`[storage] share_paths = true`. Volume names (`C:`, `/`, `Macintosh HD`) are
shown. RamWarden's own apps get full paths.

## Verification

- `ramwarden_core::storage` tests: the smaller line decides, the recovery
  margin, alert once / when worse / after the repeat, one alert at a time,
  disabled reads but never alerts.
- `storage::scan`: sizes and order, links not followed, outermost tagged cache
  only, a spent budget gives a floor.
- `storage::catalog`: unique ids, clearable templates only under user folders,
  `*` expansion, `may_clear` refuses home itself, personal folders and links.
- `storage::cleanup`: contents go and the folder stays, empty is a refusal,
  links unlinked with targets intact, a folder swapped for a link is left
  alone, build output needs its tag at delete time, recent temp files kept.
- `storage::tests` and `tests/storage_api.rs`: scan, the agent view has no
  paths, nothing deleted until approval, apply once, declined and expired
  refused, the bubble's Free / What's using it? / Not now, the critical baa.
- `ramwarden-cli` `mcp` tests: handshake, no destructive tool, arguments that
  could steer a route refused.
- `ramwarden-kernel` mount table parsing; macOS and Windows readers compile in
  CI and have not yet run on the reference Mac and PC.

## Basis and limitations

- The reference PC's C: drive reached 0.2 GB free (2026-11) from caches, temp
  files, old build output, a 48 GB page file, Docker and games, with no
  warning. Each of those is in the catalogue, as `clear` or `guide`.
- Free space on macOS excludes purgeable space, so it reads lower than Finder.
- Hard-linked files are counted once per link, which overstates package
  stores. The Recycle Bin's size is not read.
- The scan covers the home folder and the catalogue, not a whole drive: a
  large folder elsewhere (such as `D:\games`) shows in the volume's free space
  but not in the list.
- Lite has no pet, so no bubble: the CLI, `/storage` and the MCP tools still
  work there, and the alert is logged.
