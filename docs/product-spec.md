# RamWarden product and site spec

Status: **draft for review** (2026-11-01). Nothing in this document is built yet
unless a section says so. Once approved, each workstream changes its own spec in
`docs/specs/` first and then the code, per `CLAUDE.md`.

This is the one plan that the website, accounts, payments, installers, the
agent skill and the wardens must agree with. Where an open PR disagrees with
this document, this document wins and the PR is updated.

Open questions are collected at the end. Everything else is a proposal that
stands unless you change it.

---

## 1. What we sell

### 1.1 Two products that never blur together

| | **Open source** | **RamWarden / RamWarden Pro** |
|---|---|---|
| What it is | The GitHub repo and its releases | The packaged, signed app |
| Where you get it | github.com, nowhere else | The website, after signing in |
| Price | Free, forever | Paid, 14-day free trial |
| Setup | You build or install it and configure everything: your own decision-model key or a local model, your own security server | Install, sign in, done |
| Account | None. It never contacts a RamWarden service | Required. Wallet or email |
| Hosted decisions, ThreatWarden hosting, multi-device, chat wardens, color sync | No | Yes |

Rules that keep them separate:

- **There is no free account.** The free option is the open-source repo. The
  site says this once, in one line, with a GitHub link, below the pricing
  cards. It is not a pricing card and not a tier.
- **The open-source build has no sign-in UI.** Hosted features are compiled
  behind a `hosted` cargo feature that is off in GitHub builds. Turning it on
  yourself gives you a client with no account, which does nothing extra.
- **The paid build degrades, never breaks.** If a subscription lapses, the
  app keeps running in local mode (local estimates, no hosted features) and
  shows one "Renew" prompt. This is "degrade, don't fail".
- The site never says "free and local by default" or "no account needed",
  because for the product on the site that is false.

### 1.2 Tiers

Prices set by Ethan on 2026-11-01:

| Tier | Monthly | Annual | Includes |
|---|---|---|---|
| **RamWarden** | $4 / month | $20 / year (save 58%) | AI-powered memory management and system resource optimization: hosted decisions, browser extension, chat with your warden, color sync, up to 5 devices |
| **RamWarden Pro** | $5 / month | $24 / year (save 60%) | Everything in RamWarden, plus the ThreatWarden security companion on the same 5 devices |
| Extra devices | Pay as you go (see Q3) | | Each device past 5, on whichever tier the account has |

- Both tiers start with a **14-day free trial**. After the trial the chosen
  payment method is charged automatically every month or every year,
  whichever the user picked, until cancelled.
  The "prepaid pass, nothing renews" model is retired.
- One trial per account **and** per device (see 4.4), so new wallets do not
  mean endless trials.
- Plan ids (the contract's numbers, used by the agent skill):
  1 = RamWarden monthly, 2 = Pro monthly, 3 = RamWarden annual,
  4 = Pro annual. The old 60-day passes and the $10 bundle are retired, and
  `contracts/plans.json` changes to match.

### 1.3 Naming

- Product: **RamWarden**. Tiers: **RamWarden** and **RamWarden Pro**. The agents are
  **wardens**: the RAM warden (in both tiers) and **ThreatWarden** (Pro).
- Tagline: "Free your RAM. Keep your peace."
- Pets: the pixel ram (RamWarden) and the pixel cyborg ram (ThreatWarden).

---

## 2. Website

The site is the user's redesign (PR #15 on `claude/project-thread-r9koyh`,
built on their `web/`). This section lists every change. Rules that apply to
every page:

- **No em dashes anywhere** (copy, alt text, titles, metadata). A test fails
  the build if `—` appears under `web/`.
- **Less text.** Each section is a heading, one sentence, and a visual or a
  card row. No paragraph longer than two lines on desktop.
- **Abstraction.** The site never says "our server(s)", never names the
  engines we use (Wazuh, ClamAV, Suricata, ntopng, Jev, TypeSafe, MongoDB,
  Oracle, Stripe as a brand beyond the payment button), and never explains
  where a component comes from. It says what the warden does. Licences and
  attributions live on a single "Open-source notices" page linked from the
  footer, which is where GPL obligations are met.

### 2.1 Structure: two identical panes

`index.html` (RamWarden) and `threat.html` (ThreatWarden) use **the same
section skeleton, in the same order, with the same components**. Only the
palette, copy and media differ (RamWarden crimson/black/silver, ThreatWarden
black/purple/green).

