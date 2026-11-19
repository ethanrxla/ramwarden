//! Paying with any coin THORChain swaps: the buyer sends BTC, ETH, BNB, LTC,
//! DOGE and so on from their own wallet, THORChain swaps it, and USDC lands
//! on Base at the operator's payout address. Nobody here holds the coins.
//!
//! # One payment buys one period
//!
//! A swap is a single push from the buyer; nothing can pull the next period
//! later, as the Base contract does. So a THORChain payment is prepaid: it
//! adds the plan's days to the account (from today, or from the end of time
//! already paid for), records the account as not renewing, and has no trial.
//! The account page shows when it ends; paying again extends it.
//!
//! # How a swap finds its account
//!
//! Every swap memo carries a minimum output, the price limit
//! (`=:BASE.USDC:<payout>:<limit>/1/0`). The server picks that limit as the
//! price plus THORChain's outbound fee, rounded up to a cent, plus a random
//! 1 to 999,999 (less than a cent, in THORChain's 1e8 units). The payment's id
//! is `thor-<limit>`, so the watcher reads the limit back out of the memo and
//! looks the payment up directly. No transaction hash is claimed by the
//! buyer, so nobody can claim someone else's swap.
//!
//! The limit also protects the operator: THORChain refunds the buyer rather
//! than pay out less than it. The watcher still checks that at least the
//! plan's price arrived before it grants anything.
//!
//! # Mainnet only
//!
//! THORChain has no public testnet (stagenet moved to Maya and uses real
//! funds), so quotes are free but every payment is real money. Without
//! `RAMWARDEN_ALLOW_LIVE_PAYMENTS=1` the server only quotes ("about 0.00006
//! BTC") and never hands out a deposit address.
//!
//! # Sending
//!
//! - EVM coins (ETH, ETH on Base, BNB, AVAX): the server builds the router's
//!   `depositWithExpiry` call and MetaMask or Phantom sends it, like the Base
//!   checkout.
//! - Everything else: the page shows the vault address, the exact amount and
//!   the memo. The buyer's wallet must be able to add a memo (OP_RETURN on
//!   Bitcoin-like chains). Memoless deposits (THORChain's reference memos)
//!   would remove that step, at the cost of a RUNE wallet on the server.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use ramwarden_cloud::model::{Account, Billing, BillingMethod, PaymentStatus, Prepaid, PrepaidGrant};
use ramwarden_cloud::store::Store;
use serde_json::{Value, json};

use crate::checkout::{Address, PlanDef, Plans, word_address, word_uint};
use crate::wallet::keccak;

/// USDC on Base, as THORChain names its pool.
pub const PAYOUT_ASSET: &str = "BASE.USDC-0X833589FCD6EDB6E08F4C7C32D4F71B54BDA02913";
/// The short form THORChain accepts in memos.
pub const PAYOUT_MEMO_ASSET: &str = "BASE.USDC";
/// THORChain counts every asset in 1e8 units: $1 of USDC is 100,000,000.
pub const UNITS_PER_USD: u64 = 100_000_000;
const UNITS_PER_CENT: u64 = UNITS_PER_USD / 100;
/// Public endpoints from THORChain's docs. Both can be overridden.
pub const DEFAULT_NODE: &str = "https://gateway.liquify.com/chain/thorchain_api";
pub const DEFAULT_MIDGARD: &str = "https://gateway.liquify.com/chain/thorchain_midgard";

/// The plan's price in cents. The one place plans.json's price is read here.
pub fn plan_cents(plan: &PlanDef) -> u64 {
    plan.cents
}

/// A coin the checkout offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PayAsset {
    /// THORChain's name, e.g. `BTC.BTC`.
    pub asset: &'static str,
    pub name: &'static str,
    pub symbol: &'static str,
    /// The coin's own decimals.
    pub decimals: u32,
    /// EVM chain id, for coins a browser wallet can send through the router.
    pub evm_chain: Option<u64>,
}

