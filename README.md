# RamWarden

A RAM-management daemon for Linux desktops. It watches memory pressure, works out
what you are *actually* using, and offers to reclaim the rest — stale browser tabs,
idle terminals, and forgotten background apps — instead of letting the OOM killer
decide for you.

---

## Run the app

From the repository root:

```bash
cd ~/ramwarden
python3 -m daemon.main
```

That is the command. It opens the RamWarden window and starts the monitor loop and
the HTTP/WebSocket server on `http://127.0.0.1:7823` (port and bind address come
from `[server]` in `ramwarden.toml`).

Stop it with `Ctrl-C`. Closing the window only hides it — the daemon keeps running,
and `curl -X POST localhost:7823/window/show` brings it back.

### First-time setup

```bash
cd ~/ramwarden
pip install -r requirements.txt
sudo apt install python3-gi gir1.2-gtk-3.0   # GTK window (Debian/Ubuntu/Pop!_OS)
```

If GTK is missing, RamWarden still runs and falls back to a text prompt in the terminal.

### Optional: Claude-powered analysis

Heuristics run with no API key at all. To enable the smarter tier, export a key
before starting:

```bash
export ANTHROPIC_API_KEY=sk-ant-...
python3 -m daemon.main
```

You can also put it in `secrets.toml` next to `ramwarden.toml` (gitignored):

```toml
[api]
key = "sk-ant-..."
```

Never put a real key in `ramwarden.toml` — that file is tracked by git.

### Run it in the background

```bash
cd ~/ramwarden
nohup python3 -m daemon.main > ~/.local/share/ramwarden/daemon.log 2>&1 &
```

Or install the bundled systemd user unit so it starts with your desktop session:

```bash
mkdir -p ~/.config/systemd/user
cp ~/ramwarden/ramwarden.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now ramwarden
journalctl --user -u ramwarden -f     # follow the log
```

---

## The window

RamWarden is a small window, not a popup. It sits out of the way showing live
memory pressure and a colour-coded list of what it believes you are using:

- **blue — protected**: never touched. Virtual machines, container runtimes, the
  compositor, agent sessions, terminals, and anything holding a listening socket.
- **green — in use**: you are demonstrably using it right now.
- **amber — idle**: no activity signal. Reclaimable, but only if you also put it
  on the watchlist.

Hover any row to see the reason for its verdict. When RAM crosses the threshold the
window raises itself with a suggested cleanup already loaded, each item with its own
checkbox — it never seizes the screen with a modal. Type what you are working on in
the goal box and hit **Analyze** to have the suggestion account for it.

If GTK is unavailable the daemon still runs headless and falls back to a terminal
prompt at threshold time.

---

## What it will not touch

The old version classified processes by name, and anything it did not recognise fell
through to "idle" — which made a 4 GB QEMU virtual machine a suspend candidate. It
now decides from live signals instead:

| Signal | Meaning |
|---|---|
| listening socket | it is a server — suspending it hangs whatever is connected |
| focused window | you are looking at it |
| audio stream | it is playing or recording |
| CPU in the sample window | it is doing work |
| active descendant | a child is compiling, syncing, or running |
| started minutes ago | you just launched it |

On top of that, some processes are protected structurally no matter how quiet they
look: hypervisors, container runtimes, the compositor, agent sessions, terminal
emulators, system services, media players, builds, and file sync.

Two rules make this safe:

1. **Nothing outside the watchlist is ever suspended.** Dynamic detection only ever
   narrows what may be touched — it never widens it.
2. **Structural protection cannot be forced.** `?force=true` skips the watchlist and
   in-use checks; it will still refuse to freeze a VM.

Ask RamWarden about anything:

```bash
curl -s localhost:7823/activity | jq             # the whole picture
curl -s localhost:7823/activity/qemu-system-x86_64 | jq   # why one process is spared
```

```json
{
  "name": "qemu-system-x86_64",
  "verdict": "PROTECTED",
  "suspendable": false,
  "explanation": "qemu-system-x86_64: virtual machine — a stalled guest can corrupt its disk"
}
```

---

## Check that it is working

```bash
curl -s localhost:7823/health              # {"ok":true}
curl -s localhost:7823/ram-report | jq     # what is using RAM and what can be freed
curl -s localhost:7823/stats | jq          # live snapshot + connected browsers
curl -s -X POST localhost:7823/analyze     # force an analysis + raise the window
curl -s -X POST "localhost:7823/analyze?goal=bug+bounty"   # analyse with a goal
curl -s localhost:7823/activity | jq       # what is protected, in use, and idle
curl -s -X POST localhost:7823/window/show # raise the window
```

---

## What it does

| Piece | Behaviour |
|---|---|
| **Monitor** | Polls RAM every 10s. Below `ram_percent`, it stays quiet. |
| **Activity detector** | Samples listening sockets, focus, audio, CPU deltas, process ancestry and uptime every tick, and rules each process PROTECTED / IN USE / IDLE. Every suspend passes through it. |
| **Analyzer** | Tier 1 quiet, Tier 2 local heuristics, Tier 3 Claude — the API is only called when memory is genuinely in trouble. |
| **Window** | Persistent small panel. Shows live pressure and verdicts; raises itself with a per-item checklist at threshold. Nothing is closed without confirmation. |
| **Browser extension** | Reports open tabs and their idle time, and executes close commands. Load `extension/` as an unpacked extension. |
| **Terminals** | Closes only shells with zero child processes — a shell running an agent or a server is never touched. |

---

## Configuration

Edit `ramwarden.toml` in the repository root.

```toml
[thresholds]
ram_percent = 65          # below this, RamWarden does nothing
critical_percent = 72     # above this, the Claude tier is allowed to run
inactivity_minutes = 45   # a tab must be idle this long to be a candidate
debounce_minutes = 120    # at most one automatic prompt every 2 hours

[server]
host = "127.0.0.1"
port = 7823

[processes]
watchlist = ["Discord", "burpsuite"]   # only these may ever be suspended
```

Only names on the `watchlist` can ever be suspended, and only when the activity
detector agrees they are idle. Anything RamWarden suspends can be woken again — from
the window's **Resume suspended** button, or:

```bash
curl -s -X POST localhost:7823/resume/Discord
```

---

## Browser extension

1. Open `brave://extensions` (or `chrome://extensions`) and enable Developer mode.
2. **Load unpacked** → select `~/ramwarden/extension`.
3. Firefox: `about:debugging#/runtime/this-firefox` → **Load Temporary Add-on** → pick `extension/manifest.json`.

Chrome and Brave connect over WebSocket; Firefox polls `POST /api/tabs` every 30s.
Multiple browsers can be connected at once — close commands are routed back to the
browser that owns each tab.

---

## Layout

```
daemon/
  main.py             FastAPI server, analysis flow, REST endpoints
  monitor.py          RAM polling loop and threshold/debounce logic
  activity.py         Live "what is the user actually using" detection + the suspend gate
  process_profiler.py Rolling CPU/RSS samples, feeds /ram-report categories
  analyzer.py         Tiered heuristic / Claude recommendation engine
  process_manager.py  SIGSTOP / SIGCONT with watchlist safety
  terminal_manager.py Idle-vs-busy shell detection
  workspace_manager.py Window→workspace sorting (X11/XWayland via Wnck)
  browser_windows.py  Private/incognito window detection
  history.py          SQLite log of what was closed
ui/window.py          The persistent RamWarden window
ui/prompt.py          Modal fallback when GTK cannot start a window
extension/            Browser extension (Chrome/Brave/Firefox)
tests/                pytest suite — run with `pytest`
```

## Tests

```bash
cd ~/ramwarden
pytest
```
