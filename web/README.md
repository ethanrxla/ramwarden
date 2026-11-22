# RamWarden website

Static site: no build step, no dependencies. Behaviour and the API contract
are in [`docs/specs/website.md`](../docs/specs/website.md).

## Preview

```bash
cd web && python3 -m http.server 8000
```

- <http://localhost:8000/> RamWarden tab, <http://localhost:8000/threat.html> ThreatWarden tab
- <http://localhost:8000/?mock=1&as=trial> signed in on a trial (personal install line); `as=lapsed` shows Renew
- <http://localhost:8000/link.html?mock=1&code=BCDF-GHJK> device sign-in, against the mock server
- <http://localhost:8000/account.html?mock=1> account and checkout, against the mock server

`?mock=1` uses a pretend server and demo wallets, so every flow can be
clicked through before the hosted API exists.

## Test

```bash
node --test web/tests/*.test.mjs   # unit
node web/tests/e2e.cjs             # browser, needs Playwright + Chromium
```

## Deploy

ramwarden.com is the Vercel project `ethanrxlas-projects/ramwarden`, connected
to this repository with **Root Directory `web`**, framework "Other" and no build
command. A push to `main` deploys production. Every other branch gets a preview.
`.vercelignore` keeps `tests/`, the Dockerfile, `nginx.conf` and this README off
the site. With the root directory set to `web`, Vercel never reads the repo-root
`vercel.json`, which describes a separate container setup for the API.

`favicon.ico` and `apple-touch-icon.png` are the app icon
(`assets/brand/mac-app-icon.png`) at browser sizes. Hosts and link previews,
including Vercel's project tile, look for these files at the site root.

Any other static host (Cloudflare Pages, Netlify, GitHub Pages) works the same
way: upload `web/`. Before going live:

1. Set `apiBase` in `assets/config.js`, and the same origin in each page's
   `connect-src`.
2. Fill `chromeStore` in `assets/config.js` when the store listing goes up.
   Until then the account page shows unpacked-install steps. Install lines
   come from the API (`POST /v1/install/link`), so there is nothing to fill
   per release.
3. The API must allow CORS from the site's origin for the routes in the spec.
4. Finish `privacy.html` (it is marked draft) and add terms.
