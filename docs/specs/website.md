# Website (`web/`)

The public site: the pitch, downloads, pricing, the device sign-in page that
the app opens, and the account page where people buy passes and revoke
devices. Plain HTML, CSS and ES modules with no build step, so it deploys as
static files to any host.

## Contract

### Pages

The product plan is `docs/product-spec.md` (section 2 is the site). This spec
is the site's part of it.

| Page | Purpose |
| --- | --- |
| `index.html` | RamWarden tab (crimson, black, silver) |
| `threat.html` | ThreatWarden tab (black, purple, green) |
| `account.html` | Sign in, start a trial, status, personal install line, browser helper, devices |
| `link.html?code=XXXX-XXXX` | Approves a device sign-in (RFC 8628). The daemon opens it after `POST /v1/device/start` |
| `privacy.html` | Privacy policy (draft until launch) |
| `notices.html` | Open-source notices: the projects the wardens are built on and their licences |
| `install.sh`, `install.ps1` | Installers that the personal one-liner runs (product spec section 5) |

Both warden tabs share one skeleton, in this order, with the same components:

| # | Section | RamWarden | ThreatWarden |
| --- | --- | --- | --- |
| 1 | Hero | Rotating italic catchline, "Free your RAM. Keep your peace.", one line, **Start free trial** and **How it works**. Graphic: the radar with RamWarden's own pixel ram (crimson sweep) | Same, "Catch threats. Keep your peace.". Graphic: the radar with the cyborg ram |
| 2 | Install (`#get`) | `assets/install.js`, the same on both | same |
| 3 | Scroll world | The promo film scrubbed by scroll | The green clip (`assets/world/threat-scrub.mp4`) scrubbed by scroll |
| 4 | Pet | The ram, talking, with the app's look controls | The cyborg ram, plus a visor color |
| 5 | How it works (`#how`) | Measures, Triggers, Protects, Decides; then three questions | Host, Files, Network, Traffic; then Acts, Protected, Plain words |
| 6 | Pricing (`#pricing`) | `assets/pricing.js`, the same on both | same |
| 7 | FAQ (`#faq`) | Short | Short |
| 8 | Footer | Same | Same |

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
| `POST /v1/install/link` | *session*, live plan | `{expires_at, commands: {macos, windows, linux}, installers: {macos, windows, linux}}`: single use, 30 minutes, links the new device on first run (product spec 2.4); 402 without a live plan, 409 at 5 devices. Installers are plain release downloads that sign in through the device page |
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

Plans are the contract's ids: 1 Plus monthly ($4), 2 Pro monthly ($5),
3 Plus annual ($20), 4 Pro annual ($24). Plus (tier id `ram`) starts with a
14-day trial; Pro is charged from day one (`trial_days: 0`). Both renew until
cancelled. RamWarden Lite has no plan: a signed-in account with nothing
bought, on 1 device, with no AI.

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

- **Two tabs, one skeleton.** The table above is the order on both pages;
  only palette, copy and media differ. Shared components (install, pricing)
  are built by one module each, so the two pages cannot drift.
- **Copy is short and abstract.** Each section is a heading, one sentence
  and cards. Pages never name the engines or providers behind a warden and
  never say "our server"; attributions live on `notices.html`. No em dashes
  anywhere under `web/`. A unit test enforces both.
- **Three cards: Lite, Plus, Pro** (changed 2026-11-01; was "no free tier").
  Lite (free) is the fixed-rules engine with no AI, on 1 device, with a free account
  (email or wallet, no card), because device limits are counted per account.
  Plus adds the AI warden and is the only plan with a trial. Pro adds
  ThreatWarden and has no trial. Bullets say what the visitor gets and promise
  only what ships; engines are still never named. The open-source repo stays
  as one line under the cards and a small card beside the install box.
