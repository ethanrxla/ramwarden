# RamWarden on Windows

## Contract
RamWarden on Windows keeps the Linux contract: measure memory the way the kernel
accounts for it, reclaim in order of cost to the user, report only what actually
happened, and undo everything reversible once pressure passes. The `:7823` API and
the browser extension contract are unchanged.

The target is **memory held by idle background applications and bloat**, not
the "% used" figure. On a 32 GB machine an idle 31% is not harmful in itself.
Standby cache is free memory, and RamWarden never purges it to make a number
look smaller.

Baseline measured on the target machine (32 GB, RTX 5070, idle desktop):

| Process | Working set |
|---|---|
| Brave (20 processes) | 2.4 GB |
| Discord | 0.95 GB |
| LM Studio | 0.83 GB |
| Epic + EOS overlay | 0.6 GB |
| ChatGPT | 0.5 GB |
| msedgewebview2 | 0.4 GB |

DiagTrack was running and set to Automatic.

## Mechanism mapping (`ramwarden-nt`)

| Linux (`ramwarden-kernel`) | Windows (`ramwarden-nt`) |
|---|---|
| PSS from `smaps_rollup` | Private working set (`PROCESS_MEMORY_COUNTERS_EX2.PrivateWorkingSetSize`) plus private commit. Shared pages are counted once, as Task Manager's default column does. |
| cgroup scope | `AppGroup`: a process tree rooted at its top-level executable (a browser and its renderers make one group) |
| `memory.reclaim` | `EmptyWorkingSet` on each group member. Pages move to the modified/standby lists and the compression store, so the operation is reversible. |
| `memory.high` soft cap | `SetProcessWorkingSetSizeEx` with `QUOTA_LIMITS_HARDWS_MAX_DISABLE` (a soft maximum) |
| `process_madvise(PAGEOUT)` | `ProcessMemoryPriority = LOW` (pages are evicted from standby first) plus EcoQoS |
| PSI `some`/`full avg10` | Pressure estimate (see below) |
| zram | Memory Compression store |
| pidfd | Process `HANDLE`, which stays bound to one process object for its lifetime |
| `SIGSTOP`/`SIGCONT` | `NtSuspendProcess`/`NtResumeProcess` |
| `SIGTERM`/`SIGKILL` | `WM_CLOSE` to top-level windows, then `TerminateProcess` |
| setcap helper over a Unix socket | `ramwarden-service` (LocalSystem) over a named pipe |

## Pressure estimate
Windows has no PSI. `ramwarden-nt::pressure` computes an estimate on the same
0–100 scale, so the ladder thresholds in `ramwarden.toml` keep their meaning.
Every reading carries `estimated: true`, and `/psi` reports it.

The estimate takes the largest of three terms:

- **Scarcity**, from available memory (free + standby) as a fraction of RAM.
  It is 0 at 20% available and rises linearly to 9.9 at 0%.
- **Commit**, from commit charge against the commit limit. It is 0 at 85%
  and rises to 9.9 at 98%.
- **Faulting**, from hard page faults per second, where 2000/s maps to 100.
  - This term is gated by scarcity: it counts not at all above 15% available,
    and in full at 5%.
  - Hard faults with RAM to spare are file I/O, not memory stall, which is why
    the gate exists.

The other parts of the reading:

- **The 9.9 cap.** Scarcity and commit are capped at 9.9, just below the
  default tab threshold of 10. Being low on memory can reach reclaim and
  page-out, but never a visible action. Anything visible needs real faulting.
- **`full`** is the faulting term, ramped in as available memory falls from
  8% to 5%.
- **Averaging.** Both values are averaged exponentially over 10, 60 and 300
  seconds, starting from the first reading.
- **Wakeup.** The low-memory resource notification
  (`CreateMemoryResourceNotification`) wakes the monitor. The notification is
  level-triggered, so wakeups are spaced at least 1 s apart. Without it, the
  monitor keeps adaptive polling: 10 s quiet, 2 s watchful, 1 s acting.

On the target machine (32 GB), these default thresholds work out as:

| Rung | Triggered when |
|---|---|
| reclaim | under about 16% available (about 5 GB) |
| page-out | under about 10% available |
| tabs and above | only with sustained hard faulting while memory is scarce |
| release | above 18% available |

## Policy

### Tidy rung (runs below the ladder, on every tick)
Tidy runs at any pressure and costs only soft faults. It is implemented in
`ramwarden-core::tidy`, so the policy is the same on every OS. Only the
background hint is Windows-only so far.

