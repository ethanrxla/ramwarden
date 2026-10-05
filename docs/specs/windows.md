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
Windows has no PSI. The daemon computes a 0–100 estimate on the same scale, so the
ladder thresholds in `ramwarden.toml` keep their meaning:

- `some_avg10` is the 10-second exponential average of
  `max(fault_term, standby_term, commit_term)`:
  - `fault_term` = hard page faults per second (PDH `\Memory\Pages Input/sec`),
    scaled so that 0 faults/s gives 0 and 1000 faults/s gives 100.
  - `standby_term` rises as available memory drops below 10% of physical RAM.
  - `commit_term` rises as commit charge goes past 90% of the commit limit.
- `full_avg10` is the same average restricted to `fault_term` while available
  memory is under 5%, approximating "everything is waiting".
- The low-memory resource notification (`CreateMemoryResourceNotification`)
  wakes the monitor immediately. Without it, the daemon keeps the existing
  adaptive polling of 10 s / 2 s / 1 s.

This is an estimate, labelled as one in `/psi`. It is not stall time.

## Policy

### Tidy rung (Windows rung 0)
Tidy runs at any pressure and costs only soft faults.

1. A process is a Tidy candidate when **all** of these are true:
   - it is not structurally protected
   - it is in the interactive session
   - it is not focused
   - it has no audio session
   - it has been idle (the detector's verdict) for `tidy_after_minutes`
     (default 10)
2. For each candidate:
   - set `ProcessMemoryPriority = LOW` and EcoQoS
   - trim its working set **once per idle period**, never every tick
3. When a candidate becomes active again (focus, audio or CPU), clear its
   memory priority and EcoQoS.
4. Every Tidy action is recorded in `/actions`, with the bytes actually trimmed
   (working set before minus after).

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
- `vgc`, `vgk`, EasyAntiCheat
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