- **Upgrades are a few clicks.** The account page offers Upgrade to Plus on
  Lite and Upgrade to Pro on Plus. Upgrading calls the normal checkout with the
  Pro plan while Plus is live: Base answers `{upgrade: true, payer,
  transactions}`, sent from the paying wallet; card answers `{upgrade: true,
  url: null}`, changed in place and prorated, so the page follows the payment.
  The limits themselves (Lite: no AI, 1 device) are enforced by the server and
  the signed entitlement, never by the page.
- **The install box knows who you are.** Signed out: OS tabs and Choose a
  plan, no command. Trial or active: a personal one-liner and installer
  download from `POST /v1/install/link`. Lapsed or cancelled: Renew. No
  command from `raw.githubusercontent.com` appears on the site.
- **Scroll drives the world.** `assets/world.js` scrubs each tab's clip by
  scroll position (fetched as a Blob, coalesced seeks, a lighter phone
  encode with a keyframe every 4 frames).
  Reduced motion shows the final state and all copy.
- **The pets talk, a little.** The corner pet says a line per section and,
  now and then, a joke: one every 45 to 90 s, at most 6 a visit, never while
  typing, and one click mutes it (remembered). Lines live in
  `assets/pets/<warden>-lines.json`. Comments may react to the page (the
  demo slider), never to the visitor's machine, which the site cannot see.
- **Warden look.** The Mac app's Colors pane on the page: wool, horns,
  hooves and eyes color wells, the pupil style (slits, round, none), Reset
  eyes and Original, plus six wool presets (ThreatWarden adds a visor
  color). `assets/sprite.js` redraws the sprite from the app's pixel grid with
  the app's tint factors, so each warden keeps its own shape (RamWarden never
  gets the cyborg's antenna or plates). The original look draws exactly the
  shipped SVG. Kept in `localStorage` (`rw.pet.look.<warden>`); signed in,
  also saved to `prefs.wardens.<warden>.look`.

- **No third-party code on pages that sign.** No wallet SDK, analytics or
  CDN. Each page carries a CSP of `script-src 'self'`; its `connect-src`
  names `config.apiBase`, so moving the API means editing both.
- **Show the device before signing.** The link page shows the code, device
  name and platform from `lookup`, and warns against codes someone else sent.
- **Text from the server or a wallet is never HTML.** Pages build DOM with
  `textContent`.
- **Degrade, don't fail.** No server: the page says hosted sign-in may not be
  live and that the app works without it. No wallet: links to install
  MetaMask or Phantom. No store listing: unpacked-install steps for the
  browser helper. No server: the install box stays in its signed-out state.
- **Prices in one place.** `assets/config.js` holds plans and tiers for the
  landing pages; `checkout/config` overrides them on the account page.

## Verification

- `node --test web/tests/*.test.mjs`: user-code normalisation against the
  server's alphabet, base58, hex encoding, wallet error wording, OS detection,
  the API client's session handling, the pet's look, pricing math, and the
  copy rules (no em dashes, no engine or provider names, no "our server").
- `node web/tests/e2e.cjs`: Chromium against the mock with injected fake
  MetaMask (EIP-6963) and Phantom. Both tabs have the same section order;
  the install box in signed-out, trial and lapsed states; the pricing toggle;
  pet jokes, mute and look controls. Device link signs a message bound to the
  code; bad and expired codes re-prompt; a rejected signature offers a retry;
  sign in with Phantom and start a trial on Base with MetaMask (chain switch,
  capped approve then startTrial, install line appears); revoke; unreachable server.
- Both run in CI (`website` job).

## Basis and limitations

- The routes above are not built yet. `ramwarden-api` (phase 1) has only the
  Jev endpoints; `ramwarden-cloud` has the storage for accounts, device codes
  and payments. This spec is what phase 2 implements.
- Email sign-in (product spec 3.1) is not built yet; the account page says so.
- ThreatWarden's acting steps (quarantine, blocking) are not built yet, so
  their cards carry a Soon tag.
- The checkout contract is not deployed, so the Base flow is tested only
  against the mock.
- The domain `ramwarden.app` is a placeholder until one is registered.
