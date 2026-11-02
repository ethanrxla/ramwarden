# Account-linked RamWarden and ThreatWarden rollout

## Objective and acceptance evidence

The end state is working RAM and threat coverage on the developer's Windows PC
and MacBook, both owned by one account, then a repeatable customer setup across
Windows, macOS and Linux. This document records incomplete work; it is not a
claim that installation or a green test proves coverage.

| Requirement | Evidence needed before completion | Current evidence / remaining work |
| --- | --- | --- |
| Developer account and both devices | Verified email/wallet sign-in, two distinct device tokens, same server account, valid Pro entitlement | Neither device has a saved token. User supplied developer email and manager address in the task; do not put the email in public repository files. |
| Wazuh on both devices | Manager lists both agents as connected; harmless synthetic events arrive under the correct account | Windows service and Mac agent processes run. Manager delivery not verified. Manager is agents.ramwarden.com. |
| ClamAV | Current signatures, scan-only execution and safe test detection delivered through Wazuh on both devices | Not verified. Keep Defender enabled. No automatic deletion/quarantine. |
| Suricata | Capture works on intended interface; safe fixture produces an IDS alert through Wazuh | Windows service runs; capture/delivery not verified. Mac installation not verified. Check Npcap availability/licensing on Windows. |
| App/extension sign-in | Extension opens wallet/email device approval; daemon alone stores its device token; both show the same account | Hosted device-code endpoints exist on PR23/PR5 stack; local bridge and extension UI remain to implement. |
| Tailscale integration | Account-bound discovery and selection, authenticated pairing, cross-account denial, revocation | Both existing host clients are online. No RamWarden integration yet. |
| Licensing and database | Atomic 3-device Plus / 5-device Pro reservation, concurrent enrollment/replay/revocation tests | Limits exist in API source; end-to-end verification and network identity mapping remain. |
| Request limits based on real use | Representative measured request totals, costs, restart handling and documented Plus/Pro sizing | Mac reports 4 calls in preceding hour, 1724 input / 272 output tokens since daemon startup; not a daily baseline. Do not set quotas from this sample. |
| Personalized behavior | Account-scoped learning data, device/OS identity, bounded feedback, isolation and offline behavior tests | Local activity protections exist. Cross-device account learning is not verified. |
| Repeatable setup | Versioned installation/enrollment/rollback steps, a fresh-device reproduction, optional setup skill | Read-only readiness probe added; full setup runbook/skill remains. |
| Hosting | Existing Oracle resources assessed, manager/API/indexer health and capacity verified | Inspect existing server first; only quote a paid alternative if required. No droplet purchased. |
| Sales/support | Verified sign-in, truthful plan answers, chat-ticket-reply-revisit behavior | Support PR23 browser tests pass; production email sign-in still needs verification. |

## Transport and licensing boundaries

Tailscale is transport and discovery, not proof of paid entitlement. Reserve
device slots transactionally in the application database before enrollment;
counting online tailnet nodes alone is neither atomic nor a license check.
Offline registered devices still occupy slots. Repeated activation must be
idempotent and must not issue extra credentials.

Use the existing personal host clients for initial discovery without retagging
the user's devices. A future embedded tsnet identity can represent the app
service separately. Select an explicit supported transport mode and verify it;
do not claim tsnet is shipped merely because a host client exists.

For a shared service tailnet, use per-account tags and explicit deny-by-default
grants with cross-account tests. Tags identify devices; groups identify users.
The example single shared tailnet must not inherit broad access between customer
devices. Revoke the application device credential and the associated network
node when removing coverage. Revoking an enrollment auth key alone does not
deauthorize a node already enrolled with it.

References: [auth keys](https://tailscale.com/docs/features/access-control/auth-keys),
[tags](https://tailscale.com/docs/features/tags),
[tsnet](https://tailscale.com/docs/features/tsnet).

## Readiness probe

Run `python scripts/device-readiness.py` on each device. It reads only the local
health, aggregate decision counters and filtered threat status, plus local
Tailscale status. It reports whether a token file exists without opening it.
It does not export alerts, app answers, page contents or tokens. HTTP redirects
are refused. Tests: `python -m unittest discover -s scripts/tests -p test_device_readiness.py`.

On 2026-11-02 the probe ran on Windows and macOS. Both daemons answered health;
Windows returned 404 for /intel and /wardens/threat, while Mac reported the
ThreatWarden pane locked. Both token files were absent. A daemon upgrade and
account sign-in are separate gates from agent installation.

Keep the full objective above open until every acceptance row is proven. Respect
the user's latest 20% usage reserve and save a handoff before reaching it.
