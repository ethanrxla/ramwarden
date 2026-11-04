# Guided setup, model routing, and private device access

Status: implementation in progress. The browser setup guide is implemented;
GitHub device sign-in requires operator configuration and end-to-end approval.
The native installer now installs a bundled payload; the disk image is being built. General file/terminal tools are not shipped.

## Setup

`/setup.html` presents account connection, installation/update, browser extension,
customization, and a final copyable agent request. Its checks are explicitly user
notes, never simulated installation verification. Each device approves a distinct
code and receives a distinct token. Plus permits 3 computers and Pro 5; the server
remains the authority. The request includes no credentials or reusable install link.
Existing installations must update in place and keep user data.

## GitHub identity

Set `GITHUB_CLIENT_ID` to a GitHub OAuth app with Device Flow enabled. The flow
requests no repository scope and does not need a client secret on the desktop.
Routes: `/v1/auth/github/config`, `/start`, and `/poll`. Server-side one-use tickets
are random, bounded to 128 outstanding, and expire within 15 minutes. Polls respect
GitHub's interval/slow_down. GitHub numeric user ID is the identity; mutable names
and email addresses never silently merge accounts. Linking requires the original
signed-in account. GitHub access tokens are used only to fetch `/user`, not stored.
This sign-in proves identity; it does not grant Tailscale or shell permissions.

## Tailscale architecture

Verified 2026-11-03: this Mac has a healthy Tailscale connection and existing SSH
access to RXLA_PC_1. This is developer access, not a shipped customer integration.

Default customer setup uses outbound HTTPS to the hosted Warden API, with per-device
credentials, account ownership, and server-enforced seats. It requires no VPN.
Customers who want direct PC-to-PC workflows keep devices on their own tailnet.
Do not join customers to the operator's personal tailnet. A provider-managed
network, if offered later, needs a separate service tailnet with deny-by-default
per-tenant policy, lifecycle revocation, audited grants, and no customer lateral
access. GitHub sign-in does not discover or authorize a customer's Tailscale account.
Use Tailscale's supported OAuth authorization independently if such a connector is
built; never distribute an operator auth key or request a full tailnet admin key.

## Conversation and tools

Keep the original request immutable. A bounded structured interpretation can add
an intent, complexity, requested resources, missing details, and proposed steps.
It must not add goals, permissions, paths, hosts, or irreversible actions. Unclear
targets require clarification. Conversation: Grok; routine plans: gpt-6-luna;
multistep plans: gpt-6.1-terra; complex work: gpt-6-sol. Exact configurable model
IDs must be available to the operator account. Sales stays on Luna. A cheap local
first pass avoids paying for Grok just to classify every request. Classifications
are advisory and never authorize a tool or override confirmation requirements.

The existing executor still supports its bounded memory actions only. General
file, network, and terminal workflows need a typed tool registry, time/output
limits, cancellation, approved target scopes, and confirmation tied to the exact
plan before executing. No arbitrary model-produced shell string should execute.

## Activity UI

Show playful labels only while a request is active, alongside truthful state.
Waiting/thinking is not proof of executing a command. Do not show fake progress
percentages. Tool blocks will separately identify planned, awaiting approval,
running, succeeded, failed, and cancelled actions. Stop all timers when idle/hidden.

## Nyquest

Inspected reference commit 2c31d1799567abdea4b97f7ad5c58bccd7117677.
The owner authorizes use of this proprietary repository; do not relicense or copy
its source into RamWarden's MIT distribution. Run as a required hosted sidecar for long requests.
`POST /v2/compress` accepts a chat-shaped object and returns `messages`, `model`,
`original_tokens`, and `optimized_tokens`, without calling an upstream model.
Its source documents a safe policy that preserves code and disables user/tool
compression, semantic condensation, structural conversion, and context pruning.
Long requests (8,000 serialized input bytes or more) must use the Nyquest sidecar. Customers have no bypass setting. Only an operator internal-test deployment may select the control A/B arm. A proxy timeout refuses the hosted call instead of silently sending the long prompt directly.

Compression savings vary; do not advertise the repository's percentages as a Warden
measurement. Benchmark billing tokens, latency, and task success on real fixtures.
Never compress confirmation records, exact commands, signed payloads, identifiers,
paths, wallet addresses, tool schemas/results, or the original current request.
Prefer bounded narrative summaries; reject malformed/increased-size responses
and refuse long hosted calls on timeout. Keep sidecar credentials server-side.
Nyquest now runs privately on Oracle; the November 3 validation below records the development tunnel and measured fixture.

References:
- https://docs.github.com/en/apps/oauth-apps/building-oauth-apps/authorizing-oauth-apps
- https://tailscale.com/docs/features/access-control/acls
- https://tailscale.com/docs/features/oauth-clients
- https://developers.openai.com/api/docs/models/gpt-6-luna
- https://developers.openai.com/api/docs/models/gpt-6.1-terra
- https://developers.openai.com/api/docs/models/gpt-6-sol
- https://github.com/Nyquest-ai/nyquest-rust-fullstack

## November 3 validation

The private Oracle Nyquest 3.2.29 service is running on loopback only, with
384 MiB maximum memory and half a CPU core quota. No public proxy port was
opened. A synthetic compression fixture measured 3,902 to 3,326 input tokens;
its immutable policy and exact file request were unchanged. This is one fixture,
not a production savings or task-quality guarantee. An SSH tunnel provides
loopback access for Mac development; production's API and sidecar should be
co-located and need no customer tunnel or VPN.

