# CLAUDE.md

RamWarden: memory manager daemon + browser extension. Rust workspace (edition 2024).
This is the `macbook` branch — macOS (Apple silicon) port. See `MACBOOK_SPEC.md` for the machine and model differences.

## Branches
- `main` — Linux (cgroup v2, PSI, zram, systemd, GTK4). Do not break it.
- `macbook` — macOS work. Linux code stays, gated with `#[cfg(target_os = "linux")]`; macOS code is `#[cfg(target_os = "macos")]`.

## Build & test
```bash
source ~/.cargo/env
cargo build --release -p ramwarden-daemon
cargo test --workspace --exclude ramwarden-ui --exclude ramwarden-helper   # UI is GTK/Linux-only; helper is Linux-only
cargo run -p ramwarden-kernel --example probe                              # live pressure + footprint readout
```
`ramwarden-ui` (GTK4) and `ramwarden-helper` (process_madvise) are not built on macOS.

## Crate map
- `ramwarden-kernel` — OS primitives, no policy. Public function signatures are shared across platforms; `macos.rs` backs them with libproc/sysctl/mach.
- `ramwarden-core` — ladder, detector, actuator (policy).
- `ramwarden-daemon` — monitor loop, axum HTTP/WS API on :7823, browser tab control.
- `extension/` — Chrome extension (tab discard).
- `packaging/macos/` — launchd agent; `scripts/install-macos.sh`.

## Rules
- Keep kernel-crate signatures identical across platforms so `core`/`daemon` need no `cfg`.
- Unavailable levers return `Error::Unsupported`; callers degrade, never panic.
- Tests must use fixtures / synthetic `Root`. Never SIGSTOP/SIGKILL real processes in tests.
- Destructive rungs (suspend, kill) are inert on macOS until apps are added to `[processes].watchlist` (shipped empty). There is no separate dry-run flag.
- Never read or commit API keys (`ANTHROPIC_API_KEY` comes from the environment).
- Commits end with: `Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>`. Do not push unless asked.