| # | Section | RamWarden | ThreatWarden |
|---|---|---|---|
| 1 | Hero | Rotating italic catchline, "Free your RAM. Keep your peace.", one-line sub, CTAs **Start free trial** and **How it works**. Media: the new ram clip (see 2.2) | Same layout, catchline "For people who click every link.", one-line sub, CTAs **Start free trial** and **How it works**. Media: keep the radar with the cyborg ram |
| 2 | Install | Moved up to directly under the hero (see 2.4) | Same component |
| 3 | Scroll world | The scroll-scrubbed world (scroll-world style) | Same component, ThreatWarden palette |
| 4 | Pet | The ram pet sits under the scroll world, talking (see 2.5) | The cyborg pet, same position |
| 5 | How it works | 3 or 4 cards (Measures, Triggers, Protects, Decides) | 4 cards (Host, Files, Network, Traffic), abstract copy |
| 6 | Pricing | Shared component: RamWarden card, Pro card, open-source line | Same component |
| 7 | FAQ | Short | Short |
| 8 | Footer | Same | Same |

The interactive memory demo and the comparison table move below FAQ or are
cut, to keep the page short (your call, Q6).

### 2.2 Hero media

- RamWarden side: the uploaded clip (a stone ram facing a stack of glowing
  windows on a drive, 5 s, 1280x720) **replaces the current promo film**. It
  loops muted, with a still poster for reduced-motion and slow connections.
- ThreatWarden side: keep the current radar and cyborg ram graphic.
- Remove the **Watch the film** button.

### 2.3 Copy changes (exact)

| Where | Now | Change to |
|---|---|---|
| Hero | "Free your RAM. Keep your place." | "Free your RAM. Keep your peace." |
| Hero sub | "...Your ram explains the decisions—and asks before applying a fix from chat." | "A small guardian for a crowded computer. It frees memory you won't miss and protects what you're using." |
| Hero | "Free and local by default. No account needed to use it." | Remove |
| Hero | "Watch the film" button | Remove |
| Pricing | "A prepaid pass: nothing renews or charges you later" | Remove |
| Pricing | "Pro" card ($4 / 2 months) | **RamWarden** card, $4 / month or $20 / year |
| Pricing | (none) | Add **RamWarden Pro** card after it, $5 / month or $24 / year, the only card in ThreatWarden colors |
| Pricing | "Free account, $0, sign in with a wallet, 5 hosted decisions a day, 1 device" | Remove. Replaced by the open-source line below the cards |
| Account | "It's an account without a password or an email... Your RamWarden account is separate from the wallet..." | "Sign in with your wallet or your email." |
| ThreatWarden eyebrow | "ThreatWarden · a RamWarden warden" | Remove |
| ThreatWarden hero body | "ThreatWarden starts with Wazuh... It observes; it never switches anything off." | "Watches this computer for threats and removes the real ones. Explains every call in plain words." |
| ThreatWarden "What it watches" intro | "Each engine is the official, unmodified open-source project..." | Remove |
| ThreatWarden cards | Engine names, "planned", technical detail | Host: "Spots break-ins, tampering and risky software." Files: "Catches malware in downloads and hiding places." Network: "Blocks known-bad connections." Traffic: "Shows which apps talk to whom." Unreleased ones show a small "Soon" tag |
| ThreatWarden | "OBSERVES · Read-only by design..." | Replace with "ACTS · Removes real threats. Quarantine first, always undoable. Your allowlist wins." |
| ThreatWarden | "Hosted · We run the servers... RamWarden's servers to be analysed" and "Self-hosted · ... Nothing reaches our servers" | Remove both cards. Self-hosting is part of the open-source repo's docs |
| ThreatWarden pricing | "ThreatWarden is an add-on to RamWarden, which is free. Checkout methods will appear here when hosted enrollment launches." | Remove. Shared pricing component instead |
| Jev section | "RamWarden asks Jev, TypeSafe's 'system one' model..." | "Your warden asks three quick questions about everything it might close." The three question cards stay |
| Anywhere | "our server", "our servers", "RamWarden's servers" | Remove or rephrase as "your warden" / "RamWarden" |

### 2.3a Marketing hooks (from Ethan, 2026-11-01)