The preview DMG contains the setup app, Applications shortcut, daemon, account
helper, menu app, browser extension and default configuration. A staged installation
test preserves existing config, secrets and history. It does not run live launchd
jobs in the test. Public distribution still requires completed Developer ID signing
and notarization; the local preview is not a public release.

## Companion and agent boundary (November 3)

RAM and security now share a native companion base and conversation transport, with separate profiles and server personas. The security companion can explain its measured Wazuh report and recent rule alerts; it does not inherit RAM action proposals. The browser extension has separate RAM and ThreatWarden panes and opted-in standalone Lite unloading. This is not a general terminal agent yet.

The next Warp-style implementation must add a typed tool registry and streamed plan/tool/result events before granting file or terminal powers. Preserve original requests and exact arguments through routing and Nyquest. Use Grok for conversation and the configured Luna/Terra/Sol planner for tool workflows, with Grok selected for the operator’s current local conversation tests.

Start with read-only diagnostics and a workspace the user explicitly chooses. File changes preview exact paths and contents; terminal tools use program/argument arrays and a bounded allowlist, never free-form shell text. Every mutation consumes an expiring, single-use confirmation bound to the exact tool, arguments and observed target. New tool output, file contents, browser text and security logs are untrusted observations, not authorization. No elevation, file deletion, network exposure, quarantine or arbitrary command execution is enabled by persona or model selection. Keep cancellation, time/output limits and an audit trail in the executor. Shared UI code is not a shared permission grant.

## Companion notifications and recall

The Mac companions now use a clipped, bundled goat call for rare notifications,
with a shared fifteen-minute cooldown and independent mute controls. Memory must
stay at 90% for ten polling samples; a review needs at least eight tabs; security
notifications require a recent level-12-or-higher rule alert after initial polling.
A higher pitch marks 95% memory or level-15 alerts. These thresholds are notification
policy, not a claim that an alert proves malware. Hiding a companion stops audio.

Each pet keeps up to 200 local conversation turns. Records older than thirty days
are excluded and pruned when loaded or saved. Files are private to the OS user;
settings can clear each pet's history. Before inference, a bounded keyword search
adds at most three relevant older turns alongside four recent turns. This is local
retrieval, not a vector index or automatic access to other applications. Remote
memory hub integration remains separate; do not silently join a vendor/personal
hub or upload customer transcripts.

For local development, the operator selected Grok 4.6 for conversation. Routed
long requests now invoke the required Nyquest compression adapter before provider
inference. Usage logs include model, token counts and provider-reported cost ticks,
never request text, credentials or retrieved memories. Sales models are unchanged.

## Scoped memory-hub retrieval contract

When explicitly enabled in the local assistant configuration, query the user's
configured private Tailscale memory hub before each pet reply. Require a project
scope; never default to another customer's hub or global history. Queries contain
only the current message, not live process/security snapshots. Bound latency and
response size, disable redirects, reuse HTTP connections, and continue normally
when the hub is unavailable. Recalled text is untrusted evidence, not instructions,
permissions or proof an action happened. Cloud opt-in also governs sending recalled
text to a hosted model. Return source titles so a pet can identify past-session
context and distinguish it from current observations. This connector reads only;
it does not upload conversations or mutate the hub.

## First executable workspace tools

A user explicitly chooses a workspace directory in companion settings. The daemon
holds a directory capability, not a model-provided unrestricted root. File tools
list directories, read bounded UTF-8 text, create a new text file, or create a new
directory. They never overwrite or delete existing files. Reject absolute paths,
parent traversal, sensitive credential filenames, and symlink escapes. Each call
requires its own exact, expiring, single-use confirmation, including read operations.

Initial terminal diagnostics are fixed argv templates (Git status and diff summary),
not a shell. Clear inherited credentials from their environment, disable Git hooks,
fsmonitor and external diff, suppress pagers, bound output and runtime, and report
the actual exit status. A tool request does not authorize a chain of follow-up tools.
Desktop arrangement continues using the existing separately reviewed planner and
single-use plans; unsupported desktop backends must report unavailable.

## Manual and Auto companion modes

Manual remains the default. Auto is a user-selected native setting, explicitly
acknowledged for the currently opened workspace capability. Auto may consume a
proposal without another dialog only when the server marks its typed operation
compatible with a direct instruction in the CURRENT message and the proposal's
scope ID matches the user's saved Auto approval. New daemon capabilities, changed
folders, and disconnection require Auto to be enabled again. Past-session text,
model-supplied scope IDs, and inferred approval never enable Auto. Memory/security
controls and desktop arrangements retain their existing confirmations. Auto is
not arbitrary shell access, elevation, or a bypass of filesystem boundaries.

## Responsiveness verification

The November 3 live check found macOS service-wide Background scheduling delayed
loopback health/status responses by about 2.7 seconds. Removing that service-wide
throttle returned status in about 0.9 milliseconds in the immediate check; retain
the paced monitoring thread and background account sync. UI status updates must
not wait for speech/security polling. Local recall reads/writes run on a utility
queue; render chat history lazily and avoid resizing unchanged windows. A native
profile showed a 39.9 MiB pet footprint and an idle main thread at sampling time;
this is one observation, not proof of every reported hover stall being resolved.
