# Website (`web/`)

The public site: the pitch, downloads, pricing, the device sign-in page that
the app opens, and the account page where people subscribe, cancel and revoke
devices. Plain HTML, CSS and ES modules with no build step, so it deploys as
static files to any host.

## Contract

### Pages

| Page | Purpose |
| --- | --- |
| `index.html` | Pitch, the reclaim ladder, Jev, comparison, downloads, extension, pricing, FAQ |
| `link.html?code=XXXX-XXXX` | Approves a device sign-in (RFC 8628). The daemon opens it after `POST /v1/device/start` |
| `account.html` | Web sign-in (wallet or email), subscription status, devices, checkout, cancel |
| `privacy.html` | Privacy policy (draft until launch) |

Any page with `?mock=1` runs against an in-page mock server (`assets/mock.js`)
and offers demo wallets, behind a banner. The mock follows this contract.

### API routes the site calls

All on `config.apiBase`, JSON in and out, errors as `{"error": "<sentence>"}`
that the page shows as is. The full list, with every field, is in
`docs/specs/hosted-api.md` (phase 2). Routes marked *session* use the
`rw_session` cookie the sign-in routes set (`HttpOnly`, `Secure`,
`SameSite=Strict`), so every call sends `credentials: "include"` and the page
never sees the token.

| Route | Body / query | Answer |
| --- | --- | --- |
| `GET /v1/device/lookup` | `?user_code=XXXX-XXXX` | `{status, device_name, platform, expires_at}`; 404 when unknown, expired or used |
| `POST /v1/auth/challenge` | `{chain: "evm"\|"solana", address, user_code: string\|null}` | `{message, expires_at}` |
| `POST /v1/auth/verify` | `{chain, address, message, signature}` | `{account, linked_device}` and the cookie; 409 when the account has 5 devices |
| `POST /v1/auth/email/start` | `{email, user_code?}` | `{sent, expires_at}`; 429 within a minute; 503 when email sign-in is off |
| `POST /v1/auth/email/verify` | `{email, code}` | as `verify` |
| `POST /v1/auth/logout` | *session* | `{ok}`, cookie cleared |
| `GET /v1/account` | *session* | `account` (below) |
| `PUT /v1/account/prefs` | *session*, any object up to 4 KB | `{prefs}` |
| `DELETE /v1/account/devices/{id}` | *session* | `{ok: true}` |
| `GET /v1/checkout/config` | | `{plans: [{id, tier, name, usd, interval, days, trial_days, approval_usd, wardens}], base: {chain_id, network, contract, usdc}\|null, solana: null, stripe: bool, stripe_test: bool}` |
| `POST /v1/checkout/base` | *session*, `{plan, payer}` | `{payment_id, chain_id, trial, approval_usd, transactions: [{to, data, value}]}`; 409 while subscribed |
| `POST /v1/checkout/stripe` | *session*, `{plan}` | `{payment_id, url, trial}`; success returns to `account.html?checkout=done&payment=<id>` |
| `GET /v1/checkout/payments/{id}` | *session* | `{id, plan, method, status: "pending"\|"confirmed"\|"failed", reason, account_status, tier, trial_ends_at, entitlement_until}` |
| `POST /v1/checkout/cancel` | *session* | Base: `{chain_id, payer, transactions}` for the paying wallet; card: `{cancelled, service_until}` |

`account` is `{id, identity, tier: "ram"|"pro"|null, interval, status:
"trial"|"active"|"grace"|"cancelled"|"lapsed"|null, trial_ends_at, paid_until,
trial_available, billing: {method}|null, entitlement_until, entitlements,
used_today, device_limit, devices: [{id, name, platform, last_seen_at}],
prefs}`.

Sign-in:

- **The server words the message.** `challenge` returns the full SIWE
  (EIP-4361) or SIWS text, with the checksummed address, domain, nonce and
  expiry. When `user_code` is set it carries
  `Resources: - urn:ramwarden:device:<user_code>`, and a valid `verify` of that
  message approves the device code as well as returning a web session.
- EVM signatures are `personal_sign` hex. Solana signatures are base58 of the
  64 signature bytes from Phantom's `signMessage`.
- Email sign-in mails a six-digit code; the page asks for it on the same
  screen. A 401 on any session route means signed out.

Checkout:

- The server builds every transaction. Base: a USDC `approve` for one year
  of the plan (shown to the user as `approval_usd`; nothing if allowance
  suffices) then `RamWardenCheckout.startTrial(account, plan, price, ref)`.
  No money moves until the trial ends (`trial: false` means it is charged
  now). The page only asks the wallet to send what it was given.
- The payer wallet need not be a wallet on the account.
- The page polls the payment every 3 s for up to 3 minutes, then tells the
  user it is safe to close: the server records the subscription on
  confirmation whether or not the page is open.
- When `checkout/config` fails or lists no method, the page says checkout
  opens soon and sells nothing.

## Policy

- **No third-party code on pages that sign.** No wallet SDK, analytics or
  CDN. Each page carries a CSP of `script-src 'self'`; its `connect-src`
  names `config.apiBase`, so moving the API means editing both.
- **Show the device before signing.** The link page shows the code, device
  name and platform from `lookup`, and warns against codes someone else sent.
- **Text from the server or a wallet is never HTML.** Pages build DOM with
  `textContent`.
- **Degrade, don't fail.** No server: the page says hosted sign-in may not be
  live and that the app works without it. No wallet: links to install
  MetaMask or Phantom. No release asset: the download goes to the releases
  page. No store listing: unpacked-install steps.
- **Prices in one place.** `assets/config.js` holds plans and limits for the
  landing page; `checkout/config` overrides them on the account page.

## Verification

- `node --test web/tests/*.test.mjs`: user-code normalisation against the
  server's alphabet, base58, hex encoding, wallet error wording, OS detection,
  the API client's session handling.
- `node web/tests/e2e.cjs`: Chromium against the mock with injected fake
  MetaMask (EIP-6963) and Phantom. Device link signs a message bound to the
  code; bad and expired codes re-prompt; a rejected signature offers a retry;
  sign in with Phantom and pay on Base with MetaMask (chain switch, approve
  then purchase, pass extends); revoke; unreachable server.
- Both run in CI (`website` job).

## Basis and limitations

- The routes above are built in `ramwarden-api` (PR #5) and tested against
  its router; the pages and `assets/mock.js` still follow the earlier
  pass-based shapes until the site's redesign moves to them.
- The checkout contract is not deployed, so the Base flow is tested only
  against the mock.
- The domain `ramwarden.app` is a placeholder until one is registered.
