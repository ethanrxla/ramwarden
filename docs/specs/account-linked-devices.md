# Account-linked RamWarden and ThreatWarden rollout

## Objective and acceptance evidence

The end state is working RAM and threat coverage on the developer's Windows PC
and MacBook, both owned by one account, then a repeatable customer setup across
Windows, macOS and Linux. This document records incomplete work; it is not a
claim that installation or a green test proves coverage.

| Requirement | Evidence needed before completion | Current evidence / remaining work |
| --- | --- | --- |
| Developer account and both devices | Verified email/wallet sign-in, two distinct device tokens, same server account, valid Pro entitlement | Live email sign-in verified on 2026-11-03. Account page reports Lite, zero devices, and existing email/wallet/GitHub identities. Neither device is linked yet; Pro entitlement remains pending. Keep personal identifiers out of repository files. |
| Wazuh on both devices | Manager lists both agents as connected; harmless synthetic events arrive under the correct account | Manager agents 001 (Windows) and 002 (Mac) confirmed active on 2026-11-02; records from both arrive. Neither is yet proven bound to the developer account. |
| ClamAV | Current signatures, scan-only execution and safe test detection delivered through Wazuh on both devices | Windows scan records reach the manager; Mac installation not found in Homebrew. Keep Defender enabled. No automatic deletion/quarantine. |
| Suricata | Capture works on intended interface; safe fixture produces an IDS alert through Wazuh | Windows alerts reach the manager; Mac installation not found in Homebrew. Check Npcap availability/licensing on Windows. |
| App/extension sign-in | Extension opens wallet/email device approval; daemon alone stores its device token; both show the same account | Bridge and popup implemented in PR34; real Chrome popup flow passes with a mocked backend. Live email approval and installation remain. |
| Tailscale integration | Account-bound discovery and selection, authenticated pairing, cross-account denial, revocation | Both existing host clients are online. No RamWarden integration yet. |
| Licensing and database | Atomic 3-device Plus / 5-device Pro reservation, concurrent enrollment/replay/revocation tests | Registration writes the account and counts/inserts devices in one transaction, using the shared active-plan cap. Memory and isolated Atlas concurrency contracts pass at c1eaf03. |
| Request limits based on real use | Representative measured request totals, costs, restart handling and documented Plus/Pro sizing | Mac reports 4 calls in preceding hour, 1724 input / 272 output tokens since daemon startup; not a daily baseline. Do not set quotas from this sample. |
| Personalized behavior | Account-scoped learning data, device/OS identity, bounded feedback, isolation and offline behavior tests | Local activity protections exist. Cross-device account learning is not verified. |
| Repeatable setup | Versioned installation/enrollment/rollback steps, a fresh-device reproduction, optional setup skill | Read-only readiness probe added; full setup runbook/skill remains. |
| Hosting | Existing Oracle resources assessed, manager/API/indexer health and capacity verified | Oracle manager/indexer active; about 7.8 GiB available memory and 71 GiB free disk on 2026-11-02. Hosted app API is not deployed there; no droplet needed for current capacity. |
| Sales/support | Verified sign-in, truthful plan answers, chat-ticket-reply-revisit behavior | Support PR23 browser tests pass. Real email delivery and sign-in verified against the Mac preview API on 2026-11-03; this is not a production API deployment. |

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
the user's latest 20% five-hour usage reserve and save a handoff before reaching
it. The weekly allowance is a separate limit, not the requested reserve.

## Local account bridge (PR34)

The loopback daemon exposes `GET /account`, `POST /account/start`,
`POST /account/refresh` and `POST /account/disconnect`. Existing Origin/Host
guards apply; none of these routes exists on a network listener. The extension
also refuses account operations when configured for a remote daemon.

Start returns the public approval URL and user code only. A daemon task polls
the configured hosted API, respects its interval and the ten-minute expiry,
and completes even if the popup closes. It stores `device-token` and
`account.json` in the daemon's configuration directory using atomic replacement
(0600 on Unix). Windows uses the daemon account's configuration directory;
a readiness probe running as a different user cannot establish whether its
credential exists. Credentials are never returned to the extension.

`entitlement.json` remains beside the daemon history database. Refresh verifies
the compiled signing key, account, device and issue time before replacing it.
An unauthorized response removes the local credential and plan; a temporary
outage preserves a previously verified plan until its own expiry. Disconnect
also cancels pending approval locally. It does not revoke the server device or
free a slot; the account page does that. A failed write after a one-use device
claim tells the user to remove the newly registered device before retrying.

Still required for live coverage:

