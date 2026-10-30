# RamWarden maintenance rubric

Every change to RamWarden is checked against this rubric before it merges.
Reviewers (human or Claude) go through it section by section; the PR
template links each item. A PR that fails a **must** item does not merge.

## 1. Platform parity (must)

RamWarden ships on macOS, Windows and Linux. A feature is not done until it
exists on all three.

- Every user-visible feature has a macOS, Windows and Linux implementation,
  or the PR says which platforms are missing and links an issue for each.
- Platform code lives behind the `ramwarden-platform` trait layer:
  `ramwarden-kernel` (Linux), `ramwarden-nt` (Windows), `ramwarden-mac`
  (macOS). Shared crates (`core`, `daemon`, `ai`, `ml`) never call an OS API
  directly.
- Platform code is gated with `#[cfg(target_os = "...")]` or
  `[target.'cfg(...)'.dependencies]`, never with Cargo features or runtime
  `if` checks on the OS name.
- New platform behaviour gets a fake in `ramwarden-platform::fake` so the
  shared logic is testable on any OS.
- CI builds and tests the change on `ubuntu-latest`, `windows-latest` and
  `macos-latest`.

## 2. Safety of what we close (must)

RamWarden ends processes and closes tabs on a user's machine. Getting this
wrong loses work.

- Nothing is killed or closed that is on the protected list: system
  processes, anti-cheat and games, password managers, auth/payment/banking
  pages, anything with unsaved state, and anything the user pinned.
- The ladder is always: trim → suspend/freeze → graceful quit → kill. A PR
  that skips a rung explains why.
- Jev decides borderline cases; its thresholds live in config, not in code.
  When Jev is unreachable RamWarden falls back to the more conservative
  choice (keep, not kill).
- Every close is logged with the reason, the score and who decided (rule,
  LRU, Jev), and dry-run mode shows the same lines without acting.
- Never open a handle to a process RamWarden has no business touching
  (games, anti-cheat, other users' processes).

## 3. Rust code standards (must)

- Rust, edition 2024. No new languages in the core; the Swift menu-bar app
  and the browser extension are the only exceptions.
- `cargo fmt --all` is clean.
- `cargo clippy --workspace --all-targets -- -D warnings` is clean.
- Errors: `thiserror` in library crates, `anyhow` only in binaries and tests.
  No `unwrap()`/`expect()` outside tests unless the invariant is written in
  the message.
- `unsafe` only in the platform crates, each block with a `// SAFETY:`
  comment saying why it holds.
- No blocking calls on the monitor loop or a tokio worker: slow work (tab
  unloads, Jev calls, process enumeration on Windows) is spawned with a
  timeout. Every external call has a timeout.
- Data structures are chosen on purpose: LRU tracking is a hash map plus an
  intrusive doubly linked list, work queues are `VecDeque`, undo/restore is
  a stack. Comment the complexity when it is not obvious.
- Secrets (Jev, NVIDIA, MongoDB) come from the environment, `.env` or
  `secrets.toml`, never from the repo. `.env` and `secrets.toml` stay in
  `.gitignore`.

## 4. Tests (must)

- `cargo test --workspace` passes on all three CI runners.
- New decision logic (scoring, LRU order, Jev parsing, protection rules) has
  unit tests, including the "must not close" cases.
- Bug fixes come with a test that fails before the fix.
- Tests do not call the network; Jev and MongoDB are faked.

## 5. Review checklist (should)

- The PR does one thing and its title says what changes for the user.
- Behaviour changes are reflected in `README.md` and `ramwarden.toml`.
- Memory or CPU cost of RamWarden itself is not worse (the daemon targets
  under 30 MB RSS and under 1% CPU at idle).
- The browser extension and the daemon agree on protocol changes; both sides
  ship in the same PR.
- Logs are useful and not noisy: one line per decision, refusals logged once
  per change.

## 6. Branches and merging

See [BRANCHES.md](BRANCHES.md). In short: feature branches open a PR, CI is
green on all three platforms, the rubric is ticked, then it merges.
Never force-push a shared branch.
