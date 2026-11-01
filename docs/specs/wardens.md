# Wardens

RamWarden is a team of *wardens*: small agents that each watch one thing on
the user's computer and explain what they see. The RAM warden is the original
product: it watches memory pressure and drives the reclaim ladder. ThreatWarden
is the second: it watches for attacks, malware and intrusions,
built on open-source engines starting with [Wazuh](https://wazuh.com)
(`docs/specs/threatwarden.md`). More wardens are added the same way.

## Contract

- `GET /wardens` lists every warden the daemon knows, in a fixed order, with:
  `id`, `name`, `role`, `icon`, `access` (`free`, `pro`), `unlocked`,
  `enabled`, and `status`; plus `paid`, the ids of paid wardens whose pass
  is current, and `tier` and `plan_status` from the signed entitlement
  (`null` without one), for display only. `icon` is a daemon URL for the warden's SVG, or `null` for the
  RAM warden, which the clients already draw as the brand ram. A paid
  warden also carries `price` (a display string, e.g. `In RamWarden Pro, from $5 a month`) and
  `upgrade_url` (where "Get a pass" opens); both are `null` for free wardens.
  Clients fall back to their own defaults when either is missing.
- `status` is `{level, headline, detail, checked_at}`. `level` is one of
  `clear`, `watch`, `alert`, `off`, `locked`, `setup`.
  - `clear`: nothing needs the user.
  - `watch`: something is worth a look; nothing urgent.
  - `alert`: the user should act now.
  - `off`: the warden is unlocked but not configured or not reachable. The
    headline says which, and what to do.
  - `locked`: the warden needs a plan the user does not have.
  - `setup`: the warden is installing what it needs (the Wazuh agent).
- `GET /wardens/{id}` returns the same entry plus a warden-specific `report`.
  For `threat` the report holds the local agent's state, the manager's view of
  the agent, and up to 50 recent alerts at or above the configured level,
  plus `engines`: one `{id, name, state, detail}` per ThreatWarden engine
  (`wazuh`, `clamav`, `suricata`, `network`), where `state` is `on`, `off`,
  `planned` or `error`. Engines not shipped yet are `planned`, so the pane
  can show the roadmap without guessing.
  A locked warden's report is `null`: locked wardens do no work at all, so
  they never contact a manager or read the agent's files.
- `GET /wardens/{id}/icon.svg` returns an add-on warden's icon
  (`image/svg+xml`), so a new warden needs no client release to appear. Icons
  are 8-bit sprites on the RAM warden's 48x48 grid and palette, drawn by
  `scripts/warden-sprites.py`. ThreatWarden is a cyborg ram.
- An unknown id is `404`.

### Hosted ThreatWarden (the default)

Paying users never run a Wazuh server, copy a key or pick a certificate.
RamWarden hosts the Wazuh manager and indexer; the daemon authenticates to the
hosted API (`ramwarden-api`, `https://api.ramwarden.app`) with the device
token written at device sign-in (`<config dir>/ramwarden/device-token`).

- `POST /wardens/threat/setup` (daemon, loopback) installs and enrols this
  computer's Wazuh agent. It is started by the user (a "Turn on" button), never
  by the daemon on its own, because it raises the OS's one admin prompt (UAC
  on Windows, the password or Touch ID sheet on macOS, polkit on Linux). It
  answers at once with `{started: true}`; progress shows in the warden status
  (`setting up`, then the normal levels, or `off` with the reason).
