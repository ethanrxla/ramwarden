# RamWarden

A memory manager for Linux desktops. It watches memory pressure, works out what
you are actually using, and reclaims the rest — **asking the kernel to page cold
memory to zram before it will consider freezing or closing anything**, so most
pressure is handled without you noticing.

---

## Why v2 exists

**v1 was measuring the wrong number.** It summed `psutil` RSS per process name,
which double-counts every page a browser shares between its renderers. Measured
on one real machine:

| Brave, same moment | |
|---|---|
| summed RSS — what v1 reported | **15,094 MB** |
| proportional set size (`smaps_rollup`) | **6,512 MB** |
| the kernel's own cgroup charge | **6,347 MB** |

PSS and the cgroup agree; RSS does not. Every decision v1 made rested on a figure
that was out by 2.3×. v2 reads PSS and `memory.current`, which are correct by
construction.

**v1 only had destructive levers.** Its strongest action was `SIGSTOP`, which
makes a GUI application indistinguishable from a crashed one. Every desktop app
on a modern systemd session sits in its own cgroup scope, and on a normal install
`memory.reclaim` on that scope is **writable with no privileges at all** — so the
kernel will page an application's cold memory out to zram on request, reversibly,
and the application never learns it happened. Measured: asking for 400 MB from one
Brave scope returned 137 MB of real RAM with both processes still serving pages.

**And it triggered on the wrong signal.** `used / total` cannot tell a
full-but-healthy machine from a stalling one: on the same box, 23 GiB of 30 GiB
"used" included 4 GB that zram had already compressed away. v2 triggers on
Pressure Stall Information — the wall time actually lost waiting for memory.

---

## Install

```bash
sudo dpkg -i dist/ramwarden_*.deb
systemctl --user daemon-reload
systemctl --user enable --now ramwarden
systemctl --user enable --now ramwarden-helper   # optional, see below
```

Then launch **RamWarden** from your application menu, or run `ramwarden`.

### Build from source

```bash
sudo apt install libgtk-4-dev libadwaita-1-dev libcap2-bin   # for the window
cargo build --release -p ramwarden-daemon -p ramwarden-helper
cargo build --release -p ramwarden-ui --features gui --bin ramwarden
bash scripts/build-deb.sh                                     # or package it
```

The daemon and helper build on a headless machine; only the window needs GTK.

Verify the desktop data path (including a real GTK window under Xvfb):

```bash
cargo test --workspace
xvfb-run -a cargo test -p ramwarden-ui --features gui -- --test-threads=1
xvfb-run -a cargo run -p ramwarden-ui --features gui --example live_display -- 7823
```

The live display check only reads daemon state. Regression tests use a local
fixture and do not call models or act on real processes. See
[the live data spec](docs/specs/ui-live-data.md) for the runtime contract.

For diagnostics, use `journalctl --user -u ramwarden -f` for daemon logs, or
launch the window with `RAMWARDEN_LOG=ramwarden_ui=debug ramwarden` for refresh
and analysis logs. The window reports loading and connection failures directly;
normal daemon operation can be quiet when no reclamation is needed.

---

## The remediation ladder

RamWarden acts on its own, and the rungs are ordered by what it costs you if the
decision is wrong:

| Pressure | Rung | Action | Cost of being wrong |
|---|---|---|---|
| `some avg10 > 2%` | reclaim | kernel pages cold memory to zram | a few page faults |
| `> 5%` | page-out | targeted `process_madvise`, soft cap | a few page faults |
| `> 10%` | tabs | unload eligible idle browser tabs, at most five per batch | reload on activation |
| `> 15%` | suspend | `SIGSTOP` idle watchlisted apps | app looks frozen until resumed |
| `full > 25%` or < 500 MB free | kill | `SIGTERM` then `SIGKILL` | **unsaved work is gone** |

Escalation is cumulative: reaching `suspend` also does reclaim, page-out and tabs
on that tick. In practice most pressure never gets past the first rung.

Between `release_below` and `reclaim_at` the ladder **holds** — it neither acts
nor undoes. Without that band a machine hovering at the threshold would suspend
and resume the same application every tick.

When pressure passes, everything reversible is undone: suspended applications are
resumed and every soft cap is lifted. A frozen app that stays frozen after the
reason has gone is how a user ends up force-quitting a healthy application.

The kill rung arms a **cancellable countdown** rather than acting immediately. It
proceeds by default — this is a window to intervene, not a request for permission.
Set `kill_grace_seconds = 0` for no window at all.

Every action is recorded, including the ones that decided to do nothing. When
nobody is watching, that record is the only way to answer "why is my editor
frozen?" afterwards:

```bash
curl -s localhost:7823/actions | jq '.actions[0]'
```

### What it will never touch

* **Anything structurally protected** — the compositor, a virtual machine, a
  container runtime, an agent session, a terminal emulator, a build in flight.
  Absolute: no watchlist entry and no button in the window overrides it.
* **Anything outside your own session.** The daemon only ever looks inside
  `user@$UID.service`, so Docker containers and system services are unreachable
  by construction rather than by a check someone might forget.
