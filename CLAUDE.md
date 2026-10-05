# CLAUDE.md

RamWarden is a desktop memory manager written in Rust. It measures memory the way
the kernel accounts it and reclaims memory in order of how little it costs the
user: kernel reclaim first, signals last. See `README.md` for the Linux design
and `docs/specs/` for behavioural contracts.

## Branches

| Branch | Target | Mechanism crate |
|---|---|---|
| `main` | Linux (cgroup v2, PSI, smaps, zram) | `ramwarden-kernel` |
| `windows` | Windows 11 (NT native APIs, Memory Compression) | `ramwarden-nt` |

Policy code (`ramwarden-core`) is meant to be identical on both branches. A fix
to policy belongs on `main` and is merged into `windows`. Do not fork it.

## Crate map and layering

```
ramwarden-platform   traits + value types, no OS code            (the seam)
ramwarden-kernel     Linux mechanism   — implements platform traits
ramwarden-nt         Windows mechanism — implements platform traits
ramwarden-core       policy: detector, ladder, actuator, history
ramwarden-ai         embeddings, reranking, VRAM, GPU scoring, predictor
ramwarden-daemon     :7823 HTTP/WS API and the loop that drives the ladder
ramwarden-helper     Linux setcap page-out shim
ramwarden-service    Windows SYSTEM service (privileged ops over a named pipe)
ramwarden-debloat    reversible Windows tweaks and profiles
ramwarden-ui         client + model shared; GTK view (Linux), egui + tray (Windows)
```

The rules:

- **Mechanism crates hold no policy.** `ramwarden-kernel` and `ramwarden-nt`
  measure and actuate. They never decide *whether* to act.
- **Policy talks to the OS only through `ramwarden-platform` traits.** If core
  needs a new OS fact, add it to a trait and implement it on both platforms (or
  return `Unsupported`). Never put `cfg(windows)` inside policy logic.
- **Dependencies point downward.** core may depend on platform. platform depends
  on nothing in the workspace. ai, daemon and ui depend on core, never the
  other way round.
- **OS-specific dependencies are target-gated** in `Cargo.toml`
  (`[target.'cfg(windows)'.dependencies]`), so the workspace builds on both.

## Code conventions

- **Object-oriented and modular.**
  - Capabilities are traits: `PressureSource`, `Reclaimer`, `ProcessControl`,
    `Tweak`, `Embedder`.
  - OS resources are RAII handle types that release themselves in `Drop`: a
    process `HANDLE`, a pidfd, a pipe, a registry key.
  - State lives in structs with methods. New code is a method on the type that
    owns the data, not a loose free function.
  - One concern per module. If a file passes ~600 lines, look for the second
    concern hiding in it.
- **Check before you write.** Before adding a type, helper or module:
  1. Search the workspace (`rg`) for an existing one: `Outcome`, `History`,
     `Root`, `rerank::cosine`, `vram::Gpu`, `tabpolicy`, `config`.
  2. Extend or generalise what exists rather than adding a parallel version.
  3. If two platforms need the same logic, it belongs in core or platform, not
     in two copies.
  Redundant code is a bug: it drifts.
- **`Outcome` reports what happened, not what was attempted.** Zero bytes moved
  is `refused`, not success. This is load-bearing because the ladder acts
  unattended.
- **Handles, not PIDs.** Anything that may signal, suspend or kill holds a
  process handle (pidfd or `HANDLE`), so a recycled PID can never be hit.
- **Degrade, don't fail.** A missing helper or service, no GPU, no model, or no
  extension: each is a normal configuration with a named fallback.
- **Comments explain *why*.** Match the existing style: module docs carry the
  reasoning and the measurements behind a decision.

## Safety rules (absolute)

- **Structurally protected processes are never touched**, whatever the
  watchlist says or which button is clicked.
  - Linux: compositor, VMs, containers, terminals, agent sessions.
  - Windows: `csrss`, `smss`, `wininit`, `winlogon`, `lsass`, `services`, `dwm`,
    `explorer`, `MsMpEng`, `vgc`/`vgk` (Vanguard), EasyAntiCheat, `vmmem*`,
    terminals (`WindowsTerminal`, `conhost`, `OpenConsole`), and anything
    running in session 0.
- **Suspend and kill only ever target watchlisted names.** Activity detection
  decides what to spare. It never widens what may be touched.
- **Debloat always takes a snapshot before it changes anything**, and every
  tweak must be revertible from that snapshot.
  - Never touched: Defender, Windows Update, SysMain, WSearch, anti-cheat
    services, drivers. Appx packages are never removed.
- **Never weaken security** to free memory: no disabling Defender, VBS/HVCI or
  Secure Boot, and no test-signing.

## Build and test

Windows (MSVC toolchain):

```powershell
cargo build --workspace
cargo test  --workspace
cargo run -p ramwarden-nt --example probe          # compare with Task Manager
cargo run -p ramwarden-daemon                      # API on 127.0.0.1:7823
cargo run -p ramwarden-ui --features tray --bin ramwarden-tray   # tray; --window = the window process
cargo run -p ramwarden-debloat -- status                          # reversible profiles
cargo build --release -p ramwarden-daemon --features embed,cuda   # GPU predictor + embeddings; needs CUDA 12.8+ (sm_120)
powershell -ExecutionPolicy Bypass -File scripts\install.ps1      # elevated; installs everything
```

Linux:

```bash
cargo test --workspace
xvfb-run -a cargo test -p ramwarden-ui --features gui -- --test-threads=1
```

CI (`.github/workflows/ci.yml`) runs both operating systems. The Linux job is
how we prove Windows work did not change Linux behaviour, so keep it green.

Tests never act on real processes or call real models. Linux tests use a
synthetic `/proc` through `Root`. Windows policy tests use `FakePlatform`.
Mechanism tests that need the real OS act only on a child process they spawn
themselves.

## Specs

`docs/specs/*.md` define behaviour as Contract / Policy / Verification /
Basis and limitations. Change the spec first when behaviour changes. Windows
behaviour is in `docs/specs/windows.md`.
