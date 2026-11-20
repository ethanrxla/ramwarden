# Tab feedback and review

Automatic closes require fresh extension activity metadata, idle past the close floor (learned from the user's own return gaps between 1 hour and 1 day, `close_after_hours` until enough are seen; see browser-memory.md; the daemon sends the same floor to the extension's safe close), a return estimate (on the user's pace) below `close_below_return`, or idle twice the floor, and all live safety guards. A tab-count target or same-site page cannot bypass this floor. Exact URLs, including query and fragment, define duplicates.

The updated extension reports timestamps and an unsaved-input bit, never typed text. Reopened and explicitly kept pages receive a 24-hour protection, without assuming browser removal was an intentional close. Local SQLite feedback and outcomes expire after 30 days. Negative feedback can only raise the return estimate; this is not external model retraining.

GET /browser/review offers at most ten uncertain tabs, at most once per 30 minutes. In review mode, where nothing closes unattended, the offer also carries what the sweep would have closed, including unloaded work pages; a loaded work page is never offered. POST /browser/review/respond accepts a token, close/keep/later, and browser/id/url targets. Tokens expire after ten minutes and are single-use. Close rechecks current metadata and uses a correlated, URL-scoped close_safe extension command. Older extensions cannot execute it. Polling-browser automatic closes currently fail closed until correlated acknowledgements are supported.

The Mac pet shows a review bubble; checkboxes start unchecked. Dismissal never authorizes closure. Other clients keep uncertain tabs open.

Validation (2026-11-02): workspace tests and clippy passed in Linux, macOS and Windows CI for merged revision 7172de7. The Swift app compiled and signed into a temporary destination. All 15 extension unit tests passed. Live Chrome tests passed on Windows and macOS, and bundled Chromium passed on macOS, using disposable profiles disconnected from the installed daemon. These test real input, active/work/cart guards, expired commands, URL identity, selected-only closing, reopening, persisted Keep, and manual closure. The test advances only the extension worker clock to exercise inactivity without waiting 24 hours.

Run `node extension/tests/live.cjs` with Playwright and its Chromium installed, or set `CHROME_PATH` to a Chrome executable. The bundled run uses the full Chromium channel because the headless shell does not support extensions. On a remote Mac, `caffeinate -i node extension/tests/live.cjs` keeps the machine awake for the test. `RW_DEBUG=1` logs the current test operation. Earlier Mac worker timeouts did not reproduce with either browser during the awake runs; their cause remains unconfirmed.

The native Swift harness renders the checklist off-screen and verifies its content height, unchecked defaults, selected-only request, Keep, Later and expiry against mocked HTTP responses. Its image was visually inspected. An installed-version rollout and end-to-end interaction with the installed pet still need validation. No installed daemon or extension was replaced during these tests.

## Work pages and browser pages (2026-11-02)

Contract: a work page is closed automatically only once it is unloaded and has waited the longest bar, after every ordinary page; a loaded one never is. A browser settings page is closed before any web page.

- **Work page** (`tabpolicy::is_work_page`, mirrored in the extension as `isWorkPage`): a signed-in cloud, database or hosting console, a registrar or DNS panel (`cloud.oracle.com`, `namecheap.com`, `dash.cloudflare.com`, ...), or any page whose path has a whole segment such as `submit`, `new`, `checkout`, `settings` or `account` (plus `submit-...` style prefixes). It is protected from unloading for 4 h like other soft protections, keeps its return chance four times longer once that lifts, so it unloads last, and is described to Jev as a work page. Automatic closing never takes a loaded one; only the user's own review checklist, a close they approve in chat, or a manual close can. Once the page is unloaded there is no form or session state left in the tab to lose, so the sweep may close it when it has gone unused for the longest bar any tab can have (4x `unused_after_minutes`: four hours of browsing elsewhere by default) or a day untouched, and only after the ordinary pages (2026-11-20; see below). The extension applies the same test before it closes: unloaded, idle at least 240 minutes, not a dev server or editor.
- **Browser settings page** (`chrome://extensions`, `edge://settings`, ...; never `chrome-extension://`): still never unloaded, but may be closed automatically after the same idle floor, ahead of every web page.
- **Reopen by host**: a reopen is matched on the web host (without `www.`), not the exact URL, so reopening Namecheap from the address bar after its control panel was closed counts. Only a tab opened, or navigated to from another site, after the close is a reopen; tabs already open are not. A visit to the host counts as a reopen only within four hours of the warden's close there; the exact page the warden closed counts whenever it comes back. A reopen protects the reopened page for 24 h and raises the negative-feedback count of its host (max 4), which lifts the return estimate of the host's other tabs without locking them. The registered domain was too wide: one reopened Google search kept Gmail and Cloud Console. Browser pages (`chrome://`) are never kept. Navigating within a site keeps the tab's visit history.

Basis: on 2026-11-02 the old tab budget closed an Oracle Cloud console signed into a database, an Agensi "submit a skill" form and a twice-reopened Namecheap tab, because no protection matched their hosts and the budget never consulted Jev, while `chrome://extensions` stayed because every non-web page was protected forever.

## What held tabs open on 2026-11-20

Contract: a protection holds the tab it was earned by, for as long as there is something to lose, and no longer.

On the owner's Mac 46 tabs were open, most untouched since the day before, and every sweep closed none. One tab was a candidate. Three rules held the rest:

- **Typed text held a tab for good.** The page script reported "something was typed here" on any keystroke, and the flag only cleared when the tab navigated. A search, a sent chat message and a half-written form looked the same, so 23 tabs were "recent work". The page script (v2) now reports whether text the user typed is still sitting unsent in a field: a textarea, an editable region or a text input that is not a password, search or filter box. It counts as sent only when it is gone: the field was emptied or removed, or the page moved on. Pressing Enter or submitting clears nothing, because a required field can block the submit and a save can fail with the text still there. Only true or false leaves the page. Unsent text holds the tab for 24 h from the last activity in it. A page still running the old script (any tab not reloaded since the update) can only send the old signal, which now lapses 4 h after the last keystroke, like every other soft protection; flags recorded before typing was timed have lapsed.
- **A reopen locked its whole host.** A warden close on `github.com` stayed on record for 30 days, the next GitHub page opened was a "reopen", and a reopen protected every tab on the host for a day. Now as described under "Reopen by host" above: only a prompt return counts, and it keeps the page that came back.
- **Unloaded consoles never closed.** Ten tabs were work pages the warden had unloaded hours earlier. Now as described under "Work page" above.

Verification: `extension/tests/discard.cjs` (typed-work hold and lapse, reopen scope and window, unloaded console), `extension/tests/activity.cjs` (what counts as unsent text), `extension/tests/live.cjs` (real typing in Chromium), `browser.rs` `an_unloaded_work_page_closes_last_and_a_loaded_one_never`.

Limitations: the unsent-text test reads fields the user typed in; text pasted by a script or kept in page memory with no field is not seen. A filter box that keeps its text and is not marked as search, or a form that saves in place and keeps its values, holds its tab for up to a day. A tab opened before the extension update keeps the old page script until it is reloaded.

## Closing tabs from chat (2026-11-20)

Contract: asked to close tabs, the companion offers `close_idle_tabs`; nothing closes before the user approves the list.

`POST /assistant/chat` carries `closable_idle_tabs` in its facts: the tabs the sweep would close on its own plus the ones it would only ask about (`tabreview::closable`), at most `max_close_per_sweep`. "Close my tabs" and close variants get the proposal without a model. The model may name the action but never a tab; the daemon fills in the list. The proposal's label is "Close N unused tabs" and its preview is one line per tab, title and site only; the Mac, Windows and GTK approval dialogs all show it (`Proposed::detail`, at most 30 lines). Approval (`POST /assistant/action`, single use, two minutes) runs the same guarded close as the review card: the browser still refuses a tab that is active, pinned, playing audio, used in the last 15 minutes or holding typed text. The reply reports closed and kept counts; closed tabs are saved to the reopen list.

Limitations: the work list is a list. A site not on it is still caught by the extension's unsaved-input bit and by path segments, but a signed-in console on an unknown host with a plain path is an ordinary page until the user reopens it once.

The popup refreshes confirmed close history on local storage changes and polls
the daemon while open. If both sources report a URL, the newest close wins.
Review mode proposes low-return tabs first and leaves closure to the user.