/// Native coins only: a token would need its own approve step and decimals,
/// and the chains' own coins cover nearly every wallet.
pub const ASSETS: &[PayAsset] = &[
    PayAsset { asset: "BTC.BTC", name: "Bitcoin", symbol: "BTC", decimals: 8, evm_chain: None },
    PayAsset { asset: "ETH.ETH", name: "Ether", symbol: "ETH", decimals: 18, evm_chain: Some(1) },
    PayAsset { asset: "BASE.ETH", name: "Ether on Base", symbol: "ETH", decimals: 18, evm_chain: Some(8453) },
    PayAsset { asset: "BSC.BNB", name: "BNB", symbol: "BNB", decimals: 18, evm_chain: Some(56) },
    PayAsset { asset: "AVAX.AVAX", name: "Avalanche", symbol: "AVAX", decimals: 18, evm_chain: Some(43114) },
    PayAsset { asset: "LTC.LTC", name: "Litecoin", symbol: "LTC", decimals: 8, evm_chain: None },
    PayAsset { asset: "BCH.BCH", name: "Bitcoin Cash", symbol: "BCH", decimals: 8, evm_chain: None },
    PayAsset { asset: "DOGE.DOGE", name: "Dogecoin", symbol: "DOGE", decimals: 8, evm_chain: None },
    PayAsset { asset: "GAIA.ATOM", name: "Cosmos", symbol: "ATOM", decimals: 6, evm_chain: None },
];

pub fn pay_asset(asset: &str) -> Option<&'static PayAsset> {
    ASSETS.iter().find(|a| a.asset.eq_ignore_ascii_case(asset))
}

impl PayAsset {
    /// THORChain 1e8 units to the coin's smallest unit, rounded up so the
    /// buyer never sends less than quoted.
    pub fn native_amount(&self, units: u64) -> u128 {
        let u = u128::from(units);
        if self.decimals >= 8 { u * 10u128.pow(self.decimals - 8) } else { u.div_ceil(10u128.pow(8 - self.decimals)) }
    }

    /// The amount as the buyer types it, e.g. `0.00006123`.
    pub fn display(&self, units: u64) -> String {
        let whole = units / UNITS_PER_USD;
        let frac = format!("{:08}", units % UNITS_PER_USD);
        let frac = frac.trim_end_matches('0');
        if frac.is_empty() { whole.to_string() } else { format!("{whole}.{frac}") }
    }

    pub fn json(&self) -> Value {
        json!({"asset": self.asset, "name": self.name, "symbol": self.symbol, "wallet": if self.evm_chain.is_some() { "evm" } else { "memo" }})
    }
}

#[derive(Clone, Debug)]
pub struct ThorConfig {
    pub node_url: String,
    pub midgard_url: String,
    /// The operator's USDC address on Base.
    pub payout: Address,
    /// Real payments allowed. Off: quotes only.
    pub live: bool,
}

impl ThorConfig {
    pub fn payout_hex(&self) -> String {
        format!("0x{}", hex::encode(self.payout))
    }

    /// The swap memo. 70-odd bytes, inside Bitcoin's 80-byte OP_RETURN.
    pub fn memo(&self, limit: u64) -> String {
        format!("=:{PAYOUT_MEMO_ASSET}:{}:{limit}/1/0", self.payout_hex())
    }
}

/// Every price limit starts at a whole cent; the tag below one cent tells
/// payments apart.
pub fn limit_for(price_units: u64, outbound_fee: u64, tag: u64) -> u64 {
    let floor = (price_units + outbound_fee).div_ceil(UNITS_PER_CENT) * UNITS_PER_CENT;
    floor + tag % UNITS_PER_CENT
}

pub fn payment_id(limit: u64) -> String {
    format!("thor-{limit}")
}

/// A number as THORChain writes it in memos: plain, or shortened with an
/// exponent (`11790e7`).
pub fn parse_amount(s: &str) -> Option<u64> {
    match s.split_once(['e', 'E']) {
        None => s.parse().ok(),
        Some((m, e)) => m.parse::<u64>().ok()?.checked_mul(10u64.checked_pow(e.parse().ok()?)?),
    }
}