**Who is a candidate.** A process qualifies only when all of these hold:
- It is not structurally protected. Shell, system, games, anti-cheat, VMs,
  agents, builds, installers and sync are never tidied.
- It is quiet: no focus, no audio, under 0.5 CPU-seconds per sample, no busy
  child, and at least 10 minutes old.
- It is not suspended.
- Its proportional size is at least 64 MB.

A process that only *serves* a socket can still be tidied. Discord's RPC
clients do not need its idle pages resident. Serving still protects against
suspend and kill.

**What Tidy does.** Once a candidate has been quiet for `after_minutes`
(default 10):
1. Set the background hint: `ProcessMemoryPriority = LOW` plus EcoQoS.
2. Trim its working set **once per quiet period**.

**Undo.** Activity (focus, audio or CPU) ends the quiet period and clears the
hint. If the process goes quiet again, a new period starts and it can be
trimmed once more.

**Records.** Every change is recorded as a ladder step and in history. Trims
report bytes measured as private working set before minus after. `/tidy`
lists what is being tracked.

**Shutdown.** On a clean shutdown (Ctrl-C, console close, logoff or system
shutdown) every hint is restored. A daemon killed outright cannot do this;
`cargo run -p ramwarden-nt --example hint -- off <pid>...` restores by hand.

**Configuration** (`[tidy]` in `ramwarden.toml`):
- `enabled` (default `true`)
- `after_minutes` (default `10`)
- `background_hints` (default `true`)
- `trim` (default `true`)

**Measured on the target machine** (one-minute setting, idle Discord and
Brave): Discord's working set fell from 799 MB to 428 MB, Brave's from
1,546 MB to 1,214 MB. A Brave renderer that became active again was restored
to normal priority on the next tick.

### Ladder
The ladder keeps the Linux thresholds, hysteresis and cumulative escalation:

| Rung | Windows action |
|---|---|
| reclaim | Trim the working set of idle groups, largest first |
| page-out | Soft working-set caps on idle groups, released when pressure passes |
| tabs | Unchanged: the extension plus `tabpolicy` |
| suspend | `NtSuspendProcess` on idle **watchlisted** apps |
| kill | `WM_CLOSE`, the grace countdown, then `TerminateProcess` |

Only processes RamWarden suspended are resumed. A process suspended by a
debugger or by the user is never in the registry.

### Structural protection
The following are never acted on:
- `System`, `Registry`, `smss`, `csrss`, `wininit`, `winlogon`, `services`,
  `lsass`, `dwm`, `fontdrvhost`, `explorer`
- `MsMpEng`, `NisSrv`
- games and anti-cheat (role `GAME`): names ending `-Shipping` (Unreal
  titles, including Fortnite and its EOS overlay), a list of common titles,
  `vgc`, EasyAntiCheat, BattlEye and FACEIT
- `vmmem*`
- `WindowsTerminal`, `conhost`, `OpenConsole`
- any session 0 process
- any process the user cannot open

### The daemon's API is not reachable from web pages
- The daemon binds `127.0.0.1` by default. A non-loopback bind logs a loud
  warning, because the API has no authentication.
- Every request passes `guard` before routing:
  - A request carrying an `Origin` must come from a browser extension
    (`chrome-extension://`, `moz-extension://`) or the daemon's own origin.
    CORS alone is not enough here: a cross-site "simple" POST is still sent.
  - On a loopback bind, `Host` must be a loopback name. This blocks DNS
    rebinding.

### `ramwarden-service`
- Runs as LocalSystem and listens on `\\.\pipe\ramwarden`.
- **Access:** the pipe DACL (`D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;IU)`)
  admits only SYSTEM, Administrators and interactive users, and remote
  clients are rejected.
- **Requests** are one JSON line per connection, at most 16 KB. The vocabulary
  is closed, and anything else does not parse:
  - `ping`
  - `purge_low_priority_standby`
  - `trim {pid}`
  - `tweak_apply {id}`, `tweak_revert {id}` and `tweak_status`
- **`trim` refuses:**
  - PIDs ≤ 4
  - session 0
  - a process outside the client's own session (resolved from the pipe's
    client PID)
  - any structural role
- **Tweaks:** the id must be in a compiled-in profile, and only
  machine-scope tweaks are accepted.
- **`install` refuses a binary outside `%ProgramFiles%`.** A user-writable
  SYSTEM binary is a privilege escalation.
- **At start, the service takes ownership of `%ProgramData%\RamWarden`** and
  applies an admin-only DACL, with users read-only. The machine journal it
  restores from as SYSTEM therefore cannot be edited by the user.
