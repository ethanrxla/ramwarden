# Wardens

RamWarden is a team of *wardens*: small agents that each watch one thing on
the user's computer and explain what they see. The RAM warden is the original
product: it watches memory pressure and drives the reclaim ladder. The Wazuh
warden is the second: it watches the machine's [Wazuh](https://wazuh.com)
security agent and reports threats. More wardens are added the same way.

## Contract

- `GET /wardens` lists every warden the daemon knows, in a fixed order, with:
  `id`, `name`, `role`, `icon` (a daemon URL for its SVG), `access`
  (`free`, `pro`), `unlocked`, `enabled`, and `status`.
- `status` is `{level, headline, detail, checked_at}`. `level` is one of
  `clear`, `watch`, `alert`, `off`, `locked`.
  - `clear`: nothing needs the user.
  - `watch`: something is worth a look; nothing urgent.
  - `alert`: the user should act now.
  - `off`: the warden is unlocked but not configured or not reachable. The
    headline says which, and what to do.
  - `locked`: the warden needs a plan the user does not have.
- `GET /wardens/{id}` returns the same entry plus a warden-specific `report`.
  For `wazuh` the report holds the local agent's state, the manager's view of
  the agent, and up to 50 recent alerts at or above the configured level.
  A locked warden's report is `null`: locked wardens do no work at all, so
  they never contact a manager or read the agent's files.
- `GET /wardens/{id}/icon.svg` returns the warden's icon (`image/svg+xml`).
  Every client (menu bar, tray, GTK window, extension, website) draws a warden
  with this icon, so a new warden needs no client release to appear.
- An unknown id is `404`.

## Policy

- **Wardens observe; only the RAM warden acts.** The Wazuh warden is
  read-only. It never suspends, kills, quarantines or blocks anything, and it
  never triggers Wazuh active response. Wazuh's own active response, configured
  on the manager, is the only thing that acts on a detection. This keeps the
  rule in `CLAUDE.md` intact: suspend and kill only ever target watchlisted
  names.
- **The security stack is structurally protected.** Wazuh's own processes
  (`wazuh-*`, `ossec-*`, `wazuh-agent.exe`, `WazuhSvc`) are classified as
  system processes, so the RAM warden can never freeze or kill them, whatever
  the watchlist says. A memory manager that can suspend the security agent is
  a way to switch the security agent off.
- **Access.** Each warden is `free` or `pro`. The RAM warden is free. The
  Wazuh warden is `pro`: it is unlocked by an active Pro plan, or by the
  `wazuh` add-on on its own. The entitlement is read from
  `entitlement.json` beside the history database, written by the account
  sign-in. No file, a malformed file, or an expired one is the free tier.
  Signature checking of that file belongs to the account code, not here.
- **Levels from Wazuh rule levels.** Wazuh rules carry a level from 0 to 15.
  An alert at level 12 or above in the lookback window is `alert`; any alert at
  or above `min_level` (default 7) is `watch`; none is `clear`. Twelve is
  where Wazuh's own documentation starts calling events "high importance".
- **Agent health matters as much as alerts.** A disconnected agent cannot
  report a threat, so silence from it is not `clear`: a local agent that is
  installed but not connected, or that the manager reports as anything but
  `active`, is `watch` with that as the headline.
- **Credentials never travel in plain text.** The manager and indexer URLs
  must be `https`, except on a loopback address. Self-signed managers (the
  default Wazuh install) are trusted by pointing `ca_file` at their root CA;
  there is no switch that turns certificate checking off.
- **Degrade, don't fail.** No Wazuh agent, no manager configured, a manager
  that is down, or wrong credentials: each is an `off` status with a headline
  that names the cause. The daemon and the RAM warden are unaffected.
- **Adding a warden** means: one entry in `ramwarden_core::wardens::CATALOGUE`,
  an icon in `web/assets/wardens/`, a config section under `[wardens.<id>]`,
  and a poller in `ramwarden-daemon::wardens`. Nothing else changes.

## Verification

- `ramwarden-core` `wardens` tests: the free tier unlocks only free wardens;
  Pro unlocks all; an add-on unlocks only its own warden; an expired plan is
  free; alert levels map to `clear`/`watch`/`alert` at the thresholds above; a
  disconnected agent is never `clear`.
- `ramwarden-core` `roles` test: Wazuh process names on all three platforms
  are structural.
- `ramwarden-wazuh` tests: the agent state file parses on each platform's
  format; manager and indexer responses parse from recorded JSON; a
  non-loopback `http` URL is refused; the client authenticates, then queries,
  against a local fake manager.
- `ramwarden-daemon` `wardens_api` tests: `/wardens` lists `ram` and `wazuh`;
  `wazuh` is `locked` on the free tier and its report is `null`; icons are
  served as SVG; an unknown id is 404.

## Basis and limitations

- Wazuh is GPLv2 (its newer engine is AGPLv3). RamWarden does not link,
  vendor or modify any Wazuh code: it reads the agent's state file and talks
  to the manager's documented REST APIs over the network. The user installs
  the official, unmodified Wazuh agent and manager from Wazuh's own packages.
  That keeps RamWarden's own licence, and a paid add-on, unaffected.
- Alerts live in the Wazuh indexer, not the manager, so seeing alerts needs
  indexer credentials as well as manager API credentials. With only the
  manager configured the warden reports agent health but no alerts.
- On Linux and macOS the agent's state file is readable only by root and the
  `wazuh` group. Without that access the warden reports the agent as installed
  with its connection unknown, and relies on the manager's view.
