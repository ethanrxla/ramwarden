# Tab feedback and review

Automatic closes require fresh extension activity metadata, at least 24 hours idle, a return estimate below 0.3, and all live safety guards. A tab-count target or same-site page cannot bypass this floor. Exact URLs, including query and fragment, define duplicates.

The updated extension reports timestamps and an unsaved-input bit, never typed text. Reopened and explicitly kept pages receive a 24-hour protection, without assuming browser removal was an intentional close. Local SQLite feedback and outcomes expire after 30 days. Negative feedback can only raise the return estimate; this is not external model retraining.

GET /browser/review offers at most ten uncertain tabs, at most once per 30 minutes. POST /browser/review/respond accepts a token, close/keep/later, and browser/id/url targets. Tokens expire after ten minutes and are single-use. Close rechecks current metadata and uses a correlated, URL-scoped close_safe extension command. Older extensions cannot execute it. Polling-browser automatic closes currently fail closed until correlated acknowledgements are supported.

The Mac pet shows a review bubble; checkboxes start unchecked. Dismissal never authorizes closure. Other clients keep uncertain tabs open.

Validation (2026-11-02): workspace tests and clippy passed in Linux, macOS and Windows CI for merged revision 7172de7. The Swift app compiled and signed into a temporary destination. All 15 extension unit tests passed. Live Chrome tests passed on Windows and macOS, and bundled Chromium passed on macOS, using disposable profiles disconnected from the installed daemon. These test real input, active/work/cart guards, expired commands, URL identity, selected-only closing, reopening, persisted Keep, and manual closure. The test advances only the extension worker clock to exercise inactivity without waiting 24 hours.

Run `node extension/tests/live.cjs` with Playwright and its Chromium installed, or set `CHROME_PATH` to a Chrome executable. The bundled run uses the full Chromium channel because the headless shell does not support extensions. On a remote Mac, `caffeinate -i node extension/tests/live.cjs` keeps the machine awake for the test. `RW_DEBUG=1` logs the current test operation. Earlier Mac worker timeouts did not reproduce with either browser during the awake runs; their cause remains unconfirmed.

The native Swift harness renders the checklist off-screen and verifies its content height, unchecked defaults, selected-only request, Keep, Later and expiry against mocked HTTP responses. Its image was visually inspected. An installed-version rollout and end-to-end interaction with the installed pet still need validation. No installed daemon or extension was replaced during these tests.

## Work pages and browser pages (2026-11-02)

Contract: a work page is never closed automatically, and a browser settings page is closed before any web page.

- **Work page** (`tabpolicy::is_work_page`, mirrored in the extension as `isWorkPage`): a signed-in cloud, database or hosting console, a registrar or DNS panel (`cloud.oracle.com`, `namecheap.com`, `dash.cloudflare.com`, ...), or any page whose path has a whole segment such as `submit`, `new`, `checkout`, `settings` or `account` (plus `submit-...` style prefixes). It is protected from unloading for 4 h like other soft protections, keeps its return chance four times longer once that lifts, so it unloads last, and is described to Jev as a work page. Automatic closing never takes one; only the user's own review checklist or a manual close can.
- **Browser settings page** (`chrome://extensions`, `edge://settings`, ...; never `chrome-extension://`): still never unloaded, but may be closed automatically after the same 24 h floor, ahead of every web page.
- **Reopen by site**: a reopen is matched on the site, not the exact URL, so reopening Namecheap from the address bar after its control panel was closed counts. A reopen protects every page on that site for 24 h, and repeated reopens raise the negative-feedback count (max 4). Navigating within a site keeps the tab's visit history.

Basis: on 2026-11-02 the old tab budget closed an Oracle Cloud console signed into a database, an Agensi "submit a skill" form and a twice-reopened Namecheap tab, because no protection matched their hosts and the budget never consulted Jev, while `chrome://extensions` stayed because every non-web page was protected forever.

Limitations: the work list is a list. A site not on it is still caught by the extension's unsaved-input bit and by path segments, but a signed-in console on an unknown host with a plain path is an ordinary page until the user reopens it once.

The popup refreshes confirmed close history on local storage changes and polls
the daemon while open. If both sources report a URL, the newest close wins.
Review mode proposes low-return tabs first and leaves closure to the user.