- **Without the service** the daemon uses only user-level levers, and
  per-user tweaks still work. This is a normal configuration.

### Debloat (`ramwarden-debloat`)
- **The `Tweak` trait.** Each tweak (`detect`, `snapshot`, `apply`, `revert`)
  is one of:
  - a registry value
  - a service start type (only ever disabled or manual)
  - a scheduled task, through the Task Scheduler COM API
  - a startup entry, through `StartupApproved` exactly as Task Manager does it
- **Snapshot before change.** The engine writes the snapshot to a SQLite
  journal *before* applying.
  - It never records a setting that was already in place, so revert cannot
    undo a choice the user made themselves.
  - Revert is byte-exact, including "this value or key did not exist".
- **Drift:** a tweak recorded as applied that Windows has put back. Applying
  it again keeps the original snapshot.
- **Scope decides who applies a tweak.**
  - User tweaks (HKCU, startup entries) run in the caller's process: SYSTEM's
    `HKCU` is not the user's hive.
  - Machine tweaks go through the service, unless the caller is already
    elevated.
- **Profiles** are compiled-in TOML (`crates/ramwarden-debloat/profiles`):
  - **`privacy`:**
    - DiagTrack and dmwappushservice disabled
    - `AllowTelemetry=1`
    - Compatibility Appraiser and CEIP tasks disabled
    - advertising ID, tailored experiences and activity history off
    - Bing in Start off
    - Copilot, Recall and Widgets policies off
    - ContentDeliveryManager suggestions off
  - **`background`:**
    - Edge startup boost and background mode off
    - Epic and Riot Client start-at-sign-in off
    - LM Studio, Discord and OneDrive start-at-sign-in off (optional, only
      when asked)
    - `lfsvc` and `MapsBroker` set to manual
- **A forbidden list is checked when profiles load.** A profile naming any of
  these fails to build:
  - Defender, the firewall, Windows Update, BITS, SysMain, WSearch
  - anti-cheat services, and Vanguard's startup entry
  - `...\Services` and Winlogon keys
- **Surfaces:** CLI `ramwarden-debloat status|apply|revert`, `/debloat` routes,
  and the window's "Privacy & startup" panel.

### GPU (RTX 5070)
- **VRAM:**
  - Device totals come from NVML.
  - Per-process VRAM comes from PDH `GPU Process Memory(*)\Dedicated Usage`
    (`ramwarden-nt::gpu`). NVML reports per-process figures as unavailable
    under WDDM.
  - Measured live: Fortnite 723 MB, dwm 667 MB, LM Studio 169 MB.
- **Model unloading:** `ai::models` lists and unloads loaded models in Ollama
  (`/api/ps`, `keep_alive: 0`) and LM Studio (the `lms` CLI, since its API
  server is off by default).
  - **Policy:** when a game is running and VRAM is at least
    `[gpu] vram_percent` (default 85) used, idle local models are unloaded,
    with a 5-minute cooldown.
  - This is the only VRAM lever pulled automatically.
