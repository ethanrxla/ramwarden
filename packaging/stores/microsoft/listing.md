# Microsoft Store — offline Win32 EXE candidate

Name: RamWarden (availability/reservation unverified)
Category: Productivity / utilities (confirm Partner Center category)
Language: English
Architecture: x64
Minimum Windows version: Windows 10 build19041; validate Windows10 and11 before certification.

## Description

For people who never close their tabs.

RamWarden helps you understand memory use and give idle browser tabs a rest.
Connect the browser extension to see tab activity and manage eligible idle tabs.
Choose your settings, keep active work protected, and reopen released tabs.

Optional desktop plans add companion features. ThreatWarden reports security
engine health and events when its modules are installed and available. Alerts
are observations to review, not a guarantee that a device is threat-free.
Accounts and paid plans are managed on the Warden website. Signing in does not
make a payment; any card or wallet checkout is a separate explicit action.

Only advertise floating pets, voice, full security coverage or other workflows
in the final listing after the actual Windows release build passes their tests.
The current parity/readiness checklist remains a release gate.

## Partner Center package fields

- Format: standalone offline EXE; no setup-time downloads.
- Package URL: must be the ACTUAL immutable versioned HTTPS installer URL returned
  by the chosen release host. Not assigned yet. Never submit a draft release,
  localhost URL, “latest” redirect, placeholder URL or replace bytes at a version URL.
- Silent install parameters: /VERYSILENT /SUPPRESSMSGBOXES /NORESTART /SP-
- Recommended diagnostic parameter: /LOG="<path chosen by tester>"
- Silent uninstall: generated unins000.exe /VERYSILENT /SUPPRESSMSGBOXES /NORESTART
- Expected success exit:0. Any other exit is failure/reboot-required until mapped
  and verified against the selected compiler/version and Partner Center handling.
- UAC is expected for protected Program Files binaries and the local helper service.
- Interactive setup: license, optional desktop shortcut, optional Open RamWarden.
  Silent setup must not open the tray UI, browser, sign-in or paid enrollment.

## Build and trust gates

Use scripts/package-windows.ps1 with the complete reviewed Windows release
payload. Production mode checks the signatures on every payload EXE and refuses
missing entitlement/update trust roots. Supply the external Inno signing command
so both installer and generated uninstaller are signed. It emits a checksummed
submission-report.json; a build alone never marks storeReady true.

-AllowUnsignedCandidate is for internal packaging validation ONLY. Its filename
contains unsigned-candidate. Never upload it to the Store or pass it off as a
trusted installer. No trusted Windows signing certificate was found on the
owner's PC in CurrentUser or LocalMachine stores at this checkpoint.

The Windows SDK signing tool and an official Inno compiler are available on the
PC. Signing enrollment/account/permissions remain owner-provided prerequisites.
No certificate purchase or enrollment has been performed.

## Required release verification

- Partner Center developer/publisher verification and product identity/reservation.
- Valid trusted signatures on every PE, generated uninstaller and installer;
  timestamp and revocation checks. Inspect all runtime DLL dependencies; no web
  bootstrapper may fetch them during Store setup.
- Fresh standard-user/admin installation, silent installation with no UI except
  UAC, reboot/login, existing-version upgrade, denied elevation, failed install,
  repair and uninstall. Preserve user settings/tokens/history securely. Test UAC
  with different administrator credentials; do not link the wrong user's account.
- Actual packaged EXE/Start-menu/taskbar icon and version metadata, not only a tray icon.
- No duplicate daemon/tray; local helper remains in protected Program Files and
  the user daemon/tray remain non-elevated. Verify service/task cleanup on uninstall.
- No automatic telemetry upload or account/payment enrollment without the configured
  consent/approval flow; required privacy/terms/notices, screenshots and support URL.
- Full Windows feature/parity acceptance, Defender/Security checks, model quotas,
  clean artifact scan, staged rollout and update/rollback validation.

Official requirements checked 2026-11-05:
https://learn.microsoft.com/en-us/windows/apps/publish/publish-your-app/msi/app-package-requirements
https://learn.microsoft.com/en-us/windows/apps/publish/publish-your-app/msi/upload-app-packages

Status: submission preparation, not a submitted or certified app. Production
signing, public HTTPS hosting and clean-machine lifecycle evidence are mandatory.
