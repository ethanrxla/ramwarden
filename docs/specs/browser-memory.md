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