- The daemon asks the API to enrol it:

  `POST /v1/wardens/threat/enroll` with `Authorization: Bearer <device token>`
  and `{os, arch, hostname}` returns

  ```json
  {"manager": "agents.ramwarden.app", "agent_name": "acct1a2b-desk",
   "group": "acct-1a2b", "agent_key": "<base64 of '042 acct1a2b-desk any <64 hex>'>",
   "package": {"url": "https://packages.wazuh.com/…", "sha256": "…", "kind": "msi|pkg|deb|rpm"}}
  ```

  `403` without a current ThreatWarden pass. The API registers the agent itself
  through the manager API (`POST /agents`, then `PUT /agents/{id}/group/{group}`,
  creating the account's group on first use) and returns that agent's key. There
  is no shared enrolment password, so one account can never enrol a machine
  into another account's group, and the manager's enrolment port (1515) stays
  closed. The API also picks the package, so a Wazuh upgrade needs no RamWarden
  release.
- The daemon downloads the package, refuses it unless its SHA-256 matches,
  and runs Wazuh's own installer elevated with the manager address, then
  writes the agent key to the agent's `client.keys` and starts it, all behind
  one admin prompt. Every value from the API is checked against a strict
  pattern (hostname, `[A-Za-z0-9._-]` names, a key that decodes to
  `<digits> <this agent's name> <any|ip> <64 hex>`, a package only from
  `https://packages.wazuh.com`) before it reaches an elevated command line.
  The installer wait is capped at ten minutes. On timeout, setup reports an
  error and warns that an elevated installer may still be running; the user
  should check its result before retrying. Windows elevation, key-write and
  service errors must fail setup rather than reporting success.
- Status comes from `GET /v1/wardens/threat/status` with the same token:

  ```json
  {"agents": [{"name": "…", "status": "active", "last_keep_alive": "…"}],
   "alerts": [{"timestamp": "…", "rule_id": "5712", "level": 10,
               "description": "…", "groups": [], "tactics": [],
               "explanation": "Someone tried 40 passwords for SSH in a minute.",
               "advice": "Nothing to do: the attempts came from your own network and failed.",
               "verdict": "benign|suspicious|threat", "confidence": 0.93}],
   "summary": "One thing to look at today."}
  ```

  The API only ever returns the caller's own account's agents and alerts:
  tenant isolation is enforced by the API filtering on the account's agent
  group, never by trusting a name the client sends.
- **AI triage happens on the server.** Each alert is explained in plain
  language and given a verdict by the hosted model tier (Jev for the typed
  verdict and its confidence; a text model for the explanation). The daemon
  shows the explanation; the level still comes from Wazuh's rule level, and a
  `threat` verdict at or above `min_level` raises it to `alert`. A model
  can make an alert louder, never quieter.
- Self-hosting stays possible with `[wardens.threat] mode = "self"` and the
  manager settings below; see `docs/threatwarden-self-hosting.md`.

## Policy

- **Wardens observe; only the RAM warden acts.** ThreatWarden is
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
- **Access.** Each warden is `free` or `pro`. The RAM warden is free.
  ThreatWarden comes with the RamWarden Pro plan. The record is
  `entitlement.json` beside the history database, as the account server
  issued it (`GET /v1/entitlement`, v2 in `docs/specs/hosted-api.md`):
  `{"v":2, "account", "device", "issued_at", "tier", "status",
  "wardens": {"<id>": <unix expiry>}, "sig"}`. A paid warden is unlocked
  while its own expiry is in the future, and ThreatWarden also needs
  `tier = "pro"`; missing means locked. `tier` can only narrow access, and
  `status` (`trial`, `active`, `grace`, `cancelled`, `lapsed`) is for
  display. `"ram"` in the map is the hosted Jev plan and does not affect the
  local RAM warden. No file, or a malformed one, unlocks nothing. The server
  signs a canonical text (`ramwarden-entitlement-v2`, then one `key=value`
  line each for account, device, issued_at, tier, status, then
  `warden:<id>=<expiry>` sorted) with Ed25519; the daemon verifies it in
  `Entitlement::load` against a public key compiled in at build time from
  `RAMWARDEN_ENTITLEMENT_PUBLIC_KEY`. An edited, unsigned, v1 or foreign
  record, or a build without the key, unlocks no paid warden. The key is
  never read at run time, which would let anyone sign their own passes.
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
- `ramwarden-daemon` `wardens_api` tests: `/wardens` lists `ram` and `threat`;
  `threat` is `locked` with no pass and its report is `null`; a RAM pass
  leaves it locked and a ThreatWarden pass unlocks it; its icon is served as SVG;
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
