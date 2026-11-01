# Agent skill (`agent/`)

RamWarden packaged so a user's own AI agent (Claude Code, Codex, Cursor, Gemini
CLI and others) can install it, explain and free memory through the daemon,
read ThreatWarden, and help the user subscribe (or cancel) in crypto.

## Contract

### Package

| Path | What |
| --- | --- |
| `agent/skills/ramwarden/SKILL.md` | The skill, in the [Agent Skills](https://agentskills.io) format: frontmatter `name: ramwarden` and a `description` agents match requests against |
| `agent/skills/ramwarden/references/api.md` | The daemon routes an agent may call, split into read and act |
| `agent/skills/ramwarden/references/install.md` | Per-OS install from the release assets of `docs/specs/updates.md`, checksum first |
| `agent/skills/ramwarden/references/buying.md` | How a subscription is started and cancelled, and the rules that keep the agent from spending on its own |
| `agent/.claude-plugin/plugin.json` | The Claude Code plugin; `version` equals the workspace version |
| `.claude-plugin/marketplace.json` | Makes the repository a plugin marketplace: `/plugin marketplace add ethanrxla/ramwarden` |

Install paths: `/plugin install ramwarden@ramwarden` (Claude Code),
`npx skills add ethanrxla/ramwarden` (agents that read Agent Skills), or copy
the skill folder. The skill installs nothing by itself.

### What the skill relies on

From the daemon (`127.0.0.1:7823`): the routes in `references/api.md`. The
warden routes (`GET /wardens`, `GET /wardens/{id}`) come with the warden
registry; until then they answer 404 and the skill treats that as "not on this
install".

From the hosted API (`docs/specs/hosted-api.md`, PR #5) and the website:

1. `GET /v1/checkout/config` plans carry `tier`, `interval`, `trial_days`,
   `approval_usd` and `wardens`, so an agent can pick a plan and tell the user
   exactly what they agree to without matching on names.
2. `base` in that answer carries `contract` and `usdc` (the checkout contract
   and USDC addresses), so an agent with a wallet tool can check that it only
   sends transactions to them.
3. `account.html?plan=<id>` preselects that plan. An unknown id is ignored and
   the page behaves as without it.

4. `POST /v1/auth/verify` with `"session": "bearer"` returns the session
   token in the body, for wallet tools without a cookie jar.
5. `POST /v1/checkout/cancel` returns the wallet's `cancel` transaction (Base)
   or cancels at Stripe (card).
6. Upgrades (Lite to Plus, Plus to Pro, monthly to yearly) go through the same
   checkout routes: `upgrade: true`, and on Base the transactions end in
   `changePlan`, sent by the `payer` the answer names. Downgrades are 409.
   `GET /v1/account` carries `plan`: `lite`, `plus` or `pro`.

Warden ids are `ram` (hosted Jev) and `threat` (ThreatWarden). Plan ids are
the contract's numbers. Prices are whatever `checkout/config` says; at the
time of writing Lite is free (one device, no Jev), plan 1 is RamWarden Plus
$4/month, plan 2 RamWarden Pro (adds ThreatWarden) $5/month, plan 3 Plus
$20/year and plan 4 Pro $24/year, renewing until cancelled. Plans 1 and 3 have a 14-day trial (`trial_days:
14`); Pro (2 and 4) is charged when it starts (`trial_days: 0`), and the
agent says "charged now", never "free trial", for it. Native Solana answers
503 until it opens, so agents pay on Base.

## Policy

- **No autonomous spending.** A subscription starts only from the user's
  request in the conversation, after the agent has told them the tier, price
  and interval, the trial (or that they are charged now), that it renews until
  cancelled, the chain, and the one-year USDC approval, and the user said yes
  to that plan. Every signature and transaction is approved by the user in
  their own wallet. The agent never holds or asks for keys or seed phrases,
  and never uses a session key, delegated permission or auto-approving
  wallet. Renewals are the contract's, from the allowance the user approved;
  the agent never re-approves, upgrades or subscribes again on its own, and
  always cancels when asked. Before an upgrade it says that any trial ends,
  the new plan is charged now, and on Base unused time is not refunded.
- **Exact amounts only.** The USDC approval equals the plan's `approval_usd`
  (one year of the plan, never unlimited), the `startTrial` price equals the
  plan's `usd`, and transactions go only to `base.usdc` and `base.contract`.
- **One subscription at a time.** The agent checks `GET /v1/account` before
  checkout and never starts a second one.
- **The checkout page is the default path.** It works for every agent, needs
  no wallet tool, and is the page that already carries the site's
  `script-src 'self'` and show-before-sign policies. Driving the API from a
  wallet tool is allowed only when that tool makes the user approve each step.
- **Reading is free; acting needs a yes.** Every route that frees memory is
  proposed with its expected saving and used only after the user agrees. The
  skill never sets `force=true` unless the user named the app, never kills
  processes outside the daemon, and never touches what the daemon refuses
  (structural protection is absolute, as in `CLAUDE.md`).
- **Installs are checked.** Downloads are verified against `SHA256SUMS`
  before anything runs, and the OS installer's own prompts are left to the
  user.
- **The skill names only routes that exist.** See Verification.

## Verification

`crates/ramwarden-daemon/tests/agent_skill.rs`, in every CI run on all three
operating systems:

- every `` `METHOD /path` `` in `SKILL.md`, `api.md` and `install.md` is served
  by the daemon's router with that method (routes listed in its `PENDING` are
  allowed to be missing until their change lands);
- `SKILL.md` frontmatter is valid: `name` is the folder name in lowercase,
  digits and hyphens; `description` is 1 to 1024 characters with no angle
  brackets;
- relative links in the skill resolve;
- `plugin.json` names `ramwarden` at the workspace version, and the
  marketplace's `source` points at it.

## Basis and limitations

- The hosted checkout (accounts, wallet sign-in, `checkout/*`) is phase 2 of
  `ramwarden-api` and is not live, and the contract is not deployed. Until
  then the skill tells the user checkout is not open, which is what
  `checkout/config` failing means.
- Daemon-side account sign-in (linking this computer to the account that holds
  the subscription) is done in the menu-bar or tray app. The skill asks the user to do
  it there rather than calling a route, because no such daemon route exists yet.
- An agent cannot be forced to follow the skill. The guarantees that hold
  regardless are on-chain (`maxPrice`, funds straight to the treasury) and in
  the user's wallet (nothing moves without their approval). The skill's rules
  keep a well-behaved agent from asking for anything else.
- No release has been published yet, so the install steps fall back to the
  README's build-from-source instructions until one is.
