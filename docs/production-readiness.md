# Production readiness

Working checklist, November 5, 2026. A checked item has evidence below; all other items are release gates. A working developer installation is not proof that a new customer can install, sign in, update, recover and uninstall successfully.

## Finish the two-device milestone

- [x] Windows and Mac linked to the developer Pro account (account page showed2of5; November4 account handoff).
- [x] Mac Warp exact create/search/read workflow verified through real cloud chat, manual approval, streamed results and replay409 on fcbc297. Failure case preserves completed work and refuses overwrite; later writes never run.
- [x] Final Mac release workspace tests18 pass; native app and fixture harness compile. Backups and entitlement verification key preserved.
- [x] CI41993576330 all5checks passed; PR57 merged into development PR54 as a211b6e. Mac source fast-forwarded; all5 concurrent source files preserved by hash.
- [ ] Recheck Wazuh001Windows/002Mac and each device's local ClamAV signatures/scans, Suricata capture/drop counters and network view. Record timestamped per-engine evidence, offline/error states and a benign detection fixture. An unlocked pass or zero alerts does not prove coverage.
- [ ] Upgrade Windows native UI: pixel ram tray icon, RAM/Threat panes, companion/chat access, account state. Current installed tray is the old memory gauge. Test with the existing Pro sign-in, preserving settings and rollback.
- [ ] Measure real model request rates/costs on both machines; establish Plus/Pro quotas with enforced account budgets. Do not infer a quota from a short sample.
- [ ] Verify account-scoped learning and reopen/Keep feedback across restarts and supported platforms. Tailscale discovery must never grant entitlement or cross-account access.

## Release source and CI

- [ ] Reconcile Windows, macOS and Linux branches into a reviewed release candidate; retain evolved Mac UI and all safety regressions. Resolve PR18 rather than silently abandoning its Linux/Windows panes.
- [ ] All required checks pass on the exact release commit: Rust tests/Clippy, Windows native build, Linux GTK/xvfb, Intel and Apple Silicon Swift builds, browser/extension tests, installer rollback tests.
- [ ] Fix current CI failures and map historical failed runs to their superseding passing commits. Formatting is currently advisory; choose a separate formatting change before enforcing it.
- [ ] Pin dependency/toolchain versions, audit dependencies and licenses, generate SBOM and reproducible checksums, scan packaged artifacts, and exclude keys, tokens and developer data.
- [ ] Threat model and review privileged helpers, local API origins, extension commands, signed entitlements, tenant isolation and tool approvals. Test revoked/expired passes and offline operation.

## Public site, account API and operations

- [ ] Inventory Vercel project ram-warden/ramwarden, Namecheap DNS for ramwarden.com, MongoDB deployment and Oracle services using stored credentials without logging secrets.
  Current access finding: Vercel connector403 for the team; consent recovery404. Existing browser tabs are present, but tab binding fails. No hosting/DNS change attempted.
- [ ] Separate development/staging/production environments and databases; host the account API independently of an awake Mac and developer SSH forwards.
- [ ] Validate a production preview before assigning ramwarden.com/www. Preserve mail/security DNS records; use Vercel's actual returned verification/DNS values. Verify TLS, redirects, CSP/CORS, cookies, auth callback URLs and canonical links.
- [ ] Real email and wallet sign-in, account recovery, device linking/unlinking, quotas and revocation tested end to end. Never ship developer entitlements or hard-coded local API endpoints.
- [ ] Verify plan definitions, checkout/webhook signatures, idempotency, refunds/cancellation and entitlement transitions in staging. Make paid deployment/infrastructure changes only after a concrete reviewed rollout is ready.
- [ ] Monitoring, structured secret-free logs, error alerts, health checks, backups with a restore drill, incident response, staged rollout and a tested rollback.
- [ ] Publish accurate privacy/support/terms, telemetry controls, data export/deletion and third-party notices. Product claims must match measured platform behavior.

## Database, sales reps and support

- [ ] Verify MongoDB schema/versioning, least-privilege accounts, indexes, bounded requests, timeouts and backup retention. No billing-record changes during index checks.
- [ ] Confirm support chat30day and ticket1year-after-last-message expiry in application code and TTL indexes. Test isolated database contract suite and chat→ticket→staff reply→revisit; unavailable Atlas/Jev, rate limits and account isolation.
- [ ] Sales/support wardens use current authoritative product/plan answers and write/read the correct visitor/account conversations and tickets. Staff routes require authenticated staff scope; AI cannot invent completed support actions.
- [ ] Implement useful vector retrieval for approved product/support knowledge and opted-in per-account learning: documented embedding model/version/dimensions, bounded ingestion/retries, update/delete propagation, relevance evaluation and keyword fallback.
  Existing `ramwarden-cloud::decisions` has feature/text vector indexes and a caller-supplied filter for personal decision recall. This is not yet proof of tenant-scoped hosted sales/support retrieval; audit it before production reuse.
