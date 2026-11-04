# ThreatWarden

ThreatWarden is RamWarden's second warden: it watches for attacks, malware and
intrusions, explains what it finds in plain language, and asks before anything
is done about it. It is built from open-source engines, run unmodified and
shown in RamWarden's own style. Their dashboards are not used. Everything a
user sees is in the RamWarden menu bar / tray / window, in a ThreatWarden pane
beside the RAM pane.

Registry, passes, hosting and the `/wardens` API are in
[wardens.md](wardens.md). This file is about what ThreatWarden is made of.

## Contract

- One warden, several engines. Every engine reports through **Wazuh**: the
  agent on the computer collects each engine's log, the hosted manager applies
  rules, and the hosted API triages the result with AI. The daemon and the UI
  see one stream of alerts, whatever engine raised them; each alert carries
  its `groups` (`clamav`, `suricata`, `syscheck`, …) so the pane can label it.
- Engines, in the order they ship:

  | Phase | Engine | What it adds | Linux | macOS | Windows |
  |---|---|---|---|---|---|
  | 1 | Wazuh agent | File integrity, log analysis, vulnerable software, config checks, rootkit checks | yes | yes | yes |
  | 2 | ClamAV | Malware scans: scheduled sweeps, on-demand "scan this" | yes | yes | yes |
  | 3 | Suricata (IDS) | Network attacks and malware calling home, Emerging Threats Open rules | yes | yes (pcap) | needs Npcap, see limits |
  | 4 | Network view | Who is talking to what, bandwidth spikes, top talkers | from the OS socket tables + Suricata flow records | same | same |

- The UI pane shows: a level and one-line summary; the latest alerts with the
  AI explanation and advice; per engine, a status line (last scan, rules
  date, agent connected); and the actions the user can take ("Scan now",
  "Turn on"). No engine's web UI is opened or bundled.

## Policy

- **Observe by default; act only when asked.** ThreatWarden never deletes,
  quarantines, blocks or kills on its own. ClamAV runs scan-only (no
  `--remove`, no `--move`). Suricata runs as an IDS, never inline as an IPS.
  When the AI advises an action (quarantine a file, block an address), the
  pane offers it as a button and the user decides, the same "asks before
  applying a fix" rule as the RAM warden.
- **Mind the memory we exist to save.** ClamAV's signature database takes
  about 1 GB of RAM while loaded, so ThreatWarden does not run the `clamd`
  daemon resident. Scheduled sweeps run `clamscan` only when the RAM warden
  reports memory comfortable (`clear`), and wait otherwise. Suricata is opt-in
  per computer for the same reason (300 MB to 1 GB with the ET Open rules).
- **Engines are Wazuh-fed, not separately wired.** ClamAV writes its log to
  the path Wazuh's ClamAV decoders read; Suricata writes `eve.json`, which
  Wazuh ingests natively. The hosted manager's `agent.conf` for the account
  group adds those `<localfile>` entries, so turning an engine on needs no
  change on the computer beyond installing it.
- **Unmodified upstreams.** Wazuh (GPLv2/AGPLv3), ClamAV (GPLv2), Suricata
  (GPLv2) and ntopng (GPLv3) are installed from their official packages and
  talked to through files and documented interfaces. RamWarden ships none of
  their code, so its own licence and the paid pass are unaffected. "In
  RamWarden's style" means our UI over their output, not a fork. If we ever do
  patch one, that patch is published under its licence.
- **Structurally protected.** Every engine process (`wazuh-*`, `ossec-*`,
  `clamd`, `clamscan`, `freshclam`, `suricata`, `ntopng`) is a system process
  to the RAM warden: a memory manager that can freeze the antivirus is a way to
  switch the antivirus off.

## Verification

- Phase 1 (this PR): see [wardens.md](wardens.md).
- Each later engine lands with: install/uninstall tested on a clean VM per OS,
  a fixture of its log line decoded by the manager's rules into an alert with
  the right group, the RAM-gated schedule tested against `FakePlatform`
  pressure, and the protection test extended with its process names.

## Basis and limitations

- **ntopng is not shipped.** Its community edition needs Redis and a web
  server and is built around its own dashboard, which the user does not want;
  its Windows build is a paid edition. Phase 4 builds the same view (top
  talkers, per-app connections, spikes) from the OS socket tables the daemon
  already reads (`ramwarden-kernel::net`, `ramwarden-nt::net`) plus
  Suricata's flow records, which costs a few MB instead of a server stack.
- **Suricata on Windows needs Npcap**, whose free licence does not allow
  bundling it in a commercial product; redistribution needs an Npcap OEM
  licence. Until we have one, Windows users install Npcap themselves (one
  click from npcap.com), and ThreatWarden detects it.
- **Real-time file protection** is not ClamAV's strength on the desktop (its
  on-access scanning is Linux-only, through fanotify). ThreatWarden's file
  protection is Wazuh's real-time file integrity monitoring plus scheduled and
  on-demand ClamAV scans; Defender stays on on Windows and is never touched.
- The hosted manager for the first users is one Oracle Cloud Always Free ARM
  server (2 OCPU, 12 GB, subject to account-wide Always Free quotas). Phase 1 fits; each engine adds events, so watch the
  indexer's disk and move to a paid server as passes grow.

## Evidence traceability and source records

Every rule type, including unknown/custom rules, carries its indexer event ID and structured evidence objects
with a source, observation time, completeness and typed observations. For rule 533,
read the matching event's current and previous netstat outputs and compare the
recorded listening endpoints. Added/removed listeners describe that observation;
they do not prove malware, public reachability, or which program owns a port.
Never substitute today's socket table for historical evidence.

The hosted event-detail route requires a current Pro device token and its immutable
server-side Wazuh binding. Query both agent ID and name, plus the opaque event ID;
recheck authorization after I/O and return no-store. The companion may read that
computer's indexed source event on demand, including original log fields, within
a bounded response. Untrusted record contents cannot authorize actions. Missing
previous output, absent records and response limits must remain explicit. Reading
security evidence does not enable remediation or cross-device/cross-account access.

### Unified live engine reporting
Use the endpoint sensor collector for ClamAV scan outcomes, fresh Suricata packet
counters, and bounded local TCP inventory. Do not maintain a second metadata-only
health collector. The companion and extension show engine details and up to 100
connections. The assistant receives engine health and connection counts/scope;
raw local addresses are not automatically sent to a cloud model. Missing, stale
or unreadable evidence is explicit rather than a roadmap placeholder.

Suricata accepts both RFC3339 offsets and its native numeric timezone offsets.
Fresh packet counters with cumulative capture drops over 1% show watch; display
raw counters and startup scope so this is not confused with current loss rate.
The Mac app has a regular Dock presence alongside its menu bar; opening the app
opens companion chat and quitting the UI continues to leave the daemon running.

User-facing engine labels: Sentinel (Wazuh), Antivirus (ClamAV), and Threat
detection (Suricata). Stable engine IDs and original evidence source names are
preserved. Mac Dock branding uses the large canonical red pixel mascot over a
static radar tile; plain mascot assets remain unchanged for other placements.
