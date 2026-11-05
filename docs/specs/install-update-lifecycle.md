# Desktop installation and updates

## Contract

macOS distributes a signed/notarized DMG containing separate RamWarden Installer.app
and RamWarden.app bundles plus an Applications shortcut. The installer handles
license, account approval, required modules and component updates. RamWarden.app
is the everyday companion/menu-bar app and does not reopen the setup walkthrough. Offline setup
is supported; account connection and telemetry consent are separate. Windows
ships an interactive installer with optional desktop shortcut and a final Run
RamWarden choice. Existing configuration, credentials, identity and history stay.

The launcher exits between scheduled checks (six-hour cadence), stores a small
local status, and offers a visible Check for updates / Download update action.
Installing remains a deliberate user action with normal OS privilege prompts.
No remote script or unsigned update manifest may authorize code execution.

## Verification and update policy

Update metadata uses a dedicated pinned Ed25519 public key, distinct from the
entitlement key. Sign exact manifest bytes in a versioned envelope. Require a
fresh stable-channel manifest, valid semantic version newer than the installed
version, one artifact for the current target, HTTPS on approved release hosts,
fixed safe installer filename, bounded size, and a matching SHA-256 after download.
Verify again when consuming cached metadata. Fail closed for missing keys,
expired/invalid signatures, duplicate targets, redirects or mismatched bytes.
Keep last verified status distinct from failed/offline checks. Never claim a
missing release feed means the installed build is current.

The update button downloads a verified installer and opens it. It does not
silently replace an app or approve an admin prompt. Mac users quit the UI, drag
the new app over the old app, then open it to update bundled components. Daemon
and user data are preserved. Release publishing must finish signing/notarization
and install checks before signing and publishing its update manifest.

## Telemetry

Reuse existing device-authenticated numeric diagnostics and 90-day retention.
The installer presents a checked usage-sharing choice directly above the privacy
link. Users can uncheck it. Only confirming the account step saves that choice;
signing in alone does not enable uploads. Device-authenticated consent affects
only this computer, and the account-level off control revokes all device choices.
An offline choice stays pending locally and blocks uploads until synchronized. No process names, URLs, chat,
file contents or credentials are usage telemetry. Update checks work without
usage-sharing consent. Installation health checks are local observations, not
proof every security engine or account feature is operational.

## Verification

Test manifest tampering, expiry, downgrade, duplicate targets, unsafe URLs,
size/hash mismatch and missing trust key with synthetic fixtures. Test payload
installation against a temporary home, preserving config, secrets and history.
Build native setup/app and Windows installer; public release requires verified
platform signatures. The attached ScholarCal Agent Network guide is future
integration context, not permission to register identities or enable networking.

## Current release assessment (2026-11-04)

Native Mac source builds with a separate setup installer and companion app, persistent chat
mute controls, and a six-hour update check while the app is running. The update
helper verifies signed metadata and streams bounded downloads to disk. Missing
trust configuration is an explicit error, never an up-to-date claim.

Public deployment is blocked until the dedicated update trust key is pinned in
release builds, a signed release feed is published, the new complete Mac image
is tested and notarized, and the Windows wizard and signing path are verified.
The previously notarized disk image does not certify these new changes.
Production API hostname resolution failed from this Mac during this check;
localhost:8000 auth configuration returned HTTP 200. Account flows must be
verified against reachable production DNS/HTTPS before launch. Local monitoring
continues without hosted sign-in.

The reviewed workspace catalog supports bounded existing-text edits with an
exact expected-content check, in addition to reads/new files/Git diagnostics.
This does not yet provide arbitrary terminal or desktop automation.

## Companion controls and action scope

Each chat header exposes one-hour, twelve-hour and indefinite quiet choices and
an explicit Unmute action. RAM quiet state also synchronizes its daemon comment
policy; per-companion audio gating covers replies and rare baa notifications.
ThreatWarden exposes Settings from its own pet context menu. Timed local audio
quiet expires when audio is next requested, without a per-pet polling timer.

Security chat treats unreachable feeds as unknown, never as zero alerts. Short
security-status questions request evidence; casual banter does not attach it.

Release builds of the daemon must include RAMWARDEN_ENTITLEMENT_PUBLIC_KEY.
Omitting it correctly rejects signed paid passes, but makes existing Pro users
look locked. The Nov 4 local rebuild omitted it; the daemon was rebuilt with the
configured public key and `/wardens/threat` returned unlocked:true again.

## Transactional Mac component installation

The setup window stages LaunchAgent definitions in a private temporary directory;
it does not overwrite live definitions before the installer starts. The installer
backs up existing binaries, app/extension and definitions, stages replacements,
and stops jobs before swapping. Any failure after swapping restores the saved
components and prior definitions, then attempts to restart the previous jobs.
Definitions containing operator credentials are staged and installed with mode
0600; backup directories use 0700. Existing config, credentials and history are
never reset. Synthetic temporary-home success and forced-failure tests cover
both installation and restoration. Setup cannot finish until installation succeeds.

Packaging checks that daemon/updater binaries contain their configured public
trust keys. Signing private keys remain outside the app and repository.

## License acceptance

The workspace declares MIT licensing. Bundle its standard text as LICENSE.txt
and show it as setup step two, before account connection and installation.
Users must scroll to the end before the unchecked acceptance control becomes
available. Continue, later-step navigation and installation remain gated until
acceptance. Store only the accepted text SHA-256 and timestamp in local defaults;
a changed license prompts again. Never accept the agreement automatically for
a user. Hosted service/payment terms and usage consent remain separate.

The development Mac's LaunchAgent explicitly selects localhost:8000 for setup
sign-in until the production service exists. This override is private local
configuration; distributable builds retain HTTPS production defaults and accept
only their trusted approval origins.

## Diagnostics and required modules

Setup emits bounded, fixed-schema local events for steps, license confirmation,
account connection, installation, health checks, sharing choices and completion.
Logs rotate at 1 MB and contain no account identifiers, URLs, tokens, commands,
private keys or chat. A Show diagnostic log action opens the local file. These
setup events are not yet uploaded to the hosted service; existing numeric usage
summaries remain the data sent under the sharing choice.

Core binaries are bundled. Optional Pro setup checks the Sentinel and security
packages, enrolls a missing Sentinel through the existing verified installer,
installs missing Antivirus/Threat detection dependencies through the supported
package manager, then configures endpoint services after OS administrator
approval. An absent package manager opens its vendor's signed OS installer after
size/hash/signature checks. Completing that separate OS approval and retrying is
required. Existing configured monitoring is not restarted merely to inspect it.
Fresh-host end-to-end module setup remains unverified; do not label it production
ready based solely on this Mac's already-installed modules.

## Agent pairing

The installer links to ScholarCal's existing signed-in agent-pairing page, which
creates a copyable short-lived invitation. It never fabricates a pairing secret
or copies an account API key. A native Warden identity/receive connector and a
browser-to-desktop pairing handoff remain unfinished; a ScholarCal platform-agent
invitation alone is not proof the local Warden can receive A2A tasks.