| Hook | Where it goes |
|---|---|
| "Two intelligent desktop companions. Performance and security, starting at $24 a year." | Pricing section heading, and the shared footer. Accurate because $24/year is RamWarden Pro, the tier that has both wardens |
| "Your PC has 32GB of RAM. So why does it still feel slow?" | Added to the RamWarden hero's rotating catchlines. On macOS visitors it reads "Your Mac" |
| "Your task manager isn't telling you the whole story." | Heading of the RamWarden "How it works" section, above the Measures card (counting memory the way the OS charges it) |

### 2.4 Install section (moved up)

Directly under the hero. Two clearly separate halves:

**Left, the product (RamWarden / Pro):**
- Signed out: OS tabs (macOS, Windows, Linux) with a **Start free trial**
  button. No command is shown yet, because the command is personal.
- Signed in (trial or paid): the same tabs show a **personal one-liner**,
  e.g. `curl -fsSL https://get.ramwarden.app/i/K7Q2-9MXA | sh` (PowerShell
  equivalent on Windows), plus a **Download installer** button. The token in
  the URL is single-use, expires in 30 minutes, and links the new device to
  the account, so the app is signed in the moment it starts (see 5).
- Lapsed account: tabs show **Renew**.

**Right, small, open source:**
- "Prefer to build it yourself? RamWarden is open source." and a GitHub link.
  No curl line from `raw.githubusercontent.com` on the site at all.

### 2.5 Pets

- Each pane has its pet (ram, cyborg ram) under the scroll world, and the
  same pet lives at the bottom right of every page as the chat launcher.
- **Clippy-style comments.** A speech bubble appears now and then:
  - At most one every 45 to 90 seconds, at most 6 per visit, never while the
    user is typing, and one click mutes them (remembered).
  - Mostly light jokes ("I've seen your tab count. We need to talk."),
    sometimes about what's on screen (the scroll position, the demo's
    simulated memory or threat level), never about anything real on the
    visitor's machine. The site cannot see it.
  - Lines live in one JSON file per warden so they can be edited without code.
  - The same lines and the same bubble ship in the apps (menu bar, tray, GTK
    pane), where the warden may also comment on **real** activity: "Freed
    1.2 GB from 14 tabs you hadn't touched since Tuesday." or "Quarantined one
    file from Downloads. Tap to review."
- **Warden color.** A color control on the pet (6 presets plus a hue
  slider) recolors the pixel sprite live by palette swap, keeping the outline
  and eyes. Signed out it is kept in `localStorage`. Signed in it is saved to
  the account (see 4.3), and the apps use the same color.

### 2.6 Chat wardens (live assistants)

The bottom-right pets open a chat panel and act as support and sales:

- Answer product, pricing, install, trial and payment questions, grounded in
  a curated FAQ and the public docs. No made-up promises: if it doesn't know,
  it says so and offers email support.
- Signed in, it can read the account's own status (tier, trial end, devices,
  last payment) to help with "why didn't my download work". It cannot change
  anything, take payment, or see other accounts.
- ThreatWarden's pet answers security questions on its pane; the ram answers
  the rest. Same backend, different persona and color.
- Rate limited per visitor and per account. Conversations are kept 30 days
  for support, and the privacy page says so.
- Model choice and provider are internal and never named on the site.

### 2.7 Pricing component

```
            ( Monthly | Annual, save up to 60% )

[ RamWarden  $4 / month ]    [ RamWarden Pro  $5 / month ]  (ThreatWarden colors)
  or $20 / year                or $24 / year
  14-day free trial            14-day free trial
  AI-powered memory            Everything in RamWarden,
  management and system        plus the ThreatWarden
  resource optimization        security companion
  5 devices                    5 devices
  [Start free trial]           [Start free trial]

  Pay with a wallet, card or Apple Pay. Cancel any time.
  Prefer to build it yourself? RamWarden is open source on GitHub.
```

---

## 3. Sign-in and payment

### 3.1 Two ways in, one account

| | Wallet | Email |
|---|---|---|
| Identity | The wallet address **is** the account | The verified email is the account |
| Sign-in | Sign-in-with-Ethereum / Solana message, no gas | Magic link or one-time code, then an optional passkey |
| Pays with | The same wallet, through the smart contract | Card, Apple Pay or Google Pay through the card processor |
| Trial charge | Contract charges the wallet at trial end (3.3) | Card processor charges at trial end (3.4) |

