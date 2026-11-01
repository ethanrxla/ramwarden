# Wardens

RamWarden is a team of *wardens*: small agents that each watch one thing on
the user's computer and explain what they see. The RAM warden is the original
product: it watches memory pressure and drives the reclaim ladder. The Wazuh
warden is the second: it watches the machine's [Wazuh](https://wazuh.com)
security agent and reports threats. More wardens are added the same way.

## Contract

- `GET /wardens` lists every warden the daemon knows, in a fixed order, with:
  `id`, `name`, `role`, `icon`, `access` (`free`, `pro`), `unlocked`,
  `enabled`, and `status`; plus `paid`, the ids of paid wardens whose pass
  is current. `icon` is a daemon URL for the warden's SVG, or `null` for the
  RAM warden, which the clients already draw as the brand ram.
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
- `GET /wardens/{id}/icon.svg` returns an add-on warden's icon
  (`image/svg+xml`), so a new warden needs no client release to appear. Icons
  are 8-bit sprites on the RAM warden's 48x48 grid and palette, drawn by
  `scripts/warden-sprites.py`. The Wazuh warden is a cyborg ram.
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
- **Access.** Each warden is `free` or `pro`. The RAM warden is free. Paid
  wardens are sold as passes, one per warden with its own expiry, so a pass
  for one never unlocks another and a bundle is simply several passes. The
  record is `entitlement.json` beside the history database, as the account
  server issued it: `{"v":1, "account", "device", "issued_at",
  "wardens": {"<id>": <unix expiry>}, "sig"}`. A paid warden is unlocked
  while its expiry is in the future; missing means locked. `"ram"` in that
  map is the hosted Jev tier and does not affect the local RAM warden. No
  file, or a malformed one, unlocks nothing. The server signs the canonical
  JSON without `sig` with Ed25519; the daemon verifies it in
  `Entitlement::load` against a public key compiled into `ramwarden-core`
  (until that key exists, unsigned records are accepted).
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

- `ramwarden-core` `wardens` tests: no passes unlock only free wardens; a
  pass unlocks only its own warden (a RAM pass never unlocks Wazuh); a bundle
  keeps separate expiries; an expired pass is locked; alert levels map to `clear`/`watch`/`alert` at the thresholds above; a
  disconnected agent is never `clear`.
- `ramwarden-core` `roles` test: Wazuh process names on all three platforms
  are structural.
- `ramwarden-wazuh` tests: the agent state file parses on each platform's
  format; manager and indexer responses parse from recorded JSON; a
  non-loopback `http` URL is refused; the client authenticates, then queries,
  against a local fake manager.
- `ramwarden-daemon` `wardens_api` tests: `/wardens` lists `ram` and `wazuh`;
  `wazuh` is `locked` with no pass and its report is `null`; a RAM pass
  leaves it locked and a Wazuh pass unlocks it; its icon is served as SVG;
  an unknown id is 404.

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
