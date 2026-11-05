# Windows Store readiness

Assessment: **NOT READY**, November 5, 2026. The working developer installation
is not a customer release candidate. The current tray and daemon were inspected
with Get-AuthenticodeSignature: both report NotSigned. The artifact audit expanded this to all six installed
EXEs: daemon, debloat, gpu, service, tray and usage-sync are unsigned.

Use the Win32 EXE submission path requested for RamWarden. Microsoft requires a
standalone EXE/MSI, trusted signing for the installer and all PE payloads, silent
installation (UAC allowed), and an immutable versioned HTTPS installer URL.
Source: https://learn.microsoft.com/en-us/windows/apps/publish/publish-your-app/msi/app-package-requirements

## Blocking gates

- [ ] Build a standalone, offline Windows EXE installer; the current private
  PowerShell upgrade scripts are not a Store installer. Include only redistributable
  product payloads. Verify silent switches, exit codes and cancellation/repair.
- [ ] Sign and timestamp installer, tray, daemon and every PE dependency with a
  trusted publisher identity. Verify signatures after packaging and download.
- [ ] Serve immutable, versioned EXE downloads on public HTTPS with recorded hashes.
  Submit that exact URL and silent-install parameters to Partner Center.
- [ ] Account-authenticated hosted model access with per-account quotas/rate limits,
  billing/plan enforcement, provider timeouts and tenant isolation. Customers must
  never receive developer provider keys. BYOK must be an explicit separate mode.
- [ ] Public account/API services with no dependency on the developer Mac, localhost
  website or private tailnet. Verify sign-in, device linking/revocation, offline and
  expired-plan behavior on a clean PC outside the tailnet.
- [ ] Upgrade/verify the daemon fun-facts feed and cache contract: the current
  developer daemon returns404 for /warden/facts, though /warden/bubble works.
  Verify live proactive delivery, mute and expiry without injecting fixture
  messages into production.
- [ ] A clean install starts one daemon/tray, shows signed-in entitlements and the
  expected companions, independent chat/thought boxes and tested visibility controls.
- [ ] Validate engine installation, least privilege, definitions/rules updates,
  queue/backpressure recovery and benign AV/IDS fixtures. Missing engines must be
  reported truthfully; do not promise protection from an entitlement alone.
- [ ] Upgrade from the original version, preserve settings and account tokens,
  recover from interrupted/denied setup, test reboot, uninstall and data deletion.
- [ ] Partner Center publisher/app identity, supported Windows versions/architectures,
  privacy/support links, age/content ratings, accurate screenshots and disclosures,
  accessibility, certification and a staged beta.

## API-key audit

This developer Windows PC currently uses the owner's provider keys copied from
the approved Mac daemon configuration into restricted per-user storage. They are
not embedded in executables or committed to the repository. Assistant cloud use
is enabled separately from automatic cloud tab analysis. This is not the release
credential architecture. The private directory, routing.json, secrets files and
device tokens must never enter an installer or release archive.

## Reproducible artifact checks

Run scripts/test-windows-store-readiness.ps1 against a staged payload plus the
installer, expected certificate subject and versioned URL. It writes JSON and
fails on missing/unsigned artifacts or obvious private credential files. A pass
still requires all manual acceptance gates above; it never declares Store-ready.
The gate does not authenticate to Partner Center or submit anything.
