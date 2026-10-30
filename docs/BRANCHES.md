# Branches

> Layout agreed on 2026-10-30. `mac`, `windows` and `linux` are cut from
> `main` once the macos-sync reconcile (PR #3) merges.

| Branch    | Role |
|-----------|------|
| `main`    | Shared trunk. All code for all three platforms, gated by `cfg(target_os)`. Default branch. |
| `mac`     | macOS release line. Only takes merges from `main`. |
| `windows` | Windows release line. Only takes merges from `main`. |
| `linux`   | Linux release line. Only takes merges from `main`. |

Work happens on short-lived feature branches (`feat/...`, `fix/...`,
`claude/...`) that open a PR into `main`. A platform-only hotfix may go
straight to its platform branch, and is merged back into `main` in the same
day so the lines do not drift.

## Migrating the current branches

| Today | Becomes |
|-------|---------|
| `macos-sync` (Windows + macOS + Linux, 27 commits ahead of main) | reconciled with PR #3, then merged into `main`; `mac` is cut from it |
| `macbook` | old local Mac branch; every commit is already in `macos-sync`, to delete |
| `windows` | kept as the `windows` line after PR #2 merges; then its last 4 commits (watchdog, game handle guard) are merged into `main`; the one conflict is the tab-unload wait in `ramwarden-daemon/src/monitor.rs` |
| `rust-rewrite` | identical to `main`; to delete (owner does it in GitHub, Branches page) |
| PR #1 (systemd units) | merged into `main`, reaches `linux` |
| PR #2 (start the daemon) | merged into `windows`, then `main` |
| PR #3 (LRU + Jev) | reconciled with `macos-sync`, merged into `main` |
| merged `claude/*` branches | deleted after merge |