* **Anything not on your watchlist**, for suspend and kill. Activity detection
  decides what to *spare*; it never widens what may be touched.

---

## The window

A task manager over the daemon's API. Every column is resizable and sortable, the
table scrolls horizontally, and the filter box narrows forty rows to the one you
want by name, PID, scope or status.

```
┌──────────────────────────────────────────────────────────────────────┐
│ ☐ Process        ⇔│ PID  ⇔│ RAM(PSS) ⇔│ VRAM ⇔│ Status   ⇔│ Scope  ⇔│
├──────────────────────────────────────────────────────────────────────┤
│ ☐ cosmic-comp     │  4886 │   2.7 GB  │ 66 MB │ PROTECTED │         │
│ ☑ ChatGPT         │ 89406 │   816 MB  │ 79 MB │ IDLE      │ …AppList│
│ ☐ brave           │  6669 │   5.2 GB  │275 MB │ IDLE      │ …Browser│
└──────────────────────────────────────────────────────────────────────┘
  ◂──────────── drag any column edge · scroll ────────────▸
```

Right-click a row for Reclaim / Suspend / Kill / watchlist / Copy PID. The menu
only offers what the daemon will actually permit — a structurally protected row
offers nothing but Copy PID, and a row holding a listening socket warns that its
clients would hang before you can act on it.

The RAM column's tooltip shows how far a naive RSS reading would have been out.

### Browser tabs

Open **Browser tabs · suggested cleanup** near the bottom of the compact window.
**Refresh** updates the recommendations. Rows show the title, idle time and
status; hover for the full URL, browser identity and protection reason. Select candidates and click
**Unload selected** (up to five tabs). Tabs stay in the tab strip and reload when selected;
this action does not close them. Confirmed, queued and refused outcomes are shown
separately, and no unmeasured per-tab RAM savings are claimed.

The Rust policy ranks older background tabs first and gives older duplicate
URLs priority while retaining the most recently used copy. Active, pinned,
audible, private, loading, opted-out and known editor tabs are protected.
Unknown form state is not detectable without additional page access: pin an
important form or leave it unselected. The automatic pressure ladder uses the
same policy after kernel reclaim/page-out, with a 30-second batch cooldown and
a 90-second retry guard for each target. This is a conservative policy, not a
claim of optimal eviction for every workload.

Requires the **0.2.1 extension** for safety metadata and discard commands. Build
with `bash scripts/build-ext.sh`; load/reload `build/chrome` in Chrome/Brave or
`build/firefox` in Firefox. Existing 0.1.x extensions can report tabs but their
rows say “update needed” and cannot be unloaded. The extension popup's Analyze
button now reports results instead of promising a nonexistent prompt.

```bash
node --test extension/tests/discard.cjs
cargo test -p ramwarden-daemon --test browser_pipeline
```

See [browser memory specification](docs/specs/browser-memory.md). Browser analysis
uses local Rust rules and does not call a model. The goal-based
Analyse remains a separate model-assisted diagnostic.

The window uses the original 420×600 default layout, with a 120px browser list
and a collapsible cleanup section. Refresh preserves selections and scrolling.
Manual **Close** is separate from **Unload**: select ordinary tabs and click Close,
then confirm the titles. This also works with older extensions and recently used
tabs; automatic-unload requirements do not block manual selection. Known active,
pinned, audio, private and protected pages remain guarded. Reports refresh while
the window is open. Protected tabs show their status instead of an inactive checkbox. See
[compact window specification](docs/specs/compact-window.md).

---

## Configuration

`ramwarden.toml`, found in `$RAMWARDEN_CONFIG`, the working directory,
`~/.config/ramwarden/config.toml`, then `/etc/ramwarden/config.toml`.

```toml
[thresholds]
inactivity_minutes = 45     # a tab must be idle this long to qualify

[server]
host = "127.0.0.1"
port = 7823

[processes]
watchlist = ["Discord", "burpsuite"]   # the only names suspend/kill may target

[ladder]
release_below = 1.0         # below this, undo everything reversible
reclaim_at    = 2.0
pageout_at    = 5.0
tabs_at       = 10.0
suspend_at    = 15.0
kill_at_full  = 25.0
kill_grace_seconds = 10     # 0 = kill with no cancellation window
```

Thresholds must be in order; a crossed ladder is rejected at load rather than
discovered in production.

> **A top-level key placed after a `[section]` header is parsed as part of that
> section and silently ignored.** Put `db_path` and friends above the first header.

---

## Model tiers

Decisions can be refined by a language model, but the heuristic is the floor, not
a failure mode — it always works.

```
pressure → heuristic rules  (always available)
         → local  nemotron-3-nano:4b via Ollama
         → cloud  NVIDIA NIM          (off by default)
```

```bash
docker exec ollama-gpu ollama pull nemotron-3-nano:4b
docker exec ollama-gpu ollama pull mxbai-embed-large
```

### Goal-aware tab ranking

Tell the window what you are working on and tabs are ranked against it by
embedding similarity, which replaces a hardcoded list of "time-wasting" domains
with something that respects what you are actually doing. The difference is not
subtle — the same seven tabs, with and without a stated goal:

