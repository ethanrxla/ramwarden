# RamWarden website

Static site: no build step, no dependencies. Behaviour and the API contract
are in [`docs/specs/website.md`](../docs/specs/website.md).

## Preview

```bash
cd web && python3 -m http.server 8000
```

- <http://localhost:8000/> landing page
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

Upload the `web/` folder to any static host (Cloudflare Pages, Netlify,
GitHub Pages). Before going live:

1. Set `apiBase` in `assets/config.js`, and the same origin in each page's
   `connect-src`.
2. Fill `downloads` and `chromeStore` in `assets/config.js` as releases and
   the store listing go up. Until then the buttons fall back to the releases
   page and unpacked-install steps.
3. The API must allow CORS from the site's origin for the routes in the spec.
4. Finish `privacy.html` (it is marked draft) and add terms.
