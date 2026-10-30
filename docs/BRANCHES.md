# Branches

> Proposed layout, pending the owner's sign-off. Nothing has been renamed or
> deleted yet.

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
| `windows` | kept as the `windows` line after PR #2 merges; then reset to follow `main` |
| `rust-rewrite` | identical to `main`, deleted |
| PR #1 (systemd units) | merged into `main`, reaches `linux` |
| PR #2 (start the daemon) | merged into `windows`, then `main` |
| PR #3 (LRU + Jev) | reconciled with `macos-sync`, merged into `main` |
| merged `claude/*` branches | deleted after merge |