/// The price limit of a swap memo paying `payout` in USDC on Base. `None` for
/// any other memo.
pub fn memo_limit(memo: &str, payout: &str) -> Option<u64> {
    let mut parts = memo.split(':');
    if !matches!(parts.next()?, "=" | "s" | "S" | "swap" | "SWAP") {
        return None;
    }
    let asset = parts.next()?;
    if !(asset.eq_ignore_ascii_case(PAYOUT_MEMO_ASSET) || asset.eq_ignore_ascii_case(PAYOUT_ASSET)) {
        return None;
    }
    // A refund address may follow the destination after a slash.
    let dest = parts.next()?.split('/').next()?;
    if !dest.eq_ignore_ascii_case(payout) {
        return None;
    }
    parse_amount(parts.next()?.split('/').next()?)
}

/// Calldata for `depositWithExpiry(address vault, address asset, uint256
/// amount, string memo, uint256 expiry)`, sending the chain's own coin
/// (`asset` = 0).
pub fn deposit_calldata(vault: &Address, amount: u128, memo: &str, expiry: u64) -> String {
    let sel = keccak(b"depositWithExpiry(address,address,uint256,string,uint256)");
    let mut out = format!("0x{}", hex::encode(&sel[..4]));
    for w in [word_address(vault), [0u8; 32], word_uint(amount), word_uint(5 * 32), word_uint(u128::from(expiry))] {
        out.push_str(&hex::encode(w));
    }
    out.push_str(&hex::encode(word_uint(memo.len() as u128)));
    let mut bytes = memo.as_bytes().to_vec();
    bytes.resize(memo.len().div_ceil(32) * 32, 0);
    out.push_str(&hex::encode(bytes));
    out
}

/// What the buyer is asked to send.
#[derive(Clone, Debug, PartialEq)]
pub struct Quote {
    /// 1e8 units of the coin.
    pub amount: u64,
    pub expected_out: u64,
    pub outbound_fee: u64,
    pub inbound_address: String,
    pub router: Option<String>,
    pub expiry: u64,
    pub seconds: u64,
}

/// One finished swap that paid the payout address, as Midgard reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Swap {
    pub limit: u64,
    /// USDC units (1e8) that reached the payout address.
    pub received: u64,
    pub in_tx: String,
    pub from: String,
    pub height: u64,
}

/// Midgard `actions` rows that are successful swaps to `payout`.
pub fn decode_actions(actions: &[Value], payout: &str) -> Vec<Swap> {
    let num = |v: &Value| v.as_str().and_then(|s| s.parse::<u64>().ok());
    actions
        .iter()
        .filter(|a| a["type"] == "swap" && a["status"] == "success")
        .filter_map(|a| {
            let limit = memo_limit(a["metadata"]["swap"]["memo"].as_str()?, payout)?;
            let received = a["out"]
                .as_array()?
                .iter()
                .filter(|o| o["address"].as_str().is_some_and(|x| x.eq_ignore_ascii_case(payout)))
                .flat_map(|o| o["coins"].as_array().cloned().unwrap_or_default())
                .filter(|c| c["asset"].as_str().is_some_and(|x| x.eq_ignore_ascii_case(PAYOUT_ASSET)))
                .filter_map(|c| num(&c["amount"]))
                .sum();
            let first_in = &a["in"][0];
            Some(Swap {
                limit,
                received,
                in_tx: first_in["txID"].as_str().unwrap_or("").to_string(),
                from: first_in["address"].as_str().unwrap_or("").to_string(),
                height: num(&a["height"]).unwrap_or(0),
            })
        })
        .collect()
}

/// What recording a swap did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Recorded {
    /// The account got its period.
    Granted { account_id: String, paid_until: u64 },
    /// Already recorded (a rescan), or not a payment this server issued.
    Ignored,
    /// Less than the price arrived. Left pending for support to look at.
    Short { payment_id: String, received: u64 },
}

/// A plan's value per second of service, in nanocents.
pub fn plan_rate(plan: &PlanDef) -> u64 {
    (u128::from(plan_cents(plan)) * 1_000_000_000 / u128::from((plan.days * 86_400).max(1))) as u64
}

