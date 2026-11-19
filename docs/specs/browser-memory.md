# Browser tab analysis and memory reclamation

## Contract
The GTK window exposes separate Processes and Browser tabs pages. Browser tabs
shows browser ownership, title/URL, idle age, status, priority, and the reason
for every recommendation. A rules-only analysis runs without a model. It states
when no extension is connected or the extension needs updating. Selection is
explicit; Unload selected keeps tabs open and reloadable.

## Rust policy
- Unknown safety metadata is ineligible (old extensions remain readable).
- Protect active, pinned, audible, incognito, loading, browser-opted-out tabs,
  internal/private-network pages and editors/login flows. Already discarded tabs
  cannot be reclaimed again.
- Use bounded LRU priority: idle age increases priority; exact duplicate URLs
  within a browser increase it further, preserving the most recently used copy.
  Ignore fragments only, never query strings. Ordinary pages require twice the
  configured inactivity threshold; duplicate and stale-media pages require once.
  Keep a minimum five-minute cooling period even with a zero config threshold.
- Batch at most five eligible tabs. Use (browser, tab ID, exact URL) throughout;
  tab IDs alone are ambiguous across browsers. Revalidate server-side and again
  in the extension immediately before discard, including URL and idle time.
- Unsupported or stale state fails closed. No closing tabs as a discard fallback.
- The automatic pressure ladder uses bounded discard in its existing tab rung,
  after cgroup reclaim and cold-page paging. Preserve pressure hysteresis and
  add a 30-second browser-batch cooldown. No model is required to save memory.
- A timed sweep (`[tabs] sweep_minutes`) runs the same bounded unload at any
  pressure, so a machine with spare RAM still releases idle tabs. It may also
  close tabs, the one exception to "keep tabs open": only rows that passed every
  unload guard or were already unloaded, not a work page, and one of: idle
  past the close floor with a return chance below `close_below_return` (at
  most 0.95); idle twice the floor, whatever the estimate; or an older copy of
  an open page idle `close_duplicates_after_hours`. At most
  `max_close_per_sweep` per sweep. Idle here is **active idle**
  (`closetune::ActivityClock`): the browsing done in other tabs since this
  one was last used, from the union of every tab's last 24 activations, with
  each gap counted up to 10 minutes, so a night or a meeting away adds at most
  that. With no activation history (old extension, polling) it is wall-clock
  idle. The floor is stretched per tab (`closetune::stretch`): visits
  `1 + 0.5 * min(frecency, 8)` (at most 5x), 2x on a site in use now, mail,
  calendar, chat and music 3x, reference 1.5x, feeds 0.5x, an older copy 0.5x;
  browser settings pages use the plain floor. The extension's wall-clock
  recheck uses the lowest bar the tab could have cleared, never under an hour;
  active idle is never more than wall-clock idle, so the two always agree.
  The return chance used for closing is the
  Jev-adjusted estimate on the user's own time scale
  (`tabrank::at_pace`): the class half-lives are fixed priors (a page: 12 h)
  that kept tabs left 6 to 10 h near 0.6, so the curve is rescaled until an
  unvisited page stands at 0.1 at the learned floor, matching the 1 in 10
  returns that come later. Visits, a related tab in use and work pages stretch
  it by the same factors as before; it is never stretched past the prior. `close` is `off`, `review`, `dry-run`
  (default: log only) or `live`. The extension rechecks each tab live with the
  same idle floor before closing, answers every close command, and saves closed
  URLs to a reopen list in the popup. A tab budget (`keep_tabs`, default 6)
  keeps the most recently used tabs per browser open; it never closes a tab on
  its own. A tab Chrome has unloaded reports status `unloaded`; it counts as
  settled like `complete`, so an unloaded tab can still be closed. Only a page
  that is actually loading is skipped.
- **The close floor is learned per user** (`ramwarden-core::tabpace`,
  `[tabs] learn_pace`, default on). Each release pass collects the user's
  return gaps from the last 14 days: how long they were away before selecting
  a tab again, measured from when they left it (the extension records each
  tab's departures beside its activations, so reading time does not count and
  closing another tab cannot change a gap; a return without a recorded
  departure adds nothing; gaps
  under 5 minutes are tab switching and skipped; visits older than 14 days are
  ignored) and how long after an unload they came back
  (`tab_outcomes`). The floor is the gap that covers 9 in 10 returns, times
  1.5 when more than 20% of the warden's own unattended closes were reopened, or 0.8 when
  under 5% of at least 10 were, held between 1 hour and 1 day. With fewer than
  20 returns seen, `close_after_hours` (default 4) is used. The floor only
  decides when a tab may be considered; every other guard (recent tabs, work
  pages, forms, keeps, focus list, return chance, Atlas recall) still applies.
  `GET /browser/pace` shows the floor in use and why.
- Protections other than the hard ones (non-web pages, private hosts, editors
  whose state lives in the tab) lift once a tab has been idle 4 hours
  (`tabpolicy::PROTECTION_RELEASE_MINUTES`): consoles, dashboards, login pages,
  school sites, open PRs and autosaving documents. The extension applies the
  same rule before it closes or unloads anything.
  The badge counts unloaded tabs, since the browser draws them unchanged, and
  the extension gathers unloaded tabs into one collapsed "Idle · N" group per
  window (never the user's own groups or pinned tabs); a tab opened from it
  leaves the group.
- Confirm only successful browser API results. Poll delivery remains explicitly
  queued, never labelled reclaimed. No per-tab bytes are estimated from RSS.

## Verification
Rust policy tests: hard guards, legacy metadata, thresholds, ranking, duplicate
keepers, ID collisions, bounded batches, URL mismatch. Hub tests: correlation,
wrong-browser replies, timeout/refusal. HTTP route tests: analysis and actual
command dispatch with mock browser; no real browser mutations. GTK fixture:
browser-page visibility, rendered recommendations, selection and action result.
Node fixture: fresh metadata, activation/navigation race, unsupported API,
partial failure, confirmed IDs only. Workspace tests, GUI tests under Xvfb,
release builds and extension bundles. Live checks only inspect tab state.

## Basis and limitations
Chrome tabs.discard keeps tabs visible and reloads them on activation:
https://developer.chrome.com/docs/extensions/reference/api/tabs
The existing cgroup memory.reclaim stage requests bounded cold-memory reclaim;
requested bytes are not guaranteed freed bytes:
https://www.kernel.org/doc/html/latest/admin-guide/cgroup-v2.html
URL protection and browser autoDiscardable are conservative signals, not a
universal unsaved-form detector. No content-script access or new host permissions
are added. Keep any important unrecognised form pinned or unselected.