- **Embeddings:** `ml::embed` runs mxbai-embed-large through candle, in-process.
  - **Attached only when a CUDA device exists** (f32, about 1.3 GB VRAM: candle's f16 and bf16 BERT returned NaN on the RTX 5070). On
    the CPU it would hold about 1.3 GB of RAM indefinitely.
  - It loads on first use and is dropped after 10 idle minutes.
  - Ollama is the fallback.
- **Batch scoring:** the predictor scores every process in one batched forward
  pass on candle's device: CUDA with the `cuda` feature, otherwise the CPU.
  - A hand-written NVRTC kernel was planned. It was dropped as redundant,
    because candle already runs the same matmul and sigmoid on the GPU.
- **Reuse predictor (JEPA-style)**, `ramwarden-ml::jepa`:
  - **Model:**
    - A context encoder reads a window of four behaviour samples.
    - An EMA target encoder (stop-gradient) encodes the window one step later.
    - A predictor regresses that embedding.
    - A linear probe gives P(the user returns within the horizon).
  - **Features** are behaviour only (size, CPU, focus, audio, sockets, time of
    day, pressure), plus a hashed app identity, so per-app routines are
    learnable.
  - **Training:**
    - Time-ordered splits: the newest 20% is held out for the final test, and
      the newest 10% of the rest is used for early stopping. Without early
      stopping, full-batch training collapsed to chance at 1000 epochs.
    - The daemon retrains it daily in the background.
  - **The gate:** it is enabled only with at least 300 training examples
    *and* a held-out AUC 0.02 above the LRU baseline.
  - **On a synthetic daily-routine benchmark:** AUC 0.88 against LRU 0.61.
    Without app identity it reached only 0.69.
  - **Effect when enabled:**
    - Suspend candidates are ordered by size × (1 − P(return)).
    - Tidy defers the trim of anything with P(return) ≥ 0.7.
    - It never makes anything eligible.
  - **Routes:** `/predict/eval` reports the metrics; `POST /predict/train`
    trains now.

### The GPU worker (`ramwarden-gpu`)
- **The daemon never loads CUDA.** Built with `--features cuda`, the daemon
  held 640 MB private, because cuBLAS and the CUDA context load at startup
  and never leave. The CPU daemon holds 23 MB.
- **Training:** the daemon starts `ramwarden-gpu train` once a day. It trains
  on the GPU (about 0.9 s), saves the model and exits.
- **Embeddings:** `ramwarden-gpu embed` speaks JSON lines on stdin/stdout. The
  daemon starts it on the first relevance query and keeps it for follow-ups,
  and the worker exits after 10 idle minutes.
  - Measured on the GPU: 0.5 s including model load; related 0.718 against
    unrelated 0.262.
- **Scoring stays on the CPU.** A few hundred windows take microseconds,
  which is not worth a resident CUDA context.
- **When it is used:** only if the worker is installed beside the daemon and
  NVML reports an NVIDIA GPU. NVML creates no CUDA context.

### Window and tray (`ramwarden-tray`)
- **Opening the app:** a Start-menu entry, or the tray icon. Launching the app
  while the tray runs opens the window; it never starts a second tray.
- **The daemon's address** (`http://127.0.0.1:7823/`) shows a page saying it
  is the background service, and where the app is.
- **Panels:** Overview, Processes (right-click actions through `menu_for`,
  with confirmation for destructive ones), Privacy & startup, and GPU.
- **Two processes**, because the UI must not cost memory while closed:
  - The **tray host** never loads a graphics driver: a tray icon, a Win32
    message pump and a `/state` poll for the live memory gauge. It uses
    13 MB working set and 2 MB private.
  - **Open** launches `ramwarden-tray --window` (egui, glow renderer, about
    80 MB while open). Closing the window ends that process.
  - Measured, the first single-process version held 433 MB private and
    253 MB of VRAM while hidden.

## Verification
- **Unit tests:** `cargo test --workspace` on Windows, and on Linux in CI.
  `FakePlatform` tests cover:
  - Tidy eligibility and undo
  - ladder escalation and hysteresis on the pressure estimate
  - protection of every name in the structural list
  - suspend registry membership
- **Mechanism:** `cargo run -p ramwarden-nt --example probe`.
  - Private working set agrees with Task Manager's "Memory (active private
    working set)" to within 5%.
  - The pressure estimate rises during a synthetic allocation stress test and
    falls afterwards.
  - Suspend, resume and trim are tested only on a child process the test
    spawns.
- **Live:** with Brave, Discord and LM Studio idle, record in-use memory and
  commit. Let Tidy run for 10 minutes, then record again.
  - The apps still respond.
  - `/actions` lists only unprotected targets.
- **Debloat:** apply a profile, `detect` reports it applied, revert, and the
  state diff against the snapshot is empty.
- **GPU:**
  - Per-process VRAM matches Task Manager's dedicated GPU memory column.
  - The real mxbai model ranks a related passage above an unrelated one. This
    is an opt-in test, because it downloads about 1.3 GB.
  - With `--features cuda`, the predictor's tests pass on the GPU device.
    This needs the CUDA toolkit.
- **Predictor:** on the routine benchmark the model beats LRU by more than the
  gate's margin. With too little data the gate stays shut. Scores survive a
  save and load.
- **Security:**
  - A web origin POSTing `/kill` gets 403, while an extension and a local
    client are served.
  - Over the real pipe, an unknown op, an out-of-catalog tweak and explorer
    are all refused.

## Basis and limitations
- `EmptyWorkingSet` moves pages to the standby or modified lists. They fault
  back cheaply, and pages that are touched immediately return, so the reported
  bytes are measured after the call rather than assumed.
- Memory priority and EcoQoS are hints to the memory manager and the scheduler.
- `NtSuspendProcess` and `NtSetSystemInformation` are undocumented but stable
  ntdll exports, the same calls Process Explorer and RAMMap use.
- No kernel driver. Every lever needed is reachable from user mode or a SYSTEM
  service, and a test-signed driver would break Vanguard and EAC.
- AllowTelemetry on Pro cannot go below 1 (Required diagnostic data).