/// When a new period ends, and what the time from now to then is worth.
///
/// Time left is kept by value, not day for day. Its value is the account's
/// running `prepaid_rate` (so monthly and annual purchases each count at
/// what they cost), or, for time not paid ahead (a cancelled card or Base
/// plan), the rate of the plan it was on. Same tier: the days carry over and
/// the rate becomes the average. Another tier: the value buys days at the
/// new plan's rate, so a year of Plus does not become a year of Pro because
/// one month of Pro was bought, and Pro left over buys more days of Plus.
pub fn prepaid_until(plans: &Plans, a: &Account, plan: &PlanDef, now: u64) -> (u64, u64) {
    let new_secs = plan.days * 86_400;
    let new_rate = plan_rate(plan);
    let left = a.paid_until.unwrap_or(0).saturating_sub(now);
    let old_rate = match (a.tier, a.billing.as_ref().map(|b| b.method), a.prepaid_rate) {
        (Some(_), Some(BillingMethod::Thorchain), Some(r)) => Some(r),
        (Some(tier), _, _) => plans
            .plans
            .iter()
            .find(|p| p.tier() == tier && Some(p.interval()) == a.interval)
            .or_else(|| plans.plans.iter().find(|p| p.tier() == tier))
            .map(plan_rate),
        _ => None,
    };
    let (left, left_value) = match old_rate {
        Some(r) if left > 0 => (left, u128::from(left) * u128::from(r)),
        _ => (0, 0),
    };
    let total_value = left_value + u128::from(new_secs) * u128::from(new_rate);
    let secs = if a.tier == Some(plan.tier()) { left + new_secs } else { (total_value / u128::from(new_rate.max(1))) as u64 };
    let rate = (total_value / u128::from(secs.max(1))) as u64;
    (now + secs, rate)
}

/// Grants the period a swap paid for.
///
/// The account update is one compare-and-swap that also records the payment
/// on the account ([`Store::apply_prepaid`]), so concurrent watchers and
/// retries add each period exactly once and never overwrite each other's.
/// The payment is confirmed only after that, so a failure in between is
/// finished by the next poll instead of losing the period.
pub async fn record(store: &dyn Store, plans: &Plans, swap: &Swap, now: u64) -> Result<Recorded, String> {
    let err = |e: ramwarden_cloud::store::StoreError| e.to_string();
    let id = payment_id(swap.limit);
    let Some(p) = store.payment(&id).await.map_err(err)? else { return Ok(Recorded::Ignored) };
    if p.method != BillingMethod::Thorchain || p.status != PaymentStatus::Pending {
        return Ok(Recorded::Ignored);
    }
    let Some(plan) = plans.plan(p.plan) else { return Ok(Recorded::Ignored) };
    if swap.received < plan_cents(plan) * UNITS_PER_CENT {
        return Ok(Recorded::Short { payment_id: id, received: swap.received });
    }
    let mut paid_until = None;
    for _ in 0..8 {
        let Some(a) = store.account(&p.account_id).await.map_err(err)? else { return Ok(Recorded::Ignored) };
        if a.prepaid_payments.contains(&id) {
            paid_until = a.paid_until;
            break;
        }
        let (until, rate) = prepaid_until(plans, &a, plan, now);
        let grant = PrepaidGrant {
            tier: plan.tier(),
            interval: plan.interval(),
            paid_until: until,
            rate,
            billing: Billing { method: BillingMethod::Thorchain, reference: swap.from.clone() },
        };
        match store.apply_prepaid(&a.id, &id, a.paid_until, &grant).await.map_err(err)? {
            Prepaid::Applied => {
                paid_until = Some(grant.paid_until);
                break;
            }
            Prepaid::AlreadyApplied | Prepaid::Changed => continue,
            Prepaid::NoAccount => return Ok(Recorded::Ignored),
        }
    }
    let Some(paid_until) = paid_until else { return Err(format!("{id}: the account kept changing; retrying next poll")) };
    if !store.confirm_payment(&id, &swap.in_tx).await.map_err(err)? {
        return Ok(Recorded::Ignored);
    }
    Ok(Recorded::Granted { account_id: p.account_id, paid_until })
}

