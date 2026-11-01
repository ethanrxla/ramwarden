# Website (`web/`)

The public site: the pitch, downloads, pricing, the device sign-in page that
the app opens, and the account page where people buy passes and revoke
devices. Plain HTML, CSS and ES modules with no build step, so it deploys as
static files to any host.

## Contract

### Pages

| Page | Purpose |
| --- | --- |
| `index.html` | Pitch, the reclaim ladder, Jev, comparison, downloads, extension, pricing, FAQ |
| `link.html?code=XXXX-XXXX` | Approves a device sign-in (RFC 8628). The daemon opens it after `POST /v1/device/start` |
| `account.html` | Web sign-in, pass status, today's usage, devices, checkout |
| `privacy.html` | Privacy policy (draft until launch) |

Any page with `?mock=1` runs against an in-page mock server (`assets/mock.js`)
and offers demo wallets, behind a banner. The mock follows this contract.

### API routes the site calls

All on `config.apiBase`, JSON in and out, errors as `{"error": "<sentence>"}`
that the page shows as is. Routes marked *session* take
`Authorization: Bearer <session>` from `/v1/auth/verify`.

| Route | Body / query | Answer |
| --- | --- | --- |
| `GET /v1/device/lookup` | `?user_code=XXXX-XXXX` | `{status, device_name, platform, expires_at}`; 404 when unknown, expired or used |
| `POST /v1/auth/challenge` | `{chain: "evm"\|"solana", address, user_code: string\|null}` | `{message, expires_at}` |
| `POST /v1/auth/verify` | `{chain, address, message, signature}` | `{session, account, linked_device}` |
| `GET /v1/account` | *session* | `account` (below) |
| `DELETE /v1/account/devices/{id}` | *session* | `{ok: true}` |
| `GET /v1/checkout/config` | | `{plans: [{id, name, usd, days}], base: {chain_id, network}\|null, solana: {network}\|null, stripe: bool}` |
| `POST /v1/checkout/base` | *session*, `{plan, payer}` | `{payment_id, chain_id, transactions: [{to, data, value?}]}` |
| `POST /v1/checkout/solana` | *session*, `{plan, payer}` | `{payment_id, message}` (base58 serialized transaction message) |
| `POST /v1/checkout/stripe` | *session*, `{plan}` | `{url}` (a `https://checkout.stripe.com/` URL; success returns to `account.html?paid=stripe`) |
| `GET /v1/checkout/payments/{id}` | *session* | `{id, plan, method, status: "pending"\|"confirmed"\|"failed", reason?, entitlement_until?}` |

`account` is `{id, tier: "free"|"pro", entitlement_until, used_today,
wallets: [{chain, address}], devices: [{id, name, platform, last_seen_at}]}`.

Sign-in:

- **The server words the message.** `challenge` returns the full SIWE
  (EIP-4361) or SIWS text, with the checksummed address, domain, nonce and
  expiry. When `user_code` is set it carries
  `Resources: - urn:ramwarden:device:<user_code>`, and a valid `verify` of that
  message approves the device code as well as returning a web session.
- EVM signatures are `personal_sign` hex. Solana signatures are base58 of the
  64 signature bytes from Phantom's `signMessage`.
- The session lives in `sessionStorage` (one tab, gone on close). A 401 clears
  it and the page asks to sign in again.

Checkout:

- The server builds every transaction. Base: a USDC `approve` (or nothing,
  if allowance suffices) then `RamWardenCheckout.purchase(account, plan)`.
  Solana: a USDC transfer with a Solana Pay reference key. The page only asks
  the wallet to send what it was given.
- The payer wallet need not be a wallet on the account.
- The page polls the payment every 3 s for up to 3 minutes, then tells the
  user it is safe to close: the server credits the pass on confirmation
  whether or not the page is open.
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

- The routes above are not built yet. `ramwarden-api` (phase 1) has only the
  Jev endpoints; `ramwarden-cloud` has the storage for accounts, device codes
  and payments. This spec is what phase 2 implements.
- No email or magic-link sign-in yet; the design allows it for card payers.
- The checkout contract is not deployed, so the Base flow is tested only
  against the mock.
- The domain `ramwarden.app` is a placeholder until one is registered.