| | tabs closed | a CVE page | PortSwigger docs |
|---|---|---|---|
| no goal — age alone decides | `1, 4, 5, 6, 7` | **closed** | **closed** |
| goal: "security research" | `1, 5, 7` | kept | kept |

**Use `mxbai-embed-large`, not `nomic-embed-text`.** Measured on a six-document
set with a security-research goal, nomic produced a 0.100 score spread and ranked
a basketball score above a CVE entry. mxbai produced 0.394 and ordered it
correctly. RamWarden refuses to act on a ranking flatter than 0.15 — an embedder
that scored everything alike has told you nothing, and guessing from noise is
worse than ignoring it.

### The optional cloud tier

```toml
# ~/.config/ramwarden/secrets.toml — NOT ramwarden.toml, which git tracks
[nvidia]
api_key = "nvapi-..."
enabled = true
```

Off by default even when a key is present: the prompt carries the titles and
hosts of every open tab, and having credentials on a machine is not the same as
consenting to send your browsing off it. The key is never logged — it appears
only as a six-character fingerprint.

---

## The page-out helper

`process_madvise` on another process needs `CAP_SYS_NICE`, and a systemd **user**
service cannot be granted capabilities — the user manager is itself unprivileged,
so `AmbientCapabilities=` does nothing there. Rather than give the daemon that
capability, a separate few-hundred-line helper holds it and does one thing:

```bash
sudo setcap cap_sys_nice+ep /usr/bin/ramwarden-helper   # the package does this
systemctl --user enable --now ramwarden-helper
```

It listens on a mode-0600 socket in `$XDG_RUNTIME_DIR`, checks every peer's
credentials against its own UID, refuses PID ≤ 1 and other users' processes, and
can only express the two non-destructive advice values. Without it the daemon
falls back to cgroup reclaim, which needs no privileges and returns most of the
memory anyway — that fallback is a normal configuration, not a broken one.

---

## Browser extensions

`extension/` is unchanged from v1 and needs no update; the daemon reproduces the
same `/ws` and `POST /api/tabs` contracts.

```bash
bash scripts/build-ext.sh   # produces Chrome and Firefox zips in dist/
```

Chrome and Brave hold a WebSocket. Firefox's extension is an event page the
browser suspends, so it polls every 30 seconds and close commands are queued for
it to collect. Brave incognito cannot be seen from outside at all — enable
"Allow in Private Windows" for the extension in `brave://extensions` if you want
those tabs counted.

---

## API

```
GET  /state            everything the window renders, in one call
GET  /ram-report       the breakdown, including how far summed RSS would mislead
GET  /psi              pressure, and the rung it currently selects
GET  /activity         every process with its verdict and the reasons for it
GET  /ladder/plan      what the ladder would do next, without doing it
POST /ladder/cancel-kill
GET  /actions          what the ladder has done unattended
GET  /ai               model tiers, GPU headroom, page-out route
GET  /signals          behaviour log, labelled for training
POST /suspend/{name}?force=true
POST /kill/{name}?force=true
POST /resume/{name}
POST /reclaim/{scope}
POST /analyze          {"goal": "..."}
GET  /history          tabs that were closed, so they can be reopened
```

`force=true` skips the watchlist and in-use checks for an explicit click. It never
skips structural protection.

---

## Architecture

```
crates/ramwarden-kernel   cgroup v2, PSI, PSS, pidfd, zram, meminfo, oom, madvise
crates/ramwarden-core     activity detection, the gate, the ladder, history
crates/ramwarden-ai       local Nemotron, goal re-ranking, NVML
crates/ramwarden-daemon   the :7823 API and the loop that drives the ladder
crates/ramwarden-helper   the setcap'd page-out shim, and nothing else
crates/ramwarden-ui       the GTK4 window
```

Every type in `ramwarden-kernel` takes a `Root`, so tests point it at a synthetic
`/proc` and `/sys/fs/cgroup` tree. That is what makes the ladder testable without
putting a real machine into swap.

```bash
cargo test --workspace --features ramwarden-ui/gui
```

### Known limitations

* **PSI triggers are unavailable on some kernels.** The daemon asks for one and
  falls back to adaptive polling (10 s idle / 2 s watchful / 1 s while
  remediating). Note that the pressure file opening `O_RDWR` proves nothing — only
  an accepted trigger write does.
* **Cold-page detection is weak on an idle machine.** It uses
  `Rss - Referenced`, and referenced bits are only cleared by kswapd during
  reclaim scanning — so at zero pressure they saturate and almost everything reads
  as hot. It works under real pressure, which is when it fires. A proper fix is
  two-phase sampling via `/proc/<pid>/clear_refs`.
* **Workspace sorting barely works on COSMIC.** `cosmic-comp` sets no
  `_NET_NUMBER_OF_DESKTOPS` and Wayland-native clients are invisible to `wmctrl`,
  so rules naming them are inert. RamWarden reports which rules can never fire
  rather than silently doing nothing.
* **A leaking compositor cannot be helped.** `cosmic-comp` lives in
  `session-$N.scope`, outside the delegated subtree and with a `memory.reclaim`
  that is not user-writable. Upgrade it and start a new session.