/// Talks to THORNode (quotes, prices) and Midgard (finished swaps).
pub struct Thorchain {
    pub cfg: ThorConfig,
    http: reqwest::Client,
    /// Every swap to the payout address at or below this height has been
    /// scanned and is finished (succeeded or refunded).
    seen_height: AtomicU64,
    /// THORNode allows one quote a second per IP.
    quote_gate: tokio::sync::Mutex<()>,
}

/// Pages of 50 swaps scanned on the first poll after a start: older swaps
/// are history. Later polls page back until they reach `seen_height`.
const FIRST_PAGES: usize = 10;
/// A bound on one poll, for a Midgard that never stops paging.
const MAX_PAGES: usize = 1_000;

impl Thorchain {
    pub fn new(cfg: ThorConfig) -> Self {
        let http = reqwest::Client::builder().timeout(Duration::from_secs(15)).build().unwrap_or_default();
        Thorchain { cfg, http, seen_height: AtomicU64::new(0), quote_gate: tokio::sync::Mutex::new(()) }
    }

    async fn get(&self, url: &str) -> Result<Value, String> {
        let r = self.http.get(url).send().await.map_err(|e| format!("thorchain: {e}"))?;
        let status = r.status();
        let v: Value = r.json().await.map_err(|e| format!("thorchain: {e}"))?;
        if !status.is_success() || (v.get("code").is_some_and(|c| c.as_u64() != Some(0)) && v.get("message").is_some()) {
            return Err(v["message"].as_str().or(v["error"].as_str()).unwrap_or("THORChain refused the request").to_string());
        }
        Ok(v)
    }

    /// The coin's price in USD (1e8), from THORChain's own pool.
    pub async fn usd_price(&self, asset: &PayAsset) -> Result<u64, String> {
        let v = self.get(&format!("{}/thorchain/pool/{}", self.cfg.node_url, asset.asset)).await?;
        if v["status"].as_str() != Some("Available") || v["trading_halted"].as_bool() == Some(true) {
            return Err(format!("{} can't be swapped right now.", asset.name));
        }
        v["asset_tor_price"].as_str().and_then(|s| s.parse().ok()).filter(|&p: &u64| p > 0).ok_or_else(|| "no price".into())
    }

    async fn swap_quote(&self, asset: &PayAsset, amount: u64) -> Result<Quote, String> {
        let _gate = self.quote_gate.lock().await;
        let url = format!(
            "{}/thorchain/quote/swap?from_asset={}&to_asset={PAYOUT_ASSET}&amount={amount}&destination={}&streaming_interval=1&streaming_quantity=0",
            self.cfg.node_url,
            asset.asset,
            self.cfg.payout_hex()
        );
        let v = self.get(&url).await;
        tokio::time::sleep(Duration::from_millis(1_100)).await;
        let v = v?;
        let n = |v: &Value| v.as_str().and_then(|s| s.parse::<u64>().ok()).or(v.as_u64());
        Ok(Quote {
            amount,
            expected_out: n(&v["expected_amount_out"]).ok_or("quote without an amount")?,
            outbound_fee: n(&v["fees"]["outbound"]).unwrap_or(0),
            inbound_address: v["inbound_address"].as_str().ok_or("quote without an address")?.to_string(),
            router: v["router"].as_str().map(str::to_string),
            expiry: n(&v["expiry"]).unwrap_or(0),
            seconds: n(&v["total_swap_seconds"]).unwrap_or(0),
        })
    }

    /// How much of `asset` pays `price_units` after THORChain's fees, with a
    /// 2% cushion for the price moving while the deposit confirms. Exact-in
    /// quotes only, so: estimate from the pool price, quote, and scale once
    /// if that fell short.
    pub async fn quote_for(&self, asset: &PayAsset, price_units: u64) -> Result<Quote, String> {
        let price = self.usd_price(asset).await?;
        let want = |q: &Quote| limit_for(price_units, q.outbound_fee, UNITS_PER_CENT - 1) * 102 / 100;
        // First guess: price, plus 50 cents of fees, plus 3%.
        let guess = (u128::from(price_units + 50 * UNITS_PER_CENT) * u128::from(UNITS_PER_USD) * 103 / 100 / u128::from(price)) as u64;
        let mut q = self.swap_quote(asset, guess.max(1)).await?;
        let target = want(&q);
        if q.expected_out < target {
            let scaled = (u128::from(q.amount) * u128::from(target) / u128::from(q.expected_out.max(1)) * 101 / 100) as u64;
            q = self.swap_quote(asset, scaled).await?;
            if q.expected_out < want(&q) {
                return Err("THORChain's price moved while quoting. Try again.".into());
            }
        }
        Ok(q)
    }

