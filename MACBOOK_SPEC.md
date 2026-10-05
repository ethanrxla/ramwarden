# MacBook spec & macOS port notes

This branch (`macbook`) adapts RamWarden from Linux to the machine below. `main` stays Linux-only.

## Hardware (measured 2026-10-05)

| | |
|---|---|
| Model | MacBook Air, `Mac15,12` |
| Chip | Apple M3 — 8 cores (4 performance + 4 efficiency) |
| Memory | 16 GB unified (`hw.memsize` = 17179869184) |
| Storage | 460 GB SSD (APFS), ~120 GB free at time of writing |
| OS | macOS 26.5.2 (Darwin 25.5.0), arm64 |
| Toolchain | Rust via rustup (stable, `aarch64-apple-darwin`), Python 3.13.5 |
| Cooling | Fanless — sustained load throttles; keep the daemon cheap (adaptive polling, no busy loops) |

## How the memory model differs

| Linux (main) | macOS (this branch) |
|---|---|
| PSI `some`/`full` stall % (`/proc/pressure/memory`) | No PSI. Proxy: `kern.memorystatus_vm_pressure_level` (1 normal / 2 warn / 4 critical) and `kern.memorystatus_level` (% memory available), mapped onto the same `some`/`full` scale so the ladder thresholds still work |
| PSS via `smaps_rollup` | `proc_pid_rusage` → `ri_phys_footprint` (what Activity Monitor shows as "Memory"); RSS is `ri_resident_size`. Footprint already accounts for compressed pages, and plays the role of PSS |
| zram + cgroup `memory.reclaim` | The kernel's memory compressor + swap files; there is **no** userspace reclaim lever. Reclaim / page-out rungs are no-ops |
| `process_madvise`, pidfd | Not available. Signals go through `kill(2)`; identity is guarded by re-checking process start time before every signal |
| `/proc` tree behind `Root` | libproc (`proc_listallpids`, `proc_pidinfo`, `proc_pidpath`) and `sysctl` |
| systemd user units | launchd agent (`packaging/macos/`) |
| GTK4/libadwaita window | Not built; use the HTTP API (`localhost:7823`) and the browser extension |

## What is active on macOS

1. Pressure monitoring (proxy PSI) driving the ladder
2. Per-process footprint accounting
3. Browser tab unloading through the extension (the highest-value rung on 16 GB)
4. SIGSTOP / SIGTERM / SIGKILL rungs — **inert by default**: the shipped macOS config has an empty `watchlist`, which is the ceiling for any signal, so only tab unloading acts until you opt apps in
5. Kill rung additionally needs available memory < 400 MB (the pressure proxy never reaches the `full` threshold)

## Never touch (macOS protected set)

`kernel_task`, `launchd`, `WindowServer`, `loginwindow`, `Finder`, `Dock`, `SystemUIServer`, `coreaudiod`,
`bluetoothd`, `Xcode`, Terminal / iTerm, and anything owned by another user.

## Tuning for 16 GB

Swap on Apple silicon is fast, so the machine tolerates being "full". Act on **pressure level**, not percent used.
Defaults live in `packaging/macos/config.toml` (installed to `~/.config/ramwarden/config.toml`); the API binds `127.0.0.1` only.
