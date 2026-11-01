# RamWarden checkout contract

`RamWardenCheckout` sells prepaid **warden passes** in USDC on Base. A plan is a
price, a duration and a set of wardens; buying one extends each of those wardens
for the account. Plans and wardens live in [`plans.json`](plans.json), which the
deploy script writes on-chain and the API serves to the website.

| Warden id | Slug | Warden | Plan |
| --- | --- | --- | --- |
| 0 | `ram` | RamWarden Pro: hosted Jev decisions | $4 for 60 days |
| 1 | `threat` | ThreatWarden: hosted threat detection | $6 for 60 days |

A bundle plan covers both for $10. Slugs are internal ids shared with the
daemon, the launcher and the release manifest, so they never change once the contract is deployed.

A new warden is a new id in `plans.json` plus plans that include it. The
contract needs no change for up to 32 wardens.

## How a payment flows

1. The signed-in user picks a plan on `account.html`.
2. The API (`POST /v1/checkout/base`) returns the transactions to sign: a USDC
   `approve` if the allowance is short, then
   `purchase(account, plan, maxPrice, ref)`. `account` is the keccak-256 of the
   RamWarden account id, `maxPrice` is the price the user was shown, and `ref` is
   the API's payment id.
3. The contract moves the USDC from the payer straight to the treasury Safe,
   extends `paidUntil[account][warden]` for each warden in the plan, and emits
   `Purchased` and one `Extended` per warden.
4. The API sees `Purchased` with that `ref`, waits for the block to be safe,
   reads `paidUntil` back and records the entitlement. The daemon learns about
   it on its next token refresh.

Anyone may pay for any account, so an agent with a wallet can buy a pass the
same way: sign in with `POST /v1/auth/challenge` and `/verify`, then send the
transactions `POST /v1/checkout/base` returns (see `docs/specs/hosted-api.md`).

Solana (Phantom) needs no program: the API builds a plain USDC transfer to the
treasury tagged with a unique reference key (Solana Pay) and records the same
per-warden entitlement once it finds the transfer. Stripe writes the same record
from its webhook.

## Safety properties

- Funds never rest in the contract, so a bug can miscount days but not lose money.
- USDC only: no ETH, no oracle, no swap. No upgrade proxy.
- `maxPrice` means a price change between quote and inclusion can't overcharge.
- A permit that someone front-ran is tolerated rather than failing the payment.
- `pause` stops sales. Ownership moves in two steps and can't be renounced.
- No pass can run more than five years ahead.

Get an outside review before mainnet.

## Develop

```bash
npm ci
npx hardhat test       # 14 tests, in-process chain
```

## Deploy

```bash
DEPLOYER_KEY=0x...  TREASURY=0x<Safe>  OWNER=0x<Safe> \
  npx hardhat run scripts/deploy.cjs --network baseSepolia   # then --network base
```

The deployer pays gas and writes the plans, then offers ownership to `OWNER`,
which must call `acceptOwnership`. `BASE_RPC_URL` and `BASE_SEPOLIA_RPC_URL`
override the public RPC endpoints.