    /// The router call a browser wallet sends for an EVM coin.
    pub fn transaction(&self, asset: &PayAsset, q: &Quote, memo: &str) -> Option<Value> {
        asset.evm_chain?;
        let router = q.router.as_deref()?;
        let vault = crate::checkout::parse_address(&q.inbound_address).ok()?;
        let wei = asset.native_amount(q.amount);
        Some(json!({"to": router, "data": deposit_calldata(&vault, wei, memo, q.expiry), "value": format!("0x{wei:x}")}))
    }

    /// Scans the newest swaps to the payout address and records them.
    ///
    /// The watermark never passes a swap of ours still in flight: a swap is
    /// listed at its inbound height while THORChain streams and sends it, so
    /// the next poll pages back far enough to see it finish.
    pub async fn poll(&self, store: &dyn Store, plans: &Plans, now: u64) -> Result<usize, String> {
        let payout = self.cfg.payout_hex();
        let seen = self.seen_height.load(Ordering::Relaxed);
        let pages = if seen == 0 { FIRST_PAGES } else { MAX_PAGES };
        let mut token: Option<String> = None;
        let mut top = seen;
        let mut open: Option<u64> = None;
        let mut n = 0;
        for _ in 0..pages {
            let mut url = format!("{}/v2/actions?address={payout}&type=swap&limit=50", self.cfg.midgard_url);
            if let Some(t) = &token {
                url.push_str(&format!("&nextPageToken={t}"));
            }
            let v = self.get(&url).await?;
            let actions = v["actions"].as_array().cloned().unwrap_or_default();
            let height = |a: &Value| a["height"].as_str().and_then(|h| h.parse::<u64>().ok());
            let heights: Vec<u64> = actions.iter().filter_map(height).collect();
            top = top.max(heights.iter().copied().max().unwrap_or(0));
            for a in &actions {
                let ours = a["metadata"]["swap"]["memo"].as_str().is_some_and(|m| memo_limit(m, &payout).is_some());
                if ours && a["status"] == "pending"
                    && let Some(h) = height(a)
                {
                    open = Some(open.map_or(h, |o| o.min(h)));
                }
            }
            for swap in decode_actions(&actions, &payout) {
                match record(store, plans, &swap, now).await? {
                    Recorded::Granted { account_id, paid_until } => {
                        tracing::info!(account = %account_id, paid_until, tx = %swap.in_tx, "THORChain payment recorded");
                        n += 1;
                    }
                    Recorded::Short { payment_id, received } => {
                        tracing::warn!(%payment_id, received, tx = %swap.in_tx, "THORChain swap paid less than the plan; left pending");
                    }
                    Recorded::Ignored => {}
                }
            }
            token = v["meta"]["nextPageToken"].as_str().filter(|t| !t.is_empty()).map(str::to_string);
            // Newest first: stop once this page reaches what was scanned.
            if token.is_none() || actions.is_empty() || heights.iter().any(|&h| h <= seen) {
                break;
            }
        }
        let mark = match open {
            Some(h) => top.min(h.saturating_sub(1)),
            None => top,
        };
        self.seen_height.store(mark, Ordering::Relaxed);
        Ok(n)
    }

