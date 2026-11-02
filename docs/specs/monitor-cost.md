# What a monitor tick costs

## Contract
A tick refreshes, for every process, only what a decision needs fresh: CPU
time, resident size, state, parent, focus, windows and audio. Everything that
cannot change between ticks, or changes slowly, is cached per process instance
and re-read on a bounded schedule. The ladder's candidate selection and every
judge request stay bounded by the number of candidates, not the number of
processes. No lock that the HTTP API waits on is held across a network call.

## Policy
- **Process identity is PID plus start time** (`starttime` on Linux, creation
  FILETIME on Windows, `pbi_start` on macOS), carried as `ProcessSample.started`.
  Nothing cached for a process is ever handed to another process that reused
  its PID: a different start time is a new instance, read and classified afresh,
  and its CPU is not differenced against the dead one's.
- **Cached per instance** (`ramwarden_platform::cache::Instances`):
  - command line and owner: read on the first two sightings (the second catches
    Chromium's post-fork `argv` rewrite), after an `exec` (comm changed), and
    every 60 s on Linux; once per instance on Windows and macOS.
  - PSS (Linux, processes over 24 MB resident): re-measured when resident size
    moves by more than 1/16 of what it was at the last measurement, and at least
    every 20 s, since sharing changes without resident size changing.
  - role, application and part (the detector's classification of the command
    line): recomputed only when name, owner or command line changed.
- **Whole-system tables on a cadence** (`ramwarden_platform::cache::Cadence`):
  the socket table (and the cgroup scope map on Linux) at least every 10 s
  (20 s on macOS, where it means two `lsof` runs), and no sooner than 3 s (5 s)
  after a new process appears. A newly started server is spared for its first
  ten minutes anyway (`FRESH_MINUTES`), so a socket seen a few seconds late
  cannot expose it.
- **Candidates**: `reclaimable` scans the process table once per watchlist
  entry. Eligible names come from walking the LRU from its cold end, stopping
  at the judge's limit (eight); only names the LRU has not seen are ranked.
  Without a judge every eligible name is still acted on, as before.
- **Judging**: the monitor asks `Ladder::judge_requests` under the locks,
  releases them, asks Jev, and runs the step with a `Prejudged` judge. The step
  re-derives its candidates under the locks: a name no longer eligible is not
  acted on whatever the answer; a name newly eligible and not judged gets no
  verdict and is spared until the next tick. A Jev failure replays as the same
  failure, so kills stay blocked and suspends fall back to the gate.
- **Sampling** reads the process table outside the detector's write lock and
  takes the lock only to score the result.

## Verification
- `cargo run --release -p ramwarden-core --example tick_cost` prints time,
  allocations and read syscalls per tick for a synthetic 400-process `/proc`
  (Linux) and for a judged kill-rung ladder step. On the build machine:

  | | before | after |
  |---|---|---|
  | process sample, 400 processes | 15.4 ms, 81,223 allocs, 2,524 reads | 1.2 ms, 2,058 allocs, 802 reads |
  | socket table read (now every 10 s, was every tick) | 12.5 ms, 66,456 allocs | unchanged, amortised |
  | ladder step, kill rung, judged | 17.5 ms, 878,956 allocs | 0.22 ms, 358 allocs |

- Tests: `platform::cache` (a recycled PID never inherits an entry; sweep;
  cadence), `kernel::sampler` (per-instance command line, re-read after exec,
  recycled PID, PSS drift), `detector` (a recycled PID is reclassified; reused
  entries carry no per-tick fact over), `pattern` (the ASCII fast path agrees
  with the general matcher), `judge` and `ladder` (judging ahead of the step
  decides exactly what judging inside it would; unasked intents approve nothing).

## Basis and limitations
- Synthetic procfs files read faster than real ones; a real `smaps_rollup` makes
  the kernel walk every mapping, so real savings are larger than the table.
- A listening socket opened by a long-running process is seen up to 10 s late.
  It only matters for a watchlisted, otherwise idle process, and serving is soft
  protection that a watchlist entry already overrides.
- Allocation count is not resident memory; the allocator may keep freed pages.
  The point is less allocator CPU per tick and a smaller peak.
