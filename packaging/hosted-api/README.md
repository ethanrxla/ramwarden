# Oracle hosted API deployment

Build with `cargo build --locked --release -p ramwarden-api` on Linux ARM64.
For each environment, install the executable in
`/opt/ramwarden-api/<environment>/releases/<commit>/ramwarden-api` and switch
the root-owned `current` symlink to that release. Keep the last working release.
Install the template unit in `/etc/systemd/system`. Create separate root-only
(mode 0600) `/etc/ramwarden-api/staging.env` and `production.env` from the
examples; provision each credential from its authorized secret store.
Use separate Mongo users restricted to their own database. Do not put secrets
in the binary, Vercel upload, Git, or shell history.

Start `ramwarden-api@staging` first and check `curl -f http://127.0.0.1:8081/healthz`.
On Vercel, attach `staging.ramwarden.com` to the staging deployment; the
application chooses its staging API only on that exact hostname. Add the
staging API DNS record to Oracle and use only the staging block of the Caddyfile
initially. Allow public TCP 80/443 for Caddy and keep 8080/8081, Wazuh management,
the indexer and Nyquest private. Do not modify existing Wazuh ingress.
Verify HTTPS and restart recovery before the browser checks.

Check both wardens' chat and a staff ticket reply, test checkout and webhooks
with test credentials, and exercise Lite/Plus/Pro link, denial, revocation, and
enrollment on disposable accounts and devices. Only after staging passes,
start the production instance, add its Caddy block, and verify the production
origin and certificate. Keep live payments and install links disabled until
their separate release checks pass. To roll back, point `current` to the last
working binary, restore the matching environment configuration and restart
only the affected API instance. Keep database backups before changing schemas.
