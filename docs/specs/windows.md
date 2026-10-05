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

### `ramwarden-service`
- Runs as LocalSystem and listens on `\\.\pipe\ramwarden`.
- **Access:** the pipe DACL grants only SYSTEM and the interactive user's SID.
  Each connection is checked by resolving the client PID to its token user,
  and is refused if that user does not match.
- **Requests** are JSON lines. The vocabulary is closed:
  - `purge_low_priority_standby`
  - `trim {pid}`
  - `memory_priority {pid, level}`
  - `tweak_apply {id}` and `tweak_revert {id}`
- It refuses PIDs ≤ 4, protected processes, and any tweak id that is not in a
  shipped profile.
- **Without the service** the daemon uses only user-level levers. This is a
  normal configuration.

### Debloat (`ramwarden-debloat`)
- Each tweak implements `detect` / `apply` / `revert`. `apply` stores a
  `Snapshot` of the prior state in SQLite **before** writing, and `revert`
  restores it exactly.
- Profiles are TOML:
  - **`privacy`:**
    - DiagTrack and dmwappushservice disabled
    - `AllowTelemetry=1`
    - CEIP and Compatibility Appraiser tasks disabled
    - advertising ID, tailored experiences and activity history off
    - Bing in Start off
    - Copilot, Recall and Widgets policies off
    - ContentDeliveryManager suggestions off
  - **`background`:**
    - Edge startup boost and background mode off
    - launcher autostart (Epic, Riot Client) off through `StartupApproved`
    - OneDrive and Discord autostart off (opt-in)
    - `lfsvc` and `MapsBroker` set to manual
- **Never touched:** Defender, Windows Update, SysMain, WSearch, anti-cheat
  services, drivers. No Appx removal.
- `detect` reports **drift**: tweaks that a Windows update has reverted.

### GPU (RTX 5070, CUDA 12.8+)
- **VRAM:**
  - Device totals come from NVML.
  - Per-process VRAM comes from PDH `GPU Process Memory(*)\Dedicated Usage`,
    because NVML per-process figures are unavailable under WDDM.
  - **Lever:** unload idle local models through the LM Studio API
    (`127.0.0.1:1234`) or Ollama (`keep_alive: 0`).
- **Embeddings:** `CandleEmbedder` runs mxbai-embed-large in fp16 on CUDA, inside
  the daemon. It is used only when there is room on the GPU
  (`vram::Gpu::has_room_for`). The Ollama embedder is the fallback.
- **Batch scoring:** a CUDA kernel compiled at runtime with NVRTC scores the
  process × signal matrix. A CPU implementation produces identical results and
  is the default path.
- **Predictive ranker (JEPA-style):**
  - It learns from the `/signals` log to predict each process's or tab's next
    activity embedding, and from that the probability of reuse within T minutes.
  - It can only **reorder** candidates. It never makes anything eligible.
  - It is disabled until its held-out AUC beats the LRU baseline
    (`/predict/eval`).

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
  - CUDA embeddings rank the same as Ollama, with cosine within 1e-3.
  - CUDA scoring is bit-identical to the CPU path.
  - Per-process VRAM matches Task Manager's dedicated GPU memory column.

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
