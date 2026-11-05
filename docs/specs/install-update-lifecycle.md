# Desktop installation and updates

## Contract

macOS distributes a signed/notarized DMG containing RamWarden.app and an
Applications shortcut. Drag the actual app to Applications, open it, and use its
first-run setup to install/update bundled monitoring components. Offline setup
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
Sharing is off until the account owner opts in through account settings. Setup
shows what is collected and links to the actual consent control; no checked
box or account login silently enables uploads. No process names, URLs, chat,
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

Native Mac source builds with first-run setup in the actual app, persistent chat
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
