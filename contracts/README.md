# RamWarden checkout contract

`RamWardenCheckout` sells **subscriptions** in USDC on Base, per
`docs/product-spec.md` 1.2 and 3.3: the plan's price every month or year until
cancelled, with a 14-day free trial first on RamWarden plans (Pro is charged
at once). Plans live in [`plans.json`](plans.json),
which the deploy script writes on-chain and the API serves to the website.

| Plan | Tier | Price | Trial | Unlocks |
| --- | --- | --- | --- | --- |
| 1 | RamWarden | $4 / month | 14 days | RAM warden (`ram`, warden 0) |
| 2 | RamWarden Pro | $5 / month | none | RAM warden and ThreatWarden (`threat`, warden 1) |
| 3 | RamWarden | $20 / year | 14 days | RAM warden |
| 4 | RamWarden Pro | $24 / year | none | RAM warden and ThreatWarden |

Plan ids are fixed: the agent skill uses them.

## How a subscription flows

1. **Start.** The signed-in user picks a plan. `POST /v1/checkout/base` returns
   a USDC `approve` for one year of the plan (12 × the monthly price, or the
   annual price) and `startTrial(account, plan, maxPrice, ref)`. `account` is
   the keccak-256 of the RamWarden account id. On a plan with a trial no money
   moves, and every warden in the plan is unlocked for 14 days. A plan without
   one (Pro), or an account that already had its trial, emits `TrialStarted`
   with `trialEnds = 0` and is charged its first period at once.
2. **Charge.** When the trial ends, and every period after, anyone may call
   `charge(account)`. `scripts/keeper.cjs` does it. The contract pulls exactly
   the price fixed at start, from the wallet that started the subscription,
   and extends `paidUntil[account][warden]` to the end of the new period.
3. **Fail.** A short balance or revoked approval reverts the charge. The
   keeper simulates first, so it costs no gas, and retries. The API shows the
   account in grace for 3 days after `paidUntil`, then local mode.
4. **Cancel.** `cancel(account)` from the paying wallet, or revoking the
   approval, stops future charges. Service runs to `paidUntil`.
5. After a year of monthly charges the approval is spent and the user is
   asked to approve another year. Nobody is ever asked for an unlimited
   approval.

The API reads `TrialStarted`, `Charged`, `Cancelled` and `Extended` logs and
records the account's tier, status and expiry. `paidUntil` is public, so the
daemon can check a licence on-chain when the API is down.

## Safety properties

- It never pulls more than the price fixed at start, never before a period is
  due, never twice for one period, and only from the wallet that subscribed.
  Changing a plan's price or wardens affects new subscriptions only.
- A charge more than 3 days late starts the period now, so nobody pays for
  time they had no service.
- Funds never rest in the contract. USDC only: no ETH, no oracle, no swap, no
  upgrade proxy.
- `pause` stops trials and charges but never a cancel. Ownership moves in two
  steps and can't be renounced.

Get an outside review before mainnet.

## Develop

```bash
npm ci
npx hardhat test       # in-process chain
```

## Deploy

```bash
DEPLOYER_KEY=0x...  TREASURY=0x<Safe>  OWNER=0x<Safe> \
  npx hardhat run scripts/deploy.cjs --network baseSepolia   # then --network base
```

Run the keeper every few minutes:

```bash
KEEPER_KEY=0x... CHECKOUT=0x<contract> FROM_BLOCK=<deploy block> \
  npx hardhat run scripts/keeper.cjs --network baseSepolia
```

The keeper wallet only pays gas. `charge` can't send it anything.

The deployer pays gas and writes the plans, then offers ownership to `OWNER`,
which must call `acceptOwnership`. `BASE_RPC_URL` and `BASE_SEPOLIA_RPC_URL`
override the public RPC endpoints.
