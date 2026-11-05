# Installer agent pairing

The installer offers guest agent pairing without a ScholarCal login. Optional
ScholarCal connection opens its account settings for calendar skill access only.
The daemon owns a persistent private identity. Local owner actions register it,
create a ten-minute one-use invitation, accept an invitation, and review/approve
or revoke peers. Creating an invitation is not granting file or command access.
Incoming peer content never executes tools automatically.

The loopback API uses the existing Host/Origin guard. Identity disk operations
run off the async executor; HTTP requests are bounded and never run on the UI
thread. No background messaging polls run during installation. Private keys
remain local; invitations are never included in diagnostics. A failed or disabled
hosted relay shows an actionable error, never a fabricated pairing key.

Acceptance: identity persists, retries preserve ciphertext, tampering fails;
installer shows pending peers and requires an explicit confirmation; guest flow
never opens ScholarCal login; offline status check does not contact the relay.
Hosted migration, shared abuse-control secret and signed interoperability test
must pass before enabling production pairing.


Validation on 2026-11-05: native installer compiles; three daemon route tests,
four identity/encryption tests and clippy pass. The real Rust client completes
registration, invitation redemption, inviter approval, encrypted messaging,
idempotent retry and revocation against the hosted service implementation with
PostgreSQL/PGlite. This is local interoperability evidence, not a production test.

Mac app placement is explicit: drag the app from the disk image to Applications,
then setup verifies its executable matches this payload before installing daemon
components. The app and installer remain separate. Rollback tests pass. Setup
records only operation type, duration and success; no invitation or peer key is
written to diagnostics.
