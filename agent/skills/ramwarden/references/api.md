# Daemon API for agents

All routes are on `http://127.0.0.1:7823`, JSON in and out. Send no `Origin`
header. A route marked † needs a daemon with wardens (ThreatWarden); an older
daemon answers it with 404, which means "not available on this install", not an
error to retry.

`tests/agent_skill.rs` in `ramwarden-daemon` checks that every route in the
tables below exists in the daemon, so a renamed route fails CI instead of
silently breaking agents.

## Read (no confirmation needed)

| Route | What it tells you |
| --- | --- |
| `GET /health` | `{ok, version, warm, uptime_seconds}`. `warm: false` means it has not watched long enough for idle times to be trustworthy yet |
| `GET /psi` | Pressure: `some`/`full` stall averages (`avg10`, `avg60`, `avg300`, percent of time stalled), `available_mb`, `rung` (what level the ladder is at), `estimated` (true on Windows) |
| `GET /stats` | Used and total memory, the top 15 processes by PSS (the field is called `rss_mb` for old clients but holds PSS), connected browsers, terminals |
| `GET /apps` | Memory by app: in use, idle, protected, how long it has been quiet. The best answer to "what is taking my RAM" |
| `GET /lru` | Apps from least to most recently used, the order the ladder considers them in |
| `GET /ladder/plan` | What the ladder would do now: `rung`, scopes to `reclaim`, apps to `suspend` or `kill`, a `kill_pending` countdown, and the `undo` stack |
| `GET /suspended` | What RamWarden has frozen |
| `GET /watchlist` | The user's watchlist patterns and the processes they match. Suspend and kill only ever touch these |
| `GET /browser/tabs` | Tabs the browser extension reports, with memory and idle time per tab, and each tab's `browser`, `id` and `url` |
| `GET /actions` | What the ladder did unattended, newest first |
| `GET /ai` | Which decision tier is answering (local model, hosted Jev, heuristic) and what it can do |
| `GET /wardens` † | Every warden with `access`, `unlocked`, `status {level, headline, detail}`, and `paid` (ids the subscription unlocks) |
| `GET /wardens/{id}` † | One warden plus its report. `threat`: agent health and up to 50 recent alerts |

`status.level` is `clear`, `watch`, `alert`, `off` (unlocked but not set up or
not reachable; the headline says which) or `locked` (needs a subscription that includes it).

## Act (only after the user says yes to that action)

| Route | Effect | Undo |
| --- | --- | --- |
| `POST /reclaim/{scope}` | Asks the kernel to page out a scope's cold memory (Linux cgroup). Nothing closes | none needed |
| `POST /browser/discard` | Body `{"targets": [{"browser", "id", "url"}]}` from `/browser/tabs`. Unloads tabs; they stay in the tab strip and reload on click. Keep to the `batch_limit` that `/browser/tabs` reports | the user clicks the tab |
| `POST /browser/close` | Same body. Closes tabs. Only when the user asked to close them | browser's reopen-closed-tab |
| `POST /suspend/{name}` | Freezes a watchlisted app (SIGSTOP or NtSuspendProcess). 409 when refused | `POST /resume/{name}`, `POST /ladder/undo` |
| `POST /resume/{name}` | Thaws an app RamWarden froze | |
| `POST /ladder/undo` | Thaws whatever the ladder froze most recently | |
| `POST /ladder/cancel-kill` | Stops a pending kill countdown. Always safe; use it if the user objects | |
| `POST /kill/{name}` | Terminates a watchlisted app. Unsaved work is lost. Only when the user named the app and asked for it | none |
| `POST /hold/{secs}` | Pauses freezing for up to 600 s and thaws everything, e.g. while the user rearranges windows. `DELETE /hold` ends it early | |

`?force=true` on suspend and kill skips the watchlist and in-use checks, never
structural protection. Use it only when the user named that exact app.

Read the answer of every action: `freed_mb`, `suspended`, `resumed`,
`confirmed` / `queued` / `refused` and `notes`. `queued` tabs were asked to
unload but not confirmed yet; say so rather than counting them as freed.