- Configure the API entitlement signer and matching public key in app builds.
  The current Mac local API has a Resend provider but no entitlement key in its
  process environment. Real email delivery and sign-in are now verified on
  the Mac preview, but entitlement signing remains unverified.
- Implement the hosted `/v1/wardens/threat/enroll` and `/status` handlers and
  account-scoped agent mapping. Currently those paths exist only in the client
  and fixtures, so a signed plan alone cannot provide hosted threat status.
- Validate a live sign-in and signed plan on both devices, then deploy verified
  builds with rollback artifacts and preserved user settings.

Validation at 2d3ebb4: 188 daemon unit tests, account/warden route checks and
daemon all-target clippy pass on Mac; 22 extension unit tests and the real
Chrome popup flow pass. The popup uses mocked backend responses and a disposable
browser profile. No production account, payment record or installed app was
changed by these tests.

### Atomic enrollment

Registration writes an account version inside a MongoDB transaction before
counting and inserting devices. Concurrent registrations for that account
therefore conflict and retry, instead of both counting the same free slot.
The plan is read in that transaction; billing writes to the same account also
participate in conflict detection. `Account::device_capacity` supplies the
Lite/Plus/Pro limits of 1/3/5 and honors a lower stored operator cap.

The driver retries transient transaction failures and ambiguous commits; the
device identity and token stay constant across internal retries. An unsupported
or unavailable transaction fails without falling back to the previous unsafe
count/insert sequence. Existing devices are counted directly, so no counter
backfill or production migration is needed. Revoking a device frees its slot;
an expired plan prevents additions without deleting existing devices.

The store contract races ten registrations for each tier, races two replacements
after one revocation, checks revoked tokens and cross-account refusal, and
checks expiry. Run it against the isolated Atlas test database as well as memory.
Reference: [MongoDB Rust driver transactions](https://www.mongodb.com/docs/drivers/rust/current/crud/transactions/).

Validated on the Mac at c1eaf03: all 76 API and 21 cloud library tests pass;
API/cloud all-target clippy passes with warnings denied. The isolated Atlas
contract passes (26.77 seconds), including concurrent 1/3/5 device caps,
revocation, expiry, support retention, legacy index compatibility, repeated
index initialization, and decision-sync idempotence. The test uses a fresh
`ramwarden-test-*` database and removes its test collections afterward.
No application database migration or billing change was needed. Full CI is
green at 6c31773 (run 41576589262); this is source validation, not deployment.

### Agent ownership storage

The `threat_agents` collection reserves one Wazuh agent for one registered
device and account. Its primary key is the device ID; a unique index on
`deployment, agent_id` prevents two API instances from assigning the same agent
to different devices. The deployment identifier comes from operator
configuration, not a URL or hostname supplied by a customer. No agent key,
alert content, email address or browser data belongs in this record.

The store operation is for a server that has already verified the Wazuh agent's
ownership. It must not become a route where a customer can claim an arbitrary
agent ID. Existing developer agents require explicit verified mapping; matching
their hostname alone is insufficient. Enrollment must revalidate the device
token before handing out an agent key.

Bindings are immutable and retryable. Revocation removes a device from the
active binding query immediately on subsequent reads, while retaining the
reservation so its historical alerts cannot silently move to another account.
This reservation has no TTL. Any future operator reclamation must first remove
the agent credentials and address historical indexer records. This storage
change does not itself revoke Wazuh access, implement the hosted endpoints,
bind either existing developer agent, or deploy indexes to the application DB.

The memory contract and isolated Atlas contract (28.80 seconds) both pass with
these bindings. They race two accounts for the same agent, reject foreign-device
claims and binding replacement, verify idempotent retries and original timestamps,
hide revoked devices, preserve reservations, and allow the same numeric agent ID
on a separate deployment. All 21 cloud library tests and API/cloud all-target
clippy pass on the Mac. The next implementation step is a server-controlled
enrollment/status handler using these bindings and live device authorization.

### Live email sign-in checkpoint (2026-11-03)

The user received the real sign-in email and supplied its code. The account
page authenticated successfully and reported Lite with zero registered devices.
No plan was granted, payment made, or device credential installed. A GitHub
identity added by newer backend work was incorrectly rendered as an Ethereum
wallet with an undefined address. The frontend now labels identity kinds
explicitly in both the account summary and sign-in-method list.

All 23 website unit/site tests and focused identity-label checks pass locally.
An unrelated POSIX installer execution test fails on Windows; it must be run
on a supported host before claiming a full installer-suite pass. The Mac went
offline after sign-in, before the preview patch could be copied or applied;
live confirmation of the label fix and device linking remain pending. The
signed-in browser session is preserved for continuation. Do not save email
codes, sessions, or personal install links in this document or shared memory.