- [ ] Vector queries pre-filter tenant/account, access scope and non-expired documents before ranking; test adversarial cross-account queries, stale content and deletion. Vector similarity is not authentication or permission to close tabs/run tools. Keep private URLs/page contents out of hosted support storage.
- [ ] Measure vector/LLM latency, storage and cost under representative load; set budgets and failure behavior before enabling production traffic.

## Platform installers and updates

- [ ] Windows standalone signed .exe installer with product icon/version metadata, per-user app plus narrowly scoped elevated sensor setup, Start menu/uninstaller, logon behavior, silent flags/exit codes, upgrade from the original installed version and repair/rollback. Verify no console flashes or duplicate tray/daemon.
- [ ] Windows desktop exposes RAM, ThreatWarden, both companions and account/sign-in status; a Pro login unlocks features without reinstalling or copying developer credentials. Offline or missing engines report truthfully.
- [ ] Linux separately packaged GTK app and daemon: supported distro/architecture matrix, .deb/.rpm or documented chosen formats, desktop entry/icon, systemd user/service separation, polkit/capabilities only where needed, upgrade/uninstall and Wayland/X11 tests.
- [ ] Mac signed/notarized app/installer for Intel/Apple Silicon, verified update feed, Gatekeeper fresh-download test, permissions, upgrade/repair/uninstall and rollback. Draft54 is not yet release-ready.
- [ ] Signed release metadata, artifact integrity checks, version/channel migration, resumable update failures and a known-good rollback. App settings and account tokens survive updates securely.
- [ ] Clean-machine and existing-install tests on Windows/Linux/Mac, including non-admin users, reboot, offline startup, denied prompts and partial sensor installs.

## Opt-in microphone voice mode

- [ ] Explicit microphone toggle, initially off; OS permission prompt, visible listening indicator, immediate stop/mute and audio-device selection. Never start background listening merely because Pro is unlocked.
- [ ] Local voice activity/end-of-utterance detection: stop after a bounded silence interval, handle pauses/noise, maximum duration, cancellation, denied/disconnected microphone and screen-reader/keyboard access. Wake-word support is a separate decision; no Siri-equivalent claim without testing.
- [ ] Transcription→visible/editable transcript→same companion chat and approval flow. Spoken input cannot bypass tab protections, file/desktop review or privileged setup confirmation.
- [ ] Document local versus hosted transcription, explicit consent and retention, secure bounded audio handling; do not retain raw audio by default. Prevent synthesized voice from retriggering capture; test echo/headset/languages and latency/cost.
- [ ] Test voice mode on all supported operating systems; distinguish existing spoken output/speech bubbles from microphone input, which is not yet shipped.

## Extension and stores

- [ ] Browser extension production manifest/IDs, signed account link to the local app, email/wallet sign-in and logout/revocation, restricted origins/permissions and backward compatibility. Old extensions without fresh activity evidence cannot authorize automatic closure.
- [ ] Regression and real disposable-tab tests: active/background work, reopening twice, Keep persistence, exact versus same-site URLs, stale predictions, missing activity, restart/tab-ID reuse, selected-only approval, dismissal and throttling.
- [ ] Chrome Web Store listing/review, privacy declarations, screenshots, support URL, permission justification, packaged artifact and rollout. Test store-installed native messaging/local connection; plan Edge/Firefox separately where supported.
- [ ] Microsoft Partner Center app identity, publisher verification, signed hosted Windows .exe, silent installer parameters and handling, stable version-specific HTTPS download, screenshots/privacy/support metadata and certification checks. An EXE may use the Store's Win32 MSI/EXE submission path; MSIX is not the only route.
- [ ] Release acceptance: a new user can discover, install, sign in, link allowed devices, see engine health, talk to the warden, receive updates, revoke access and uninstall on supported systems. Run a small opt-in beta before broad release.

## Reference requirements

- Microsoft EXE submission: https://learn.microsoft.com/en-us/windows/apps/publish/publish-your-app/msi/upload-app-packages
- MongoDB tenant-filtered vector search: https://www.mongodb.com/docs/vector-search/deployment/multi-tenant-architecture/
- Vercel custom domains: https://vercel.com/docs/domains/set-up-custom-domain

No public launch, DNS migration, store submission or production database change is recorded as completed by this checklist.
