# Warden voice: speech bubbles, tab check-ins and comments

The RAM warden talks. Now and then it asks whether you are coming back to a
tab it is about to close, makes a light comment about your memory, or says
what it just did. Everything it says appears in a speech bubble next to the
warden: the floating ram on macOS, the window and tray menu on Windows, the
window on Linux, and the browser extension's popup.

This is the in-app half of product spec section 2.5 (pets) and the
"asks before applying a fix from chat" promise. The website's pets are a
separate implementation in `web/` and never comment on anything real.

## Contract

- **One voice, many faces.** The daemon decides what the warden says and when
  (`ramwarden-daemon::voice`). Every client only renders the bubble the daemon
  hands it and posts the user's answer back, so macOS, Windows, Linux and the
  extension say the same thing at the same time and answer once for all.
- **Kinds of bubble**, in priority order:
  1. `activity`: what the warden just did, from a real release pass. "Put 4
     idle tabs to sleep. That is roughly 1.1 GB back." Figures from
     `est_mb` are estimates and the text says so ("roughly").
  2. `check_in`: "Coming back to "Rust book: lifetimes"? It has been idle
     for 3 hours." Choices: **Yes, keep it**, **No, close it**, **Ask me
     later**.
  3. `quip`: a light joke, sometimes about the real numbers: tab count,
     memory in use. "41 tabs. I'm not judging. I'm counting."
  4. `reply`: the warden's short answer to a choice ("Closed. It's in the
     reopen list."). Shown for 10 seconds, never counted against the limits.
- **Answers change decisions** (the reason the check-in exists):
  - *Keep*: the tab is held out of every automatic close (sweep, budget,
    duplicates) for `keep_hours` (24). Unloading is still allowed, because it
    is reversible and the tab stays in the strip.
  - *Close*: the tab is closed now, through the same checked path as a manual
    close (`close_selected_tabs`: fresh report, owner and URL match,
    `can_close`). It goes in the reopen list. If the browser does not confirm,
    the reply says so: an unconfirmed close is not reported as done.
  - *Later* (or dismissing the bubble, or letting it expire): nothing closes
    because of the question, and that tab is not asked about again for
    `later_hours` (6).
  - While a check-in is on screen its tab is held, so a sweep cannot close the
    tab out from under the question.
- **Who gets asked about.** Only tabs the warden may close (`can_close`:
  never active, pinned, audible, private, loading or protected), idle at least
  `ask_after_minutes` (60), and not answered recently. First the tabs the next
  sweep would close, then borderline ones (return chance between 15% and 60%,
  the same band where the daemon asks Jev), biggest estimated memory first.
  A check-in is a question, never a widening of what may be closed.
- **Rate limits: never annoying.** At most one bubble every `min_gap_minutes`
  (30), a check-in at most every `ask_every_minutes` (45), a quip at most
  every `quip_every_minutes` (120), at most `max_quips_per_day` (3) jokes and
  `max_per_day` (6) bubbles of any kind a day. The site's pets are held to a
  similar 3 jokes a visit. A bubble
  stays up for 60 seconds (`quip`, `activity`) or 10 minutes (`check_in`).
- **Mute.** One click mutes the warden for an hour, until tomorrow, or until
  unmuted. The mute is saved in `<data dir>/ramwarden/voice.json`, so it
  survives restarts, and all clients see it. `[voice] enabled = false` turns
  the voice off entirely; `ask_about_tabs = false` keeps the comments and
  drops the questions; `quips = false` keeps the questions and drops the jokes.
  With the voice off or muted, closing behaves exactly as before.
- **Who has a pet.** The pet and its bubble come with Plus and Pro, not Lite
  (`Entitlement::has_pet`). A lapsed plan loses it. Signed out is the
  self-built open-source app, which keeps its pet. Without a pet,
  `/warden/bubble` answers `"pet": false` and the clients hide the pet, and
  closing behaves exactly as it does with the voice off.
- **Lines live in data**, one JSON file per warden
  (`assets/warden/lines/ram.json`), so they can be edited without code. They
  follow the site's style: short, kind, no em dashes. Placeholders: `{title}`,
  `{idle}`, `{tabs}`, `{percent}`, `{count}`, `{mb}`.

## API

| Route | Does |
|---|---|
| `GET /warden/bubble` | `{"bubble": Bubble or null, "pet": bool, "muted": bool, "muted_until": unix or null}` |
| `POST /warden/answer` | `{"id", "choice"}`, choice is `keep`, `close`, `later` or `dismiss`. Returns `{"ok", "reply", "closed"}`. A stale id is `409`. |
| `POST /warden/mute` | `{"muted": bool, "hours": optional}` |

`Bubble` is `{"id", "warden": "ram", "kind", "text", "choices": [{"id", "label"}], "tab": {"browser", "id", "url", "title"} or null, "expires_in_s"}`.

## Clients

| Client | Where the bubble shows |
|---|---|
| macOS menu bar (`macos/menubar/Companion.swift`) | A bubble panel above the floating pixel ram, with the choices as buttons and a mute menu. If the ram is hidden, the menu bar menu shows the question instead. |
| Windows (`ramwarden-ui` `tray`) | The tray tooltip and tray menu carry the question and its choices, so it works with the window closed. The window shows the bubble across its top. |
| Linux (`ramwarden-ui` `gui`) | A bubble bar at the top of the GTK window. |
| Extension (`extension/`) | The popup shows the bubble at its top. While a check-in waits, the badge shows `?`. |

The shared Rust client and bubble model are in `ramwarden-ui/src/voice.rs`,
used by both the GTK and egui faces.

## Analyze now (extension)

The popup's **Analyze Now** asks the warden for its read of the tabs, the way
chat does, instead of a rule count (`POST /browser/explain`,
`ramwarden-daemon::tab_analysis`).

- The model gets measured memory and a numbered, feature-only tab list (site
  type, idle time, status, estimated MB, return chance), never a URL, title or
  host. It refers to tabs as `#N`; titles are put back on this machine.
- It may suggest tabs to unload and to close. Suggestions for tabs that are
  not eligible (unload) or closeable (close) are dropped. Nothing happens
  until the user presses **Unload N** or **Close N**, which go through
  `/browser/discard` and `/browser/close` and are re-checked there.
- The model is the chat's (local first, or cloud when preferred and opted
  in). With none answering, the rules' view is written in the warden's voice
  and marked "Rules only".

## Verification

- `voice.rs` unit tests: priority order, every rate limit, the daily cap and
  its reset, mute and its expiry, keep/later windows, the hold while a
  question is up, that check-ins only pick closeable tabs, and that every line
  in `ram.json` fills its placeholders and has no em dash.
- `tests/voice_api.rs`: the routes end to end on a fake platform, including a
  `close` answer that the browser confirms and one it does not.
- `tab_analysis.rs` tests: no title or address reaches the model, `#N` becomes
  a title, suggestions are limited to what may be touched, and the rules
  fallback.
- `extension/tests/warden.cjs` (run in CI): the popup's bubble, mute and
  analysis rendering.

## Basis and limitations

- The daemon cannot see typing, so "never while typing" (website rule) is
  approximated by the gaps and the daily cap, and by the mute.
- Answers are kept in memory, keyed by browser, tab id and URL. A browser
  restart renumbers tabs, so answers do not survive it; that is the right
  failure, since the same id may then be a different page.
- Account-synced mute (product spec 4.3) and ThreatWarden lines come later;
  the bubble carries a `warden` field so they slot in without a new route.
