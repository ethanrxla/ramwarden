# Hosted API (`ramwarden-api`)

RamWarden's Jev decisions, answered with the operator's TypeSafe key instead of
one each user brings. Phase 1 of the [paid backend design](https://claude.ai/artifact/JKhG8jLtwhNnZ8h5LdzqvT):
no accounts or payments yet, a fixed token list instead.

## How a daemon uses it

The server speaks Jev's own protocol, so the daemon needs no code change. In
`ramwarden.toml`:

```toml
[jev]
enabled  = true
base_url = "https://api.ramwarden.example"   # the hosted server
```

and the user's RamWarden token goes where the TypeSafe key went: `JEV_API_KEY`
in `secrets.toml`, `.env` or the environment. Never in `ramwarden.toml`.

## What the server enforces

| Rule | Default | Answer when broken |
| --- | --- | --- |
| Bearer token on the list | required | 401 |
| Every question equals a [catalog](../../crates/ramwarden-api/src/catalog.rs) entry (name and definition), or fits a ladder-judge template (`return_{i}`, `kind_{i}`: same index in name and text, an app name of at most 80 characters, fixed criteria) | 6 fixed, 2 templates | 422 |
| `state` size | 16,384 bytes | 422 |
| Questions per request | 24 (the ladder judge sends up to 16) | 422 |
| Requests per token per minute | 20 | 429 |
| Requests per token per UTC day | 1000 | 429 |
| Upstream input tokens per UTC day, all users | 20,000,000 (about $0.84 at $0.042/MTok) | 503 |

The model is always the server's `JEV_MODEL`, whatever the request names. Jev's
reply is returned whole, with `x-ramwarden-remaining-today`. A 429 or 503 is a
failed Jev call to the daemon, which already falls back to its closed-form
estimate.

`state` is never logged. Logs carry an 8-hex token label, question names, sizes,
input tokens and latency.

## Running it

```bash
cargo build --release -p ramwarden-api
JEV_API_KEY=...                                   \
RAMWARDEN_API_TOKENS=rw_<random 32+ chars>,...    \
./target/release/ramwarden-api
```

| Variable | Default |
| --- | --- |
| `JEV_API_KEY` | required, from the host's secret store |
| `RAMWARDEN_API_TOKENS` | required, comma-separated, each at least 24 characters |
| `RAMWARDEN_API_BIND` | `0.0.0.0:8080` |
| `JEV_BASE_URL` / `JEV_MODEL` / `JEV_TIMEOUT_SECONDS` | `https://api.typesafe.ai` / `jev-latest` / `10` |
| `RAMWARDEN_API_CATALOG` | unset; a JSON file `{"name": [<question as the daemon sends it>]}` of extra definitions |
| `RAMWARDEN_API_PER_MINUTE` / `_PER_DAY` / `_MAX_STATE_BYTES` / `_MAX_QUESTIONS` / `_DAILY_INPUT_TOKENS` | see the table above |
| `RAMWARDEN_API_LOG` | `ramwarden_api=info` |

Make a token with `openssl rand -hex 24 | sed 's/^/rw_/'`. Serve it behind TLS
(the host's proxy); the server itself speaks plain HTTP.

## When the daemon's questions change

The catalog matches definitions exactly, so new or reworded questions must be
added to `catalog.rs` (or the catalog file) **before** a daemon that sends them
ships, and old ones kept until no released daemon sends them. The ladder judge's
per-candidate wording lives in `Template::ladder_judge`, copied from
`crates/ramwarden-daemon/src/ladderjudge.rs`. A question the
server does not know is refused with 422, and the daemon falls back as above.

## Phase 2: accounts, sign-in, subscriptions

On when `RAMWARDEN_SITE_ORIGIN` is set. Implements `docs/product-spec.md`
sections 1.2, 3 and 4. Code: `accounts.rs` (shared pieces, account,
entitlement, downloads), `signin.rs` (device flow, wallet, email, logout),
`billing.rs` (checkout, cancel, Stripe webhook), `checkout.rs` (plans, Base
transactions, chain watcher), `stripe.rs`, `mail.rs`, `entitlement.rs`,
`wallet.rs`. Storage is `ramwarden_cloud::Store` (MongoDB Atlas, or memory for
development). The website's side is in `docs/specs/website.md`.

### Contract

**Tiers and plans** (`contracts/plans.json`, the contract's numbers; cards
confirmed by the user 2026-11-01):

| Card | Plan | Tier id | Price | Trial | Wardens | Devices |
| --- | --- | --- | --- | --- | --- | --- |
| RamWarden Lite | none | `null` | free | | fixed rules only, no hosted AI, no warden pet | 1 |
| RamWarden Plus | 1, 3 | `ram` | $4 / 2 months, $20 / year | 14 days | `ram` | 3 |
| RamWarden Pro | 2, 4 | `pro` | $5 / 2 months, $24 / year | none | `ram`, `threat` | 5 |

Plans 1 and 2 bill every 60 days (`interval: "2mo"`, `days: 60`; by card,
Stripe's every-2-months recurrence), and the device limits are Lite 1, Plus
3, Pro 5 (user decision, 2026-11-02). Account records written before then
say `interval: "month"`, which reads as `2mo`.

**The warden pet** comes with a paid plan. `pet` in the account says whether
to show it; on a device, the daemon's `GET /wardens` has `pet`, true while
the signed entitlement's `ram` warden is in date
(`Entitlement::shows_pet`). Lite, a lapsed plan and a device with no signed
record show no pet. This needs no new field in the signed text.

Lite is a signed-in account (wallet or email, no card) with no plan, or one
whose plan lapsed. It gets the install line and the entitlement (tier
`none`) for one device; the Jev routes answer 402. Devices linked while on a
plan stay linked if it lapses; adding one is refused past the Lite limit.

**Upgrades** (Lite to Plus, Plus to Pro, 2-monthly to annual) are the same
checkout routes. Lite starts a new subscription. A live one moves up in
place: on Base, `changePlan(account, plan, maxPrice, ref)` from the paying
wallet (any trial ends, the new plan's first period is charged and starts
now, wardens already paid keep their dates, unused time on the old plan is
not refunded); by card, the Stripe subscription's item moves to the new
price with `trial_end=now` and the prorated difference invoiced at once.
Moving down is refused (409): cancel, then subscribe when it ends. A live
subscription upgrades with the method that pays for it.

RamWarden plans (1 and 3) start with a 14-day trial; Pro plans (2 and 4) are
charged at once (user decision, 2026-11-01). Every plan then renews every
60 days or every year until cancelled. One trial per account: an account that had
one is charged at once. `trial` in `plans.json` and the contract's `Plan`
carry this, and `trial_days` in `checkout/config` is 14 or 0.

**Account status**, from `tier`, `paid_until`, `trial_ends_at`, `cancelled`:
`trial` (inside the trial), `active` (paid period), `grace` (3 days after an
unpaid renewal), `cancelled` (stopped renewing, still inside what was paid),
`lapsed` (none of those: the apps run in local mode). `null` before anything
was bought. A warden is unlocked until `paid_until`, plus the 3-day grace
unless cancelled.

| Route | Auth | Answer |
| --- | --- | --- |
| `POST /v1/device/start` `{device_name, platform}` | none | `{device_code, user_code, verification_uri, verification_uri_complete, expires_in: 600, interval: 5}` |
| `POST /v1/device/token` `{device_code, device_name?, platform?}` | none | `{device_token, account_id, device_id}` once approved; 400 `authorization_pending` while waiting; 400 `expired_token` when unknown, expired or claimed; 403 `device_limit` if the account filled up meanwhile |
| `GET /v1/device/lookup?user_code=` | none | `{status, device_name, platform, expires_at}` for a pending code, else 404 |
| `POST /v1/auth/challenge` `{chain, address, user_code?}` | none | `{message, expires_at}`: the SIWE/SIWS text to sign |
| `POST /v1/auth/verify` `{chain, address, message, signature, session?}` | none | sign-in reply (below) |
| `POST /v1/auth/email/start` `{email, user_code?}` | none | `{sent, expires_at}`; mails a 6-digit code. 429 within 60 s of the last; 503 when mail is not configured |
| `POST /v1/auth/email/verify` `{email, code, session?}` | none | sign-in reply. 401 for a wrong code; 5 wrong guesses or 10 minutes burn it |
| `POST /v1/auth/logout` | session | ends the session, clears the cookie |
| `GET /v1/account` | session | account (below) |
| `PUT /v1/account/prefs` `{...}` | session | `{prefs}`; any JSON object up to 4 KB, replacing the old one |
| `DELETE /v1/account/devices/{id}` | session | `{ok}`; that device's token stops working at once |
| `GET /v1/checkout/config` | none | `{plans: [{id, tier, name, usd, interval, days, trial_days, approval_usd, wardens}], wardens, base: {chain_id, network, contract, usdc} \| null, solana: null, stripe, stripe_test}` |
| `POST /v1/checkout/base` `{plan, payer?}` | session | `{payment_id, chain_id, upgrade, payer, trial, approval_usd, transactions}`; for an upgrade the transactions end in `changePlan` and `payer` names the wallet that must send them; 409 for a downgrade or the same plan |
| `POST /v1/checkout/stripe` `{plan}` | session | `{payment_id, upgrade, url, trial}`: open `url` (Stripe Checkout); for an upgrade `url` is null and the change is already made; 409 for a downgrade; 503 without Stripe keys |
| `POST /v1/checkout/solana` | session | 503 until Solana opens |
| `GET /v1/checkout/payments/{id}` | session | `{id, plan, method, status: pending \| confirmed \| failed, reason, account_status, tier, trial_ends_at, entitlement_until}` |
| `POST /v1/checkout/cancel` | session | Base: `{chain_id, payer, transactions: [cancel(account)]}` for the paying wallet to send. Card: cancels at Stripe, `{cancelled, service_until}`. 409 with nothing to cancel |
| `POST /v1/stripe/webhook` | Stripe signature | 200 when recorded or ignored; 400 for a bad or stale signature |
| `POST /v1/install/link` | session | `{expires_at, commands: {macos, linux, windows}, installers: {macos, windows, linux}}`: the personal install line (product spec 2.4). 402 without an active subscription, 409 at the device limit |
| `GET /v1/install/{token}/sh`, `/ps1` | the token | the install script, `text/plain`. Doesn't use up the token |
| `GET /v1/entitlement` | device | signed record (below) |
| `GET /v1/downloads/{warden}/{file}` | device | `RAMWARDEN_DOWNLOADS_DIR/<warden>/<file>`; 401 no or revoked device, 403 tier lacks that warden or lapsed, 404 otherwise |
| `GET /v1/models`, `POST /v1/systemone` | device or phase 1 token | as phase 1; 402 `subscription_inactive` for a device whose account is lapsed or never subscribed |

**Sign-in reply.** `{account, linked_device: {name, platform} | null}` and a
`Set-Cookie: rw_session=<token>; Path=/; Max-Age=43200; HttpOnly; Secure;
SameSite=Strict`. With `"session": "bearer"` in the body the token comes back
as `session` instead of the cookie, for tools without a cookie jar, and is
then sent as `Authorization: Bearer`. With `user_code`, the device code is
approved, unless the account already has `device_limit` devices (409, shown
on the link page).

**Account.** `{id, identity: {kind: "wallet", chain, address} | {kind:
"email", email}, tier: "ram" | "pro" | null, interval: "2mo" | "year" |
null, status, trial_ends_at, paid_until, trial_available, billing: {method:
"base" | "card"} | null, plan: "lite" | "plus" | "pro", pet, entitlement_until, entitlements: {slug: iso time},
used_today, device_limit, devices: [{id, name, platform, last_seen_at}],
prefs}`. Times are ISO 8601 UTC.

**Entitlement record.** `{"v": 2, "account", "device", "issued_at", "tier",
"status", "wardens": {slug: unix seconds}, "sig"}`. `tier` and `status` read
`none` before anything was bought. `sig` is standard base64 of an Ed25519
signature, by `RAMWARDEN_ENTITLEMENT_KEY`, over this text (one line each,
`\n`-joined, no trailing newline):

```text
ramwarden-entitlement-v2
account=<account>
device=<device>
issued_at=<issued_at>
tier=<tier>
status=<status>
warden:<slug>=<expiry>        one per warden, sorted by slug
```

The daemon verifies against the public key compiled into `ramwarden-core`
(logged by the server at start) and unlocks a warden only while its expiry is
in the future. `tier` and `status` are for display ("trial ends in 3 days",
"lapsed: local mode") and never widen access. ThreatWarden needs `tier=pro`
and a `warden:threat` line in the future. A record that fails to verify
unlocks nothing; v1 never shipped and is refused.

**Install line.** The token is a device code approved for the account when
it is issued, valid 30 minutes. `commands` are `curl -fsSL
<RAMWARDEN_PUBLIC_URL>/v1/install/<token>/sh | sh` (macOS, Linux) and `irm
.../ps1 | iex` (Windows). The script downloads the release asset, checks it
against the release's `SHA256SUMS`, writes the token to `<config
dir>/ramwarden/link-token` (owner-only) and runs the platform installer. On
first run the app claims it with `POST /v1/device/token`, sending its own
`device_name` and `platform`, saves the device token and deletes the link
token. Claimed once, like any device code. `installers` are plain release
downloads that sign in through the device page, because a downloaded file
can't carry the token yet.

**Base.** `POST /v1/checkout/base` returns USDC `approve(checkout, one
year)` (6 x the 2-month price, so $24 or $30, or the annual price; never unlimited), left
out when the payer's allowance already covers it, then
`startTrial(keccak256(account id), plan, price, ref)`, where `ref` is the
payment id. The contract (`contracts/README.md`) starts the trial, or, for a
plan without one or an account that used it, emits `TrialStarted` with
`trialEnds = 0` and charges the first period at once; afterwards anyone may call `charge(account)`
when due and the keeper (`contracts/scripts/keeper.cjs`) does. The watcher
reads `TrialStarted`, `Charged` and `Cancelled`
`RAMWARDEN_BASE_CONFIRMATIONS` blocks behind the head and copies the
contract's own dates onto the account.

**Card.** Stripe Checkout in subscription mode, prices inline from
`plans.json`, `trial_period_days=14` unless the trial was used, and
`metadata: {account_id, plan, payment_id}`. `customer.subscription.*`
webhooks set tier, interval and cancellation; `paid_until` moves only for a
`trialing` or `active` subscription.

### Policy

- **Three credentials, never interchangeable.** A web session (`rwse_`, 12 h,
  normally the cookie) opens the account and checkout; a device token
  (`rwdt_`) opens Jev, the entitlement and downloads; a phase 1 token opens Jev
  only. All are stored as SHA-256. Revoking one never signs out another.
- **Cookie sessions can't be driven by another site.** `HttpOnly`, `Secure`,
  `SameSite=Strict`; CORS names only `RAMWARDEN_SITE_ORIGIN`, with
  credentials; and a cookie request that changes something must carry that
  `Origin` (403 otherwise).
- **The server words every signed message** and accepts only that exact text
  back, once, within five minutes, from the address it was written for.
  Email codes are single use, 10 minutes, 5 guesses, one per minute.
- **Only what happened changes an account.** Checkout routes create a pending
  payment and nothing else. The chain watcher and the signed Stripe webhook
  set tier and dates, from the contract's or Stripe's own numbers. `paid_until`
  only moves forward, so a replayed log, a rescan or a retried webhook cannot
  shorten or double anything.
- **Lite has no hosted AI.** A device whose account has no `ram` warden in
  date gets 402 from the Jev routes and the daemon uses its fixed rules.
- **Devices: 1 on Lite, 3 on Plus, 5 on Pro**, checked when the link page
  approves, when an install line is issued and when the daemon claims.
- **Real money waits for the audit.** Base mainnet and live Stripe keys are
  refused at start unless `RAMWARDEN_ALLOW_LIVE_PAYMENTS=1`.
- **Downloads decide who, not what.** File names must be plain; the launcher
  checks size and SHA-256 from the signed release manifest
  (`docs/specs/updates.md`).

### Configuration

| Variable | Default |
| --- | --- |
| `RAMWARDEN_SITE_ORIGIN` | unset (phase 2 off). `https://` origin; `http://localhost:<port>` for development |
| `MONGODB_URI` / `MONGODB_DATABASE` | read as in `docs/cloud-database.md`; unset: accounts in memory (development only, a restart signs everyone out) |
| `RAMWARDEN_ENTITLEMENT_KEY` | unset: no entitlements. Base64 of 32 random bytes (`openssl rand -base64 32`), from the secret store |
| `RAMWARDEN_DOWNLOADS_DIR` | unset: no downloads |
| `RAMWARDEN_CHECKOUT_ADDRESS` | unset: Base checkout off |
| `RAMWARDEN_BASE_CHAIN_ID` | `84532` (Base Sepolia); `8453` needs `RAMWARDEN_ALLOW_LIVE_PAYMENTS=1` |
| `RAMWARDEN_BASE_USDC` | Circle's USDC on those two chains |
| `RAMWARDEN_BASE_RPC_URL` | unset: always `approve`, and nothing is recorded |
| `RAMWARDEN_BASE_CONFIRMATIONS` / `_FROM_BLOCK` | `5` / 2000 blocks back from the head |
| `STRIPE_SECRET_KEY` + `STRIPE_WEBHOOK_SECRET` | unset: card checkout off. Both or neither; `sk_test_` unless live payments are allowed |
| `RESEND_API_KEY` + `RAMWARDEN_MAIL_FROM` | unset: email sign-in off |
| `RAMWARDEN_DEV_MAIL_LOG` | `1` writes sign-in codes to the log instead of mailing them. Development only |
| `RAMWARDEN_ALLOW_LIVE_PAYMENTS` | unset |
| `RAMWARDEN_PUBLIC_URL` | `https://api.<site domain>`: this API's URL as install lines name it |
| `RAMWARDEN_RELEASES_URL` | `https://github.com/ethanrxla/ramwarden/releases/latest/download` |
| `RAMWARDEN_SUPPORT_TOKEN` | unset: the site chat answers and opens tickets, but the staff ticket routes answer 503. At least 24 characters |

With phase 2 on, `RAMWARDEN_API_TOKENS` becomes optional.
`RAMWARDEN_FREE_PER_DAY` is gone with the free tier.

### Verification

`cargo test -p ramwarden-api` (`src/flows.rs` drives the router end to end):
device sign-in through the link page with MetaMask, claimed once; Phantom
sign-in reuses one account and can take a bearer; forged, replayed, edited or
foreign signatures refused; email sign-in mails a single-use code, rate
limited, and links a device; logout; the sixth device refused on the link
page and allowed after one is removed; prefs round-trip, 4 KB cap,
cross-origin cookie writes refused; a Base trial (from contract logs) moves
the account to `pro`/`trial`, confirms the payment, signs a v2 entitlement
with both wardens and opens Jev, which answered 402 before; one subscription
at a time; cancel returns the wallet's transaction and the `Cancelled` log
ends grace; a card trial starts only from a correctly signed, fresh webhook
and cancels at Stripe; the install line needs a live subscription, its script doesn't use up the
token, and the app's claim names the device and works once; ThreatWarden
downloads need Pro; CORS with
credentials for the site only. Unit tests cover SIWE, EIP-55, Base calldata
(one-year approval, `startTrial` arguments), log decoding, Stripe signatures
and event mapping, the entitlement text and config validation.
`cargo test -p ramwarden-cloud` runs the shared store contract on memory, and
on Atlas when `RAMWARDEN_TEST_MONGODB_URI` is set. `npx hardhat test` in
`contracts/` covers the subscription contract and the keeper.

### Basis and limitations

- On Base an upgrade doesn't credit unused time on the old plan (at most one
  Plus period, $4). Card upgrades are prorated by Stripe.
- One trial per account only. Per-device-fingerprint trials (spec 4.2) need
  the `trials` collection and a device key, which come with device key pairs.
- Stripe events are applied in arrival order. Stripe can deliver out of
  order, so a late `updated` could clear a cancellation; the next event
  corrects it. Ordering by event time is a follow-up.
- After a year of 2-monthly charges the approval runs out and the wallet must
  approve again (spec 3.3 step 5). The account page offers it when
  `isDue` and the charge fails; the notice email is not built yet.
- The app side of the install line (reading `link-token` on first run) is
  not built yet. Until it is, the app signs in through the device page as
  before, and the link expires unused.
- Native Solana answers 503; Phantom pays on Base through its EVM account.
- The contract is not deployed. Base checkout has run against Hardhat and the
  test logs only; Stripe against a fake. Going live needs a deployed contract
  on Base Sepolia, Stripe test keys, a Resend key, then an outside audit
  before mainnet.

### Durable account storage is required

With hosted accounts enabled, missing `MONGODB_URI` is a startup error. Configure
Atlas before accepting sign-ins or payments. `RAMWARDEN_DEV_MEMORY_STORE=1` is an
explicit escape hatch for disposable local testing only: account IDs, device
sessions, and payment mappings disappear on restart, and blockchain replay cannot
reconstruct the lost wallet-to-account mapping. Never accept real payments in
this mode.
