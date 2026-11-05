# Vercel services deployment

## Confirmed topology

Use the Services preset with repository root `.`. `website` builds `web/` as an
Nginx container serving the existing static pages. `api` builds the Cargo
workspace's ramwarden-api using packaging/vercel/api/Dockerfile. Desktop daemon,
UI, helper, service and debloat binaries run on customers' computers, never as
hosted Vercel services.

Public routes: `/v1/*` and `/healthz` go to `api`, and all remaining site paths
go to `website`. Requests to api.ramwarden.com route to `api` with their paths
unchanged. Both web and API are public; API routes retain application-level
account/device/staff authentication. No internal Vercel services are presently
needed. There are no service-to-service callers, so there are no bindings:
static browser JavaScript calls the public same-origin `/v1/*` API, not a private
URL. Do not invent a binding merely to fill a configuration field. Future
server-side callers must declare their own runtime URL binding.

The API requires production environment values separately from local dev:
production Mongo database, account origin/public URL, entitlement signer, OAuth,
email, model credentials and webhook configuration. Never copy the operator's
entire .env or device tokens into Vercel. No loopback/Tailscale-only manager
address will work automatically from Vercel. The root Docker context is an
allowlist and excludes private env files, logs and local build output.

## Separate CLI identity

Use `scripts/vercel-ramwarden`, which strips inherited Vercel token/project/team
variables and stores auth in ~/.config/ramwarden/vercel-cli. Expected account
email ethanrisden97@gmail.com; expected team ram-warden. Verify both before any
project link or deployment. This leaves the ordinary Vercel CLI profile alone.

## Production readiness gates

- Correct account/team verification (service names, public routes and absence of bindings confirmed by owner).
- Verify shared GitHub state in deployment; email/wallet/device challenges already use MongoDB.
- Long-running payment polling and shared trivia publishing hosted as durable
  workers or scheduled jobs, not assumed to survive container scale-to-zero.
- Reachable authenticated security-manager backend and account/device isolation.
- Atlas access from the deployment network without opening management systems.
- Production account/payment tests, secrets, HTTPS and rollback verification.
- Signed/notarized Mac and Windows release artifacts published to GitHub Releases
  (only a draft candidate exists; no public release), with checksums and the signed updater manifest.

A preview with isolated development data can precede production. Do not promote
this draft API topology or enable live payments until the gates pass. Vercel
containers are a beta runtime and scale down when idle; local source compilation
is not proof a container deployment works. Docker is not currently available in
this Mac's PATH, so container build/dev validation remains outstanding.

## Verified account and project

Isolated CLI identity verified as ethanrisden97@gmail.com. Created and linked
ram-warden/ramwarden; Vercel project inspection confirms Services preset and
root directory `.`. No deployment, domain change, payment setting, or production
secret upload has been performed. CLI-generated .env.local and .vercel state
are ignored. Owner confirmed both service names, all listed public routes,
and no bindings for the current static-browser topology.

## Shared GitHub authorization state

OAuth attempts live in MongoDB oauth_attempts with a TTL date and application
expiry checks. Browser tickets are stored only as SHA-256 keys. Provider codes
and account/device links are AES-256-GCM encrypted using the shared deployment
RAMWARDEN_OAUTH_STATE_KEY, a base64 32-byte key; ticket-bound authenticated data
prevents moving ciphertext between attempts. No provider access token is saved.
Each poll acquires an atomic 90-second lease. Only its current lease can release
or consume an attempt; a stale instance cannot overwrite another instance's
work. Verified GitHub numeric IDs survive retryable database failures inside the
encrypted attempt, avoiding a second provider exchange. Failed/cancelled auth is
consumed; abandoned leases become retryable until the original attempt expires.
Changing encryption keys invalidates pending logins but does not unlink accounts.


## Deployment check — 2026-11-05

Isolated CLI still identifies the expected Ethan Risden account under ram-warden.
`vercel ls ramwarden --scope ram-warden` reports no deployments and `vercel env
ls` reports no environment variables. No public API, sign-in or production
pairing readiness is implied by the local installer build. PR #54's previous
head passed all five CI jobs; newer installer integration needs fresh CI.

Guest pairing is implemented in the daemon and separate installer. Its hosted
relay is proposed separately in smartcalendar PR #18, disabled by default. Twelve
backend tests plus a real Rust-client HTTP interoperability test pass locally.
The additive hosted database migration, shared abuse-control configuration and
HTTPS smoke test are still required before turning it on. ScholarCal login is
optional for calendar skill access and never required for guest pairing.

The private 0.3.3 Mac candidate deliberately uses localhost:8000 for account
setup. Public release builds must use the deployed HTTPS account service instead.
Do not publish the private development candidate as the production download.