- "Your RamWarden account is basically your wallet." Signing in with a new
  wallet creates the account and **immediately starts the purchase flow**:
  the user picks RamWarden or Pro, monthly or annual, confirms in the wallet, and the trial begins.
  There is no "account but nothing bought" state for wallet users.
- Email sign-up works the same way: verify email, pick a tier, enter a card
  (or Apple Pay), trial begins.
- Linking a second identity (an email to a wallet account, for receipts) is
  optional and later. The account's primary identity never changes.

### 3.2 Wallet support

- One connect dialog covering EVM and Solana wallets through a wallet
  aggregation library (recommended: **Reown AppKit**, formerly WalletConnect
  Web3Modal, which handles Phantom, MetaMask, Coinbase Wallet, Rabby and
  mobile wallets over WalletConnect on both chains). It loads from a pinned,
  hashed bundle so the site keeps no build step.
- Chains: **Base** (USDC) first, then **Solana** (USDC). Phantom works on
  both.
- Testing: **Base Sepolia** and **Solana devnet** with test USDC. Phantom
  and MetaMask both support these test networks. `?mock=1` stays for UI work
  with no chain at all.

### 3.3 Trial and renewal on-chain

The current `RamWardenCheckout` sells prepaid passes. It changes to a
subscription with a trial:

1. **Start trial**: the user signs one transaction that (a) approves the
   contract to spend USDC up to **one year** of the plan (monthly: 12 x $4 =
   $48 or 12 x $5 = $60; annual: $20 or $24) and (b) calls
   `startTrial(account, plan)`. The
   wallet shows exactly this amount. No money moves.
2. **Charge**: after 14 days, and every month or year after, anyone may call
   `charge(account)` (we run the keeper). The contract pulls **exactly one
   period's price** and extends `paidUntil`. It can never pull more, earlier,
   or for a plan the user didn't pick.
3. **Fails**: if the balance or allowance is short, `charge` fails, the
   account goes into a 3-day grace period with an in-app and email/wallet
   notice, then drops to local mode.
4. **Cancel**: `cancel(account)` from the wallet, or revoking the approval
   in the wallet, stops all future charges. Service runs to `paidUntil`.
5. After a year of charges the user is asked to re-approve. They are never asked
   for an unlimited approval.
6. **Solana**: the same flow using an SPL token delegate approval capped at
   one year and a program that enforces the same schedule. Built after Base.

The contract stays the license's source of truth (`paidUntil[account][warden]`).
The plan's warden list decides what is unlocked: RamWarden unlocks warden 0,
Pro unlocks 0 and 1.

### 3.4 Card payments

Stripe Checkout in subscription mode with a 14-day trial and the card,
Apple Pay and Google Pay methods. Webhooks update the account. The site's
button reads "Pay with card or Apple Pay", not a processor name.

### 3.5 What the user gets the moment they confirm

The trial-start transaction or the card setup confirms, and the same page
immediately shows the personal install line and installer download (2.4),
plus the browser extension step (6). No email round trip, no waiting.

---

## 4. Accounts, sessions and data

### 4.1 Database separation (MongoDB Atlas)

