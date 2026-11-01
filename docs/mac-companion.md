# Mac status app and ram companion

The existing `macos/menubar` app owns the percentage and colored status circle.
The Rust daemon remains responsible for monitoring and memory management.
Run `scripts/install-macos.sh` to build and install both. Each has a separate
per-user LaunchAgent and log (`ramwarden.log` and `ramwarden-menubar.log` under
`~/Library/Logs`). Quitting the menu-bar app leaves the daemon running. Reopen
`~/Applications/RamWarden.app` to restore it.

The companion starts hidden. Use the status menu to show it, open the assistant,
or customize its wool, horns, hooves, and eyes. The 48×48 sprite has a tapered
muzzle, curled side horns, a beard, and configurable slit, round, or pupil-free
eyes. Eye colors and styles persist locally. Drag to move it and click to chat.
Chat is a compact borderless bubble anchored beneath the ram, rather than a
separate application window. Right-click the ram for Chat, Settings, and Hide.
Dragging the ram repositions its bubble; Escape or the close button dismisses
chat. Hiding the ram hides its bubble too. Closing chat or settings also closes
native color panels. On macOS 26+, chat uses native clear Liquid Glass; earlier
macOS versions use ultra-thin material. The empty bubble is just 80 points high
and expands to 240 points for conversation.

Colors, visibility, and position are stored in local app preferences. Chat is
kept in memory only. Hiding the companion destroys its view and animation timer.

## Assistant contract

`POST /assistant/chat` is registered only when the daemon binds to loopback and
is covered by the existing Origin/Host guard. JSON input is `message` (nonempty,
at most 4,000 UTF-8 bytes) and optional `history` (at most 12 user/assistant turns,
6,000 bytes per turn, 24,000 bytes total). Only one assistant request runs at a
time; additional requests receive 429. Invalid input receives 400.

The response includes `reply`, `tier` (`local`, `cloud`, or `facts`), `facts`, and
`read_only: true`. The context contains current RAM, the largest process groups,
and eight recent actions. It excludes configuration secrets and live browser
URLs/titles. Action targets can contain historical titles; treat the context as
private. Chat never applies changes. It may return a short-lived `proposed_action`
for pausing/resuming management, unloading currently eligible idle browser tabs,
or resuming a currently frozen app. The user must choose Review fix and confirm
Apply fix. `POST /assistant/action` consumes the single-use proposal token and
rechecks the current state before executing it; proposals expire after 120 seconds.
No arbitrary shell commands, process kills, or payment actions are supported.

Common direct requests such as “free more ram” use a measured local plan, even
without an AI response. They offer idle-tab unloading only when eligible tabs
exist; otherwise they explain the missing browser connection or lack of eligible
tabs. No changes occur until the user confirms. The pet and chat display the
short name Warden.

The original sly, theatrical woodland-guardian persona and factual boundaries
are in `crates/ramwarden-daemon/src/assistant_prompt.rs`. It can use dry humor and
light swearing while separating measured facts from interpretation.

The app polls `/companion/state`, a lightweight snapshot that avoids waiting for
monitoring/remediation locks, and tolerates two transient failures before showing
disconnection. It reconnects automatically.

Ollama and NVIDIA use the existing models, credentials, provider preference,
VRAM guard, and NVIDIA cloud opt-in. Chat can additionally opt in independently:

```toml
[assistant]
cloud_enabled = true
cloud_model = "nvidia/nemotron-3.5-lightning-30b-a3b"
```

This uses the existing NVIDIA key for user-initiated chat without enabling
`[nvidia] enabled = true` for automated analysis. With neither opt-in, a key
alone does not enable cloud requests.
Jev's typed decision model is not a conversational provider. Each tier has a
20-second assistant timeout; if no tier answers, the response reports measured
facts and explicitly says AI is unavailable. Generated interpretations are not
authoritative explanations of a process's purpose.

## MongoDB and future website integration

`python scripts/check-mongodb.py` (Python 3.11+, `pymongo` installed in a virtual
environment) verifies authentication and runs a read-only Atlas ping. It reads
`MONGODB_URI` from the environment, then `mongodb_uri`/`MONGODB_URI` in
`~/.config/ramwarden/secrets.toml`, then `MONGODB_URI` from a project or per-user
`.env`. It never prints the URI, password, or raw driver errors.

MongoDB is separate from the desktop assistant and local SQLite history. Do not
ship an Atlas URI in the Mac app, browser extension, or website frontend. The
future website backend should hold its own least-privilege credential. Customer,
payment-provider event IDs, and entitlement records can live in Atlas; actual
Stripe or crypto checkout and verified payment-event processing belong in that
backend. Checkout integration and database collections are deferred until the
website and its account model exist. Do not grant access based on a client-side
payment-success redirect.

## Verification

- `cargo test -p ramwarden-daemon --lib --test assistant`
- `RAMWARDEN_APP_DEST=/tmp/RamWarden.app bash macos/menubar/build.sh`
- On a Mac: verify the percentage updates, toggle and drag the ram, customize
  each color, reopen the app to check saved settings, and send a chat message.
- Stop/restart the daemon: the icon and ram must indicate disconnection and
  recover. Quit only the app: `/health` must remain available.
- Hidden ram: no floating window or animation; first launch remains hidden.
