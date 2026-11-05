# Store submission contract

Prepare Chrome Web Store and Microsoft Store submission candidates without
claiming an unverified public release. Store packages contain only allowlisted
runtime files, never tests, credentials, developer configuration or user data.

Chrome uses Manifest V3 with bundled code, tab/activity protections, reversible
standalone cleanup and optional desktop reports. Permissions must match real API
use, including tab grouping. Listing/privacy fields must disclose URLs/titles and
interaction flags, including optional desktop/cloud analysis. Public privacy,
support and account URLs must pass HTTPS checks before submission. Synthetic
screenshots must be labeled demonstration data.

Windows uses a standalone offline Inno Setup EXE candidate because the existing
privileged helper/service architecture is not an MSIX conversion. Silent install
must suppress app/browser launches. Per-machine protected binaries and SYSTEM
helper must never run from a user-writable directory. The installer must surface
component failures, preserve user data on uninstall and check every PE signature
before a production submission build. Store certification additionally requires
an immutable versioned public HTTPS download and Partner Center identity.

Do not install candidates over the owner's running app as a packaging test.
Fresh installation/upgrade/uninstall must be verified in an isolated Windows
machine before marking the EXE submission-ready. No signing certificate, payment,
developer agreement or store publication may be fabricated.
