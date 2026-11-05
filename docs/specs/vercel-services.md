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
- GitHub pending sign-in tickets moved from process memory into shared storage;
  other device/email/wallet challenge state reviewed for multi-instance use.
- Long-running payment polling and shared trivia publishing hosted as durable
  workers or scheduled jobs, not assumed to survive container scale-to-zero.
- Reachable authenticated security-manager backend and account/device isolation.
- Atlas access from the deployment network without opening management systems.
- Production account/payment tests, secrets, HTTPS and rollback verification.
- Signed/notarized Mac and Windows release artifacts published to GitHub Releases
  (currently no releases exist), with checksums and the signed updater manifest.

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
