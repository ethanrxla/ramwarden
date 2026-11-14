# ThreatWarden console and companion

The shared native `WardenCompanion` and `CompanionModel` own identity, chat transport, glass chat panels, status bubbles, cancellation and speech lifecycle. RAM and threat companions specialize observations and personas. Security does not inherit memory-management permissions. The server independently enforces the threat pass and security-only context; changing a client profile cannot grant actions.

Current UI: compact menu-bar report with a scrollable list of up to 50 reported alerts. Each card shows rule level, rule id, description, timestamp and supplied advice. The list is evidence from Wazuh, not an assertion of confirmed malware. Chat uses a bounded fresh subset of security observations and an original tactical-machine persona. Unavailable AI falls back to reported facts. RAM retains its separate woodland persona. Voice provider failures leave text usable and identify an HTTP 402 library-voice billing restriction.

## Next engine panes

- Overview: last verified check, connected agents, alert counts and each engine's actual health. Never infer antivirus or packet inspection from a Wazuh heartbeat.
- Alerts: filter by engine/severity/time, group repeated rules without discarding individual evidence, open an event's details and ask the security companion to explain it. Preserve timestamps and source identifiers. Do not label findings as AI-confirmed unless triage was actually performed and labeled.
- Antivirus: installed/signature/last-completed-scan state, scan scope and progress, cancel control and resource consumption. Use an isolated worker with time and output bounds. Avoid a permanently resident scanner by default; schedule only when the computer has sufficient measured memory and is not busy or on constrained battery. Measure the real signature database memory cost before promising a budget. Quarantine requires a separately reviewed action naming the exact file/hash and any administrator approval.
- Network intrusion detection: Suricata sensor health, interface, rules freshness and recent detections. Installation and capture permissions are explicit. Raw packet content is not uploaded as a convenience default. A server's own sensor does not imply the customer's Mac is covered.
- Network view: compact per-app remote endpoint and connection activity, with data freshness and collection scope. No unsupported claims of malware, no automatic blocking, no packet-body collection disguised as a status view.

Keep planned/unavailable states actionable and visually distinct from running/verified. Hide closes both bubble and chat, cancels in-flight conversation and stops speech. The native report and extension pane should share the same underlying engine report schema.
