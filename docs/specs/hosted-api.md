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

## Phase 2: accounts, sign-in, passes

On when `RAMWARDEN_SITE_ORIGIN` is set. Code: `accounts.rs` (routes),
`wallet.rs` (SIWE/SIWS), `store.rs`, `checkout.rs`, `entitlement.rs`. The
website's routes are specified in `docs/specs/website.md`; this section adds
the daemon's and the launcher's.

### Contract

| Route | Auth | Answer |
| --- | --- | --- |
| `POST /v1/device/start` `{device_name, platform}` | none | `{device_code, user_code, verification_uri, verification_uri_complete, expires_in: 600, interval: 5}` |
| `POST /v1/device/token` `{device_code}` | none | `{device_token, account_id, device_id}` once approved; 400 `{"error":"authorization_pending"}` while waiting; 400 `{"error":"expired_token"}` when unknown, expired or already claimed |
| `GET /v1/entitlement` | device | signed record, saved by the daemon as `entitlement.json` |
| `GET /v1/downloads/{warden}/{file}` | device | the file from `RAMWARDEN_DOWNLOADS_DIR/<warden>/<file>`; 401 no or revoked device, 403 no pass for that warden, 404 otherwise |
| `GET /v1/models`, `POST /v1/systemone` | device or phase 1 token | as phase 1 |
| website routes | see `website.md` | `account` also carries `entitlements: {slug: iso time}` |

Passes are per warden (`contracts/plans.json`): `ram` is RamWarden Pro
($4 for 60 days), `threat` is ThreatWarden ($6 for 60 days), and a bundle covers
both. `entitlement_until` and `tier` in `account` describe `ram` only.

**Entitlement record.** `{"v":1, "account", "device", "issued_at",
"wardens": {slug: unix seconds}, "sig"}`. `sig` is standard base64 of an
Ed25519 signature, by `RAMWARDEN_ENTITLEMENT_KEY`, over this text (one line
each, `\n`-joined, no trailing newline):

```text
ramwarden-entitlement-v1
account=<account>
device=<device>
issued_at=<issued_at>
warden:<slug>=<expiry>        one per warden, sorted by slug
```

The daemon verifies against the public key compiled into `ramwarden-core`
(logged by the server at start) and unlocks a warden only while its expiry is
in the future. A record that fails to verify unlocks nothing.

**Base checkout.** `POST /v1/checkout/base` returns `approve(checkout, price)`
on USDC (only when the payer's allowance is short, or the server can't tell)
then `purchase(keccak256(account id), plan, price, ref)`, where `ref` is the
payment id's 32 bytes. The watcher reads `Purchased` and `Extended` logs
`RAMWARDEN_BASE_CONFIRMATIONS` blocks behind the head and sets each warden's
expiry to the `Extended` value. Agents buy the same way: wallet sign-in, then
send the returned transactions; or call the contract directly for an account
key, which the watcher credits too.

### Policy

- **Three credentials, never interchangeable.** A web session (`rwse_`, 12 h)
  opens the account and checkout; a device token (`rwdt_`) opens Jev, the
  entitlement and downloads; a phase 1 token opens Jev only. All are stored as
  SHA-256.
- **The server words every signed message** and accepts only that exact text
  back, once, within five minutes, from the address it was written for.
- **The chain is the source of truth for passes.** Expiries only move forward,
  so a replayed log or a rescan cannot shorten or double a pass.
- **Free tier:** a device without a `ram` pass gets `RAMWARDEN_FREE_PER_DAY`
  hosted decisions a day; with one, `RAMWARDEN_API_PER_DAY`.
- **CORS** names only `RAMWARDEN_SITE_ORIGIN`.
- **Downloads decide who, not what.** File names must be plain; the launcher
  checks size and SHA-256 from the signed release manifest
  (`docs/specs/updates.md`).

### Configuration

| Variable | Default |
| --- | --- |
| `RAMWARDEN_SITE_ORIGIN` | unset (phase 2 off). `https://` origin; `http://localhost:<port>` for development |
| `RAMWARDEN_ENTITLEMENT_KEY` | unset: no entitlements. Base64 of 32 random bytes (`openssl rand -base64 32`), from the secret store |
| `RAMWARDEN_DOWNLOADS_DIR` | unset: no downloads |
| `RAMWARDEN_FREE_PER_DAY` | `5` |
| `RAMWARDEN_CHECKOUT_ADDRESS` | unset: Base checkout off |
| `RAMWARDEN_BASE_CHAIN_ID` | `8453` (Base); `84532` for Base Sepolia |
| `RAMWARDEN_BASE_USDC` | Circle's USDC on those two chains |
| `RAMWARDEN_BASE_RPC_URL` | unset: always `approve`, and nothing is confirmed |
| `RAMWARDEN_BASE_CONFIRMATIONS` / `_FROM_BLOCK` | `5` / 2000 blocks back from the head |

With phase 2 on, `RAMWARDEN_API_TOKENS` becomes optional.

### Verification

`cargo test -p ramwarden-api`: SIWE message shape, EIP-55 vectors, MetaMask
and Phantom signatures accepted for their own address only; forged, replayed,
edited or foreign messages refused; device sign-in through the link page,
claimed once; Phantom web sign-in reuses one account; Base calldata (selector,
account key, exact price, ref); log decoding ignores other contracts; a
purchase credits every warden in the plan and confirms the payment; the
signed record verifies and any edit breaks it; downloads need that warden's
pass, refuse path tricks and stop when the device is removed; CORS.

### Basis and limitations

- Accounts are in memory until the Atlas store (`ramwarden-cloud`, PR #6)
  implements `Store`; a restart signs every device out. Passes survive, since
  the watcher re-reads the chain.
- Native Solana Pay and Stripe answer 503; Phantom pays on Base through its
  EVM account.
- The contract is not deployed, so Base checkout has run only against the
  test logs.