    pub async fn run(self: Arc<Self>, store: Arc<dyn Store>, plans: Arc<Plans>, every: Duration, now: fn() -> u64) {
        loop {
            if let Err(e) = self.poll(store.as_ref(), &plans, now()).await {
                tracing::warn!("thorchain watcher: {e}");
            }
            tokio::time::sleep(every).await;
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub const PAYOUT: &str = "0xab53825e2b3dcc7d2bc2fbfc5d17124e7da20f2a";

    pub fn cfg(url: &str, live: bool) -> ThorConfig {
        ThorConfig {
            node_url: url.into(),
            midgard_url: url.into(),
            payout: crate::checkout::parse_address(PAYOUT).unwrap(),
            live,
        }
    }

    #[test]
    fn limits_start_at_a_cent_and_carry_the_tag() {
        // $3.99 plus a 25-cent outbound fee, rounded up to a cent.
        assert_eq!(limit_for(399_000_000, 24_991_200, 0), 424_000_000);
        assert_eq!(limit_for(399_000_000, 24_991_200, 123_456), 424_123_456);
        assert_eq!(limit_for(399_000_000, 0, 1_000_000 + 7), 399_000_007, "the tag stays under a cent");
        assert_eq!(payment_id(424_123_456), "thor-424123456");
    }

    #[test]
    fn memos_fit_bitcoin_and_read_back() {
        let c = cfg("http://x", true);
        let memo = c.memo(424_123_456);
        assert_eq!(memo, format!("=:BASE.USDC:{PAYOUT}:424123456/1/0"));
        assert!(memo.len() <= 80, "OP_RETURN holds 80 bytes");
        assert_eq!(memo_limit(&memo, PAYOUT), Some(424_123_456));
        assert_eq!(memo_limit(&memo.to_uppercase(), PAYOUT), Some(424_123_456), "addresses compare without case");
        assert_eq!(memo_limit(&memo.replace("0xab53", "0xab54"), PAYOUT), None, "a different destination");
        assert_eq!(memo_limit(&format!("=:BASE.USDC:{PAYOUT}:11790e7/1/0:-_/bgw:10/30"), PAYOUT), Some(117_900_000_000));
        assert_eq!(memo_limit(&format!("=:ETH.ETH:{PAYOUT}:5"), PAYOUT), None, "not USDC");
        assert_eq!(memo_limit(&format!("+:BASE.USDC:{PAYOUT}"), PAYOUT), None, "not a swap");
    }

    #[test]
    fn amounts_convert_to_each_coins_units() {
        let btc = pay_asset("btc.btc").unwrap();
        assert_eq!((btc.native_amount(6_123), btc.display(6_123)), (6_123, "0.00006123".to_string()));
        let eth = pay_asset("ETH.ETH").unwrap();
        assert_eq!(eth.native_amount(150_000), 1_500_000_000_000_000, "0.0015 ETH in wei");
        let atom = pay_asset("GAIA.ATOM").unwrap();
        assert_eq!(atom.native_amount(1_234_567_801), 12_345_679, "rounded up to a uatom");
        assert_eq!(btc.display(100_000_000), "1");
        assert!(pay_asset("ETH.USDT-0XDAC").is_none());
    }

    #[test]
    fn time_left_on_another_tier_converts_at_its_value() {
        use ramwarden_cloud::model::Identity;
        let plans = Plans::built_in();
        let (plus_m, pro_m, plus_y) = (plans.plan(1).unwrap(), plans.plan(2).unwrap(), plans.plan(3).unwrap());
        let now = 1_000_000;
        let day = 86_400;
        let fresh = Account::new("a".into(), Identity::email("a@example.com"), now);
        assert_eq!(prepaid_until(&plans, &fresh, plus_m, now), (now + plus_m.days * day, plan_rate(plus_m)), "nothing left");

        // Buy each plan in turn, as the watcher would, at `now`.
        let buy = |a: &mut Account, plan: &PlanDef| {
            let (until, rate) = prepaid_until(&plans, a, plan, now);
            a.tier = Some(plan.tier());
            a.interval = Some(plan.interval());
            a.paid_until = Some(until);
            a.prepaid_rate = Some(rate);
            a.billing = Some(Billing { method: BillingMethod::Thorchain, reference: "x".into() });
        };
        let value = |a: &Account| u128::from(a.paid_until.unwrap() - now) * u128::from(a.prepaid_rate.unwrap());
        let cents = |p: &PlanDef| u128::from(plan_cents(p)) * 1_000_000_000;
        let close = |got: u128, want: u128| got.abs_diff(want) * 10_000 < want; // within 0.01%

        let mut a = fresh.clone();
        buy(&mut a, plus_m);
        buy(&mut a, plus_m);
        assert_eq!(a.paid_until, Some(now + 2 * plus_m.days * day), "same tier: days carry over");

        // Monthly then annual Plus, then monthly Pro: every dollar keeps its value.
        let mut a = fresh.clone();
        buy(&mut a, plus_m);
        buy(&mut a, plus_y);
        assert_eq!(a.paid_until, Some(now + (plus_m.days + plus_y.days) * day));
        buy(&mut a, pro_m);
        assert!(close(value(&a), cents(plus_m) + cents(plus_y) + cents(pro_m)), "value kept");
        let pro_days = (a.paid_until.unwrap() - now) as f64 / day as f64;
        let want = (plan_cents(plus_m) + plan_cents(plus_y) + plan_cents(pro_m)) as f64 / plan_cents(pro_m) as f64 * pro_m.days as f64;
        assert!((pro_days - want).abs() < 0.01, "{pro_days} Pro days, want {want}");
        assert!(pro_days < (plus_m.days + plus_y.days + pro_m.days) as f64, "fewer Pro days than Plus days");

        // The other order lands on the same value.
        let mut b = fresh.clone();
        buy(&mut b, plus_y);
        buy(&mut b, plus_m);
        buy(&mut b, pro_m);
        assert!(close(value(&b), value(&a)));

        // Lapsed time is gone.
        a.paid_until = Some(now - 5);
        assert_eq!(prepaid_until(&plans, &a, plus_m, now).0, now + plus_m.days * day);
    }

    #[test]
    fn the_router_call_encodes_the_memo() {
        let vault = [0x48; 20];
        let data = deposit_calldata(&vault, 10, "=:BASE.USDC:0x1:5/1/0", 99);
        let body = &data[10..];
        assert_eq!(&data[2..10], hex::encode(&keccak(b"depositWithExpiry(address,address,uint256,string,uint256)")[..4]));
        assert_eq!(&body[24..64], "48".repeat(20));
        assert_eq!(&body[64..128], "0".repeat(64), "the chain's own coin");
        assert_eq!(&body[192..256], format!("{:064x}", 160), "memo offset");
        assert_eq!(&body[256..320], format!("{:064x}", 99), "expiry");
        assert_eq!(&body[320..384], format!("{:064x}", 21), "memo length");
        assert_eq!(&body[384..426], hex::encode("=:BASE.USDC:0x1:5/1/0"));
        assert_eq!(body.len(), 384 + 64);
    }

    pub fn action(memo: &str, to: &str, amount: u64, height: u64, tx: &str) -> Value {
        json!({
            "type": "swap", "status": "success", "height": height.to_string(),
            "in": [{"address": "bc1qbuyer", "txID": tx, "coins": [{"asset": "BTC.BTC", "amount": "6000"}]}],
            "metadata": {"swap": {"memo": memo}},
            "out": [
                {"address": "thor1affiliate", "coins": [{"asset": "THOR.RUNE", "amount": "5"}]},
                {"address": to, "coins": [{"asset": PAYOUT_ASSET, "amount": amount.to_string()}]}
            ]
        })
    }

    #[test]
    fn swaps_decode_only_when_they_paid_the_payout_address() {
        let memo = cfg("http://x", true).memo(424_000_001);
        let mut pending = action(&memo, PAYOUT, 1, 9, "A");
        pending["status"] = json!("pending");
        let rows = vec![
            action(&memo, &PAYOUT.to_uppercase().replace("0X", "0x"), 430_000_000, 10, "IN1"),
            pending,
            action("=:ETH.ETH:0xother:1", "0xother", 5, 8, "B"),
        ];
        let s = decode_actions(&rows, PAYOUT);
        assert_eq!(s, vec![Swap { limit: 424_000_001, received: 430_000_000, in_tx: "IN1".into(), from: "bc1qbuyer".into(), height: 10 }]);
    }
}