Builds on `ramwarden-cloud` (PR #6). One database, these collections:

| Collection | Holds |
|---|---|
| `accounts` | `_id`, `identity: {kind: "wallet", chain, address} \| {kind: "email", email}`, `tier: "ram" \| "pro" \| null`, `interval: "month" \| "year"`, `status: "trial" \| "active" \| "grace" \| "lapsed" \| "cancelled"`, `trial_ends_at`, `paid_until`, `billing: {method: "base" \| "solana" \| "card", ref}`, `device_limit`, `prefs` |
| `sessions` | One document per signed-in browser or device: hashed token, account id, kind (`web` / `device`), user agent, created, last seen, expiry |
| `devices` | Per installed app: device public key, name, OS, `tailscale_node` (optional), first and last seen, revoked flag |
| `payments` | Every charge attempt and its outcome |
| `trials` | Hashed device fingerprints that have used a trial |
| `chat` | Chat-warden conversations, 30-day TTL |

- **RamWarden and Pro users are separated by the `tier` field**, indexed, and every
  hosted route checks it. ThreatWarden routes require `tier == "pro"` and an
  active status. We do not split them into separate databases: one account
  can move between tiers.
- **Every session is separate and has its own credentials.** Web sessions
  move from `sessionStorage` bearer tokens to an `HttpOnly`, `Secure`,
  `SameSite=Strict` cookie. Each device has its own key pair, generated at
  install, and its own refresh token. Revoking one device or session never
  signs out the others.
- The entitlement the daemon checks is the existing Ed25519-signed per-warden
  record (`/v1/entitlement`), now also carrying `tier` and `status`.

### 4.2 Abuse and limits

- Track devices by their key, not by IP. IPs are logged with sessions only
  for abuse signals (many trials from one address), never as identity.
- One trial per wallet, per email and per device fingerprint.

### 4.3 Preferences that follow the user

`accounts.prefs` holds `wardens.<id>.color` and the pet-comment mute.
The site writes it, and the apps read it at sign-in and on change, so a color
picked on the site shows up in the menu bar.

---

## 5. Install and license (the audio-plugin model)

Audio plugins ship one installer and a separate license check. We do the same:

1. **One signed installer per OS**, identical for everyone: notarized `.pkg`
   (macOS), signed `.msi` (Windows), `.deb`/`.rpm` plus a script (Linux).
   Built by the release pipeline in PR #14, which already signs manifests.
2. **License check at first launch and every 24 hours**: the daemon asks for
   its signed entitlement and caches it. Offline, the cached entitlement is
   honoured until its expiry plus 7 days. As a second source, the daemon can
   read `paidUntil` from the contract directly, so a wallet user's license
   works even if our API is down.
3. **The installer detects the user**:
   - Run from the personal one-liner or a downloaded installer that carries
     a token: it links the device and starts signed in.
   - Run without a token: it installs, and the app opens the device sign-in
     page (the existing RFC 8628 flow).
   - The open-source build has no license check at all (1.1).
4. Updates come through the launcher in PR #14, served only to devices with
   an active entitlement. Lapsed devices keep their installed version.

Code signing needs an Apple Developer account and a Windows signing
certificate (Azure Trusted Signing is the cheapest route) before launch.
Without them, macOS and Windows show scary warnings, which would undermine
"we aren't bundling malware".

---

## 6. Browser extension setup on sign-in

Browsers do not let a website or an installer silently add an extension for
a normal user. The friendliest honest flow:

1. After the trial starts, the success page shows **Add to Chrome** (or
   Edge, Firefox, Brave) for the detected browser, linking straight to the
   store listing.
2. The extension finds the local daemon on `localhost` and pairs itself with
   no code to type. It shows "Connected to your warden" in its popup.
3. If the user skipped it, the app's menu bar/tray shows a one-time "Add the
   browser helper" nudge, and the ram mentions it once.
4. Installing via browser policy (forced install) is possible but marks the
   browser "managed by your organization". It is offered only for business
   fleets (7.3), never by default.

---

## 7. Multiple devices

### 7.1 Limits

- Each account covers **5 devices**, any mix of macOS, Windows and Linux.
- Pro covers ThreatWarden on the same 5 devices.
- Device 6 onwards is **pay as you go** per device per period (Q3), billed
  through the same method as the subscription. The app refuses to link a 6th
  device until the user confirms the extra charge.
- Devices can be removed on the account page to free a slot.

### 7.2 Adding devices with the agent skill and Tailscale

The user's agent sets up their other computers. It works on the **user's own
tailnet**. We never add their devices to ours, because that would give us
network access to their machines.

1. The agent asks the daemon on the current (signed-in) machine for an
   **enrollment token**: single use, 15 minutes, scoped to one new device.
   The skill holds no long-lived credentials, so leaking the skill leaks
   nothing.
2. If Tailscale is installed, the agent lists the user's tailnet devices
   (`tailscale status --json`), shows which have no RamWarden yet, and on the
   ones the user picks runs the right installer over Tailscale SSH with the
   token (`install.sh` on macOS/Linux, `install.ps1` on Windows).
3. If Tailscale isn't installed, the agent offers to install it (the user
   signs in to Tailscale themselves) or falls back to showing a personal
   one-liner or QR code to run on the other machine.
4. Each new device generates its own key and appears in `devices` with its
   tailnet name. Every step is confirmed by the user, as the skill already
   requires for purchases.

Requires an active RamWarden or Pro account, because enrollment tokens are only
minted for one.

### 7.3 Businesses

Past 5 devices it is pay as you go. For fleets: a downloadable installer
with an **organization enrollment key** (revocable, limited to N devices) for
use with MDM, Intune, Ansible and similar, optional forced extension install
by policy, and an invoice option. This is phase 3; listed so the data model
leaves room for `org_id` on accounts and devices.

---

## 8. ThreatWarden actively protects

ThreatWarden moves from read-only to acting, the same way the RAM warden
acts on memory: a ladder ordered by how undoable each step is.

| Step | Trigger | Action | Undo |
|---|---|---|---|
| 1. Notify | Anything worth a look | Pet comment and pane entry | n/a |
| 2. Quarantine file | High-confidence malware match | Move to an encrypted quarantine folder, strip execute permission | Restore from the pane |
| 3. Block connection | Known-bad host or callback | Firewall rule for that host only, with an expiry | Remove the rule |
| 4. Stop process | A running process that is the confirmed malware itself | Kill by handle, then quarantine its file | Restore file; process is not restarted |
| 5. Ask | Anything medium confidence, or anything signed by a known publisher | Prompt with the evidence; nothing happens until the user says so | n/a |

Rules:

- **Only real threats.** Steps 2 to 4 need a high-confidence detection
  (signature or known-bad indicator match, or a high-severity rule).
  Heuristic or "suspicious" alone only ever reaches step 5.
- **Allowlist wins.** Settings let the user allow a file (by hash), a folder,
  an app (by signer), or a host. Anything allowed is never acted on and
  stops appearing as a finding. "Allow" is one tap on any finding.
- **Protected things are never touched**, the same list as the RAM warden
  plus all system, security and anti-cheat files and processes.
- **Never weaken security** (no disabling Defender, firewalls or OS
  protections), as in `CLAUDE.md`.
- **Every action is logged and reported as what happened**, not what was
  attempted (`Outcome` semantics).
- A "Watch only" switch keeps the old read-only behaviour for people who
  want it.

This needs `docs/specs/threatwarden.md` (PR #13) changed first.

---

## 9. Agent skill changes

- Purchases become "start trial" or "change tier", still only on the user's
  request and confirmed in their wallet. The agent never approves more than
  the one-year cap.
- New: add devices (7.2), list and remove devices.
- The "never renews on its own" wording is updated: the **agent** never
  renews; the **subscription** renews by the user's approval.

---

## 10. Where the existing work fits

| PR | State | Change needed |
|---|---|---|
| #15 Website | Draft, CI green | All of section 2. Copy, structure, installs higher, pets, pricing |
| #5 Hosted API + contract | Draft | Contract to trial/subscription (3.3), email auth, Stripe subscription, `tier`/`status` in entitlement, enrollment tokens, cookies |
| #6 MongoDB | Draft | Collections and fields in 4.1 |
| #13 ThreatWarden | Draft | Active protection (8), allowlist settings |
| #14 Installers and launcher | Draft | Token-aware installers, license check, signed packages (5) |
| #16 Agent skill | Merged | Follow-up for section 9 |
| #18 App panes | Draft, CI red | Pet comments and color in the app panes (2.5) |

---

## 11. Build order

1. **Approve this spec.**
2. **Website copy and structure** (section 2 minus chat): fast, visible,
   no backend. Uses `?mock=1` for signed-in states.
3. **Accounts**: wallet and email sign-in, cookies, MongoDB fields.
4. **Trial and subscription** on Base Sepolia and Stripe test mode, then
   the personal install line and instant download.
5. **License check and installers**, then code signing.
6. **ThreatWarden active protection.**
7. **Multi-device with the skill and Tailscale.**
8. **Chat wardens**, then Solana, then business fleets.

---

## 12. Open questions

- **Q1. Pro price.** Decided 2026-11-01: RamWarden $4/month or $20/year,
  RamWarden Pro $5/month or $24/year, Pro includes everything.
- **Q2. Trial needs payment up front?** Recommended yes (wallet approval or
  card on file), so the trial converts without a second step. This is how the
  wallet "charges after 2 weeks" works.
- **Q3. Price per extra device** past 5. Suggestion: $0.50 per device per
  month on RamWarden, $0.75 on Pro (annual: $4 and $5 per device per year).
- **Q4. Domain.** The spec uses `ramwarden.app` as a placeholder.
- **Q5. Jev on the site.** This spec stops naming Jev and TypeSafe on the
  site, in line with "don't say where we got it". Keep naming it if the
  partnership is a selling point.
- **Q6.** Keep the interactive memory demo and comparison table (moved down)
  or cut them.
