//! Selling subscriptions on Base: the plan list, the transactions the website
//! asks the wallet to send, and the watcher that records what the contract
//! did.
//!
//! # The server builds every transaction
//!
//! `POST /v1/checkout/base` returns a USDC `approve` for one year of the plan
//! (12 × the monthly price, or the annual price; spec 3.3, never unlimited),
//! skipped when the allowance already covers it, then
//! `RamWardenCheckout.startTrial(account, plan, maxPrice, ref)`. `account` is
//! keccak-256 of the account id; `ref` is the payment id.
//!
//! # The chain is the source of truth
//!
//! The watcher reads `TrialStarted`, `Charged` and `Cancelled` from the
//! contract a few blocks behind the head and turns each into a
//! [`BillingUpdate`]: tier and interval from the plan, `paid_until` from the
//! contract's own numbers. It never adds time itself, so a log seen twice, a
//! rescan after a restart, or a subscription started straight on the contract
//! (by an agent) all land on what the contract holds.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use ramwarden_cloud::model::{Billing, BillingMethod, BillingUpdate, Interval, Tier, chain_key};
use ramwarden_cloud::store::Store;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::wallet::keccak;

/// `contracts/plans.json`: the one list of plans. The deploy script writes
/// the same file on-chain, and a contract test checks it against the spec.
pub const PLANS_JSON: &str = include_str!("../../../contracts/plans.json");

#[derive(Clone, Debug, Deserialize)]
pub struct WardenDef {
    pub slug: String,
    pub name: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct PlanDef {
    pub id: u32,
    /// `ram` or `pro`.
    pub tier: String,
    pub name: String,
    pub usd: u64,
    /// `month` or `year`.
    pub interval: String,
    pub days: u64,
    /// Whether a first subscription starts with the 14-day trial. RamWarden
    /// only: Pro is charged at once (user decision, 2026-11-01).
    #[serde(default)]
    pub trial: bool,
    pub wardens: Vec<u8>,
}

impl PlanDef {
    pub fn tier(&self) -> Tier {
        if self.tier == "pro" { Tier::Pro } else { Tier::Ram }
    }

    pub fn interval(&self) -> Interval {
        if self.interval == "year" { Interval::Year } else { Interval::Month }
    }

    /// Whether this account gets the trial on this plan.
    pub fn trial_for(&self, trial_used: bool) -> bool {
        self.trial && !trial_used
    }

    /// What the wallet is asked to approve: one year of this plan.
    pub fn approval_usd(&self) -> u64 {
        match self.interval() {
            Interval::Month => self.usd * 12,
            Interval::Year => self.usd,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct Plans {
    pub wardens: std::collections::BTreeMap<String, WardenDef>,
    pub plans: Vec<PlanDef>,
}

impl Plans {
    pub fn built_in() -> Self {
        Self::from_json(PLANS_JSON).expect("contracts/plans.json is valid; the contract tests check it")
    }

    pub fn from_json(text: &str) -> Result<Self, String> {
        let p: Plans = serde_json::from_str(text).map_err(|e| format!("plans.json: {e}"))?;
        for plan in &p.plans {
            if !matches!(plan.tier.as_str(), "ram" | "pro") || !matches!(plan.interval.as_str(), "month" | "year") {
                return Err(format!("plan {} has an unknown tier or interval", plan.id));
            }
            for w in &plan.wardens {
                if !p.wardens.contains_key(&w.to_string()) {
                    return Err(format!("plan {} names unknown warden {w}", plan.id));
                }
            }
        }
        Ok(p)
    }

    pub fn plan(&self, id: u32) -> Option<&PlanDef> {
        self.plans.iter().find(|p| p.id == id)
    }

    pub fn slug(&self, w: u8) -> Option<&str> {
        self.wardens.get(&w.to_string()).map(|d| d.slug.as_str())
    }

    pub fn slugs(&self, plan: &PlanDef) -> Vec<String> {
        plan.wardens.iter().filter_map(|&w| self.slug(w)).map(str::to_string).collect()
    }

    /// `GET /v1/checkout/config`.
    pub fn config_json(&self, base: Option<&BaseConfig>, card: Option<bool>) -> Value {
        let plans: Vec<Value> = self
            .plans
            .iter()
            .map(|p| {
                json!({"id": p.id, "tier": p.tier, "name": p.name, "usd": p.usd, "interval": p.interval,
                       "days": p.days, "trial_days": if p.trial { 14 } else { 0 }, "approval_usd": p.approval_usd(), "wardens": self.slugs(p)})
            })
            .collect();
        let wardens: Vec<Value> = self.wardens.values().map(|w| json!({"slug": w.slug, "name": w.name})).collect();
        json!({
            "plans": plans,
            "wardens": wardens,
            "base": base.map(|b| json!({"chain_id": b.chain_id, "network": b.network(),
                                        "contract": b.checkout_hex(), "usdc": b.usdc_hex()})),
            "solana": Value::Null,
            "stripe": card.is_some(),
            "stripe_test": card.unwrap_or(false),
        })
    }
}

/// USDC has 6 decimals.
pub fn usdc_minor(usd: u64) -> u128 {
    u128::from(usd) * 1_000_000
}

pub const BASE_MAINNET: u64 = 8453;
pub const BASE_SEPOLIA: u64 = 84532;

pub type Address = [u8; 20];

pub fn parse_address(s: &str) -> Result<Address, String> {
    let h = s.trim().strip_prefix("0x").ok_or_else(|| format!("`{s}` is not a 0x address"))?;
    hex::decode(h).ok().and_then(|b| b.try_into().ok()).ok_or_else(|| format!("`{s}` is not a 20-byte address"))
}

#[derive(Clone, Debug)]
pub struct BaseConfig {
    pub chain_id: u64,
    pub checkout: Address,
    pub usdc: Address,
    /// JSON-RPC for the allowance check and the watcher. Without it the server
    /// still sells (always including `approve`) but cannot confirm.
    pub rpc_url: Option<String>,
    /// Blocks behind the head the watcher stays.
    pub confirmations: u64,
    /// First block to scan on start (the deployment block, ideally).
    pub from_block: Option<u64>,
}

impl BaseConfig {
    /// Circle's USDC on the chain, so it never has to be configured by hand.
    pub fn default_usdc(chain_id: u64) -> Option<Address> {
        let a = match chain_id {
            BASE_MAINNET => "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
            BASE_SEPOLIA => "0x036CbD53842c5426634e7929541eC2318f3dCF7e",
            _ => return None,
        };
        parse_address(a).ok()
    }

    pub fn network(&self) -> &'static str {
        match self.chain_id {
            BASE_MAINNET => "base",
            BASE_SEPOLIA => "base-sepolia",
            _ => "custom",
        }
    }

    pub fn checkout_hex(&self) -> String {
        format!("0x{}", hex::encode(self.checkout))
    }

    pub fn usdc_hex(&self) -> String {
        format!("0x{}", hex::encode(self.usdc))
    }

    /// The transactions that start a subscription, in the order to send them.
    pub fn transactions(&self, account_id: &str, plan: &PlanDef, payment_ref: &[u8; 32], needs_approve: bool) -> Vec<Value> {
        let mut txs = Vec::new();
        if needs_approve {
            let cap = usdc_minor(plan.approval_usd());
            let data = abi_call("approve(address,uint256)", &[word_address(&self.checkout), word_uint(cap)]);
            txs.push(json!({"to": self.usdc_hex(), "data": data, "value": "0x0"}));
        }
        let data = abi_call(
            "startTrial(bytes32,uint32,uint256,bytes32)",
            &[chain_key(account_id), word_uint(u128::from(plan.id)), word_uint(usdc_minor(plan.usd)), *payment_ref],
        );
        txs.push(json!({"to": self.checkout_hex(), "data": data, "value": "0x0"}));
        txs
    }

    /// `cancel(account)`, for the account page's cancel button. Only the
    /// paying wallet can send it successfully.
    pub fn cancel_transaction(&self, account_id: &str) -> Value {
        json!({"to": self.checkout_hex(), "data": abi_call("cancel(bytes32)", &[chain_key(account_id)]), "value": "0x0"})
    }
}

pub fn selector(signature: &str) -> [u8; 4] {
    let h = keccak(signature.as_bytes());
    [h[0], h[1], h[2], h[3]]
}

pub fn word_uint(v: u128) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[16..].copy_from_slice(&v.to_be_bytes());
    w
}

pub fn word_address(a: &Address) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[12..].copy_from_slice(a);
    w
}

/// Calldata for a function whose arguments are all static words.
pub fn abi_call(signature: &str, words: &[[u8; 32]]) -> String {
    let mut out = format!("0x{}", hex::encode(selector(signature)));
    for w in words {
        out.push_str(&hex::encode(w));
    }
    out
}

pub fn trial_started_topic() -> [u8; 32] {
    keccak(b"TrialStarted(bytes32,uint32,address,uint64,bytes32)")
}

pub fn charged_topic() -> [u8; 32] {
    keccak(b"Charged(bytes32,uint32,address,uint256,uint64)")
}

pub fn cancelled_topic() -> [u8; 32] {
    keccak(b"Cancelled(bytes32,uint64)")
}

fn hex32(v: &Value) -> Option<[u8; 32]> {
    hex::decode(v.as_str()?.trim_start_matches("0x")).ok()?.try_into().ok()
}

fn hex_u64(v: &Value) -> Option<u64> {
    u64::from_str_radix(v.as_str()?.trim_start_matches("0x"), 16).ok()
}

fn tail_u64(w: &[u8]) -> u64 {
    u64::from_be_bytes(w[w.len() - 8..].try_into().expect("8 bytes"))
}

/// One thing the contract did to a subscription.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChainEvent {
    TrialStarted { account_key: [u8; 32], plan: u32, payer: String, trial_ends: u64, payment_ref: [u8; 32] },
    Charged { account_key: [u8; 32], plan: u32, payer: String, paid_until: u64 },
    Cancelled { account_key: [u8; 32] },
}

impl ChainEvent {
    pub fn account_key(&self) -> &[u8; 32] {
        match self {
            ChainEvent::TrialStarted { account_key, .. }
            | ChainEvent::Charged { account_key, .. }
            | ChainEvent::Cancelled { account_key } => account_key,
        }
    }

    /// What the event means for the account.
    pub fn update(&self, plans: &Plans) -> BillingUpdate {
        let plan_update = |plan: u32, payer: &str| {
            let p = plans.plan(plan);
            BillingUpdate {
                tier: p.map(PlanDef::tier),
                interval: p.map(PlanDef::interval),
                billing: Some(Billing { method: BillingMethod::Base, reference: payer.to_string() }),
                ..Default::default()
            }
        };
        match self {
            // No trial (Pro, or the trial was used): the `Charged` in the same
            // transaction sets the dates.
            ChainEvent::TrialStarted { plan, payer, trial_ends: 0, .. } => {
                BillingUpdate { cancelled: Some(false), ..plan_update(*plan, payer) }
            }
            ChainEvent::TrialStarted { plan, payer, trial_ends, .. } => BillingUpdate {
                trial_ends_at: Some(*trial_ends),
                paid_until: Some(*trial_ends),
                cancelled: Some(false),
                trial_used: Some(true),
                ..plan_update(*plan, payer)
            },
            ChainEvent::Charged { plan, payer, paid_until, .. } => {
                BillingUpdate { paid_until: Some(*paid_until), ..plan_update(*plan, payer) }
            }
            ChainEvent::Cancelled { .. } => BillingUpdate { cancelled: Some(true), ..Default::default() },
        }
    }
}

/// A decoded log and where it was seen (`<tx hash>:<log index>`).
pub type Seen = (ChainEvent, String);

/// The contract's subscription events, in log order. Logs from any other
/// address are ignored even if the node returned them.
pub fn decode_logs(logs: &[Value], checkout: &Address) -> Vec<Seen> {
    let contract = format!("0x{}", hex::encode(checkout));
    let (trial, charged, cancelled) = (trial_started_topic(), charged_topic(), cancelled_topic());
    let mut out = Vec::new();
    for l in logs {
        if !l["address"].as_str().is_some_and(|a| a.eq_ignore_ascii_case(&contract)) {
            continue;
        }
        let t: Vec<[u8; 32]> = l["topics"].as_array().map(|t| t.iter().filter_map(hex32).collect()).unwrap_or_default();
        let d = hex::decode(l["data"].as_str().unwrap_or("").trim_start_matches("0x")).unwrap_or_default();
        let at = format!("{}:{}", l["transactionHash"].as_str().unwrap_or(""), hex_u64(&l["logIndex"]).unwrap_or(0));
        let payer = |w: &[u8; 32]| format!("0x{}", hex::encode(&w[12..]));
        let e = match t.first() {
            Some(s) if *s == trial && t.len() == 4 && d.len() == 64 => ChainEvent::TrialStarted {
                account_key: t[1],
                plan: tail_u64(&t[2]) as u32,
                payer: payer(&t[3]),
                trial_ends: tail_u64(&d[..32]),
                payment_ref: d[32..64].try_into().expect("32 bytes"),
            },
            Some(s) if *s == charged && t.len() == 4 && d.len() == 64 => ChainEvent::Charged {
                account_key: t[1],
                plan: tail_u64(&t[2]) as u32,
                payer: payer(&t[3]),
                paid_until: tail_u64(&d[32..64]),
            },
            Some(s) if *s == cancelled && t.len() == 2 => ChainEvent::Cancelled { account_key: t[1] },
            _ => continue,
        };
        out.push((e, at));
    }
    out
}

/// Applies one event to its account, and confirms the website's payment
/// record when a trial start carries its ref. False for an account this
/// server never issued.
pub async fn record(store: &dyn Store, plans: &Plans, (event, at): &Seen) -> Result<bool, String> {
    let Some(account) = store.account_by_chain_key(event.account_key()).await.map_err(|e| e.to_string())? else {
        tracing::warn!(%at, "subscription event for an unknown account");
        return Ok(false);
    };
    store.update_billing(&account.id, &event.update(plans)).await.map_err(|e| e.to_string())?;
    if let ChainEvent::TrialStarted { payment_ref, .. } = event {
        let id = hex::encode(payment_ref);
        if let Some(p) = store.payment(&id).await.map_err(|e| e.to_string())?
            && p.account_id == account.id
        {
            store.confirm_payment(&id, at).await.map_err(|e| e.to_string())?;
        }
    }
    Ok(true)
}

/// Talks JSON-RPC to a Base node.
pub struct BaseChain {
    pub cfg: BaseConfig,
    http: reqwest::Client,
    next_block: AtomicU64,
}

/// Most blocks asked for in one `eth_getLogs`; public RPCs cap the range.
const MAX_RANGE: u64 = 2_000;

impl BaseChain {
    pub fn new(cfg: BaseConfig) -> Self {
        let http = reqwest::Client::builder().timeout(Duration::from_secs(10)).build().unwrap_or_default();
        let start = cfg.from_block.unwrap_or(0);
        BaseChain { cfg, http, next_block: AtomicU64::new(start) }
    }

    async fn rpc(&self, method: &str, params: Value) -> Result<Value, String> {
        let url = self.cfg.rpc_url.as_deref().ok_or("no RPC configured")?;
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        let r: Value = self
            .http
            .post(url)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("{method}: {e}"))?
            .json()
            .await
            .map_err(|e| format!("{method}: {e}"))?;
        if let Some(e) = r.get("error") {
            return Err(format!("{method}: {e}"));
        }
        Ok(r["result"].clone())
    }

    /// Whether `payer` must approve before subscribing at `cap`. Any doubt
    /// answers yes: an extra approval costs a click, a missing one fails.
    pub async fn needs_approve(&self, payer: &str, cap: u128) -> bool {
        let Ok(payer) = parse_address(payer) else { return true };
        let data = abi_call("allowance(address,address)", &[word_address(&payer), word_address(&self.cfg.checkout)]);
        match self.rpc("eth_call", json!([{"to": self.cfg.usdc_hex(), "data": data}, "latest"])).await {
            Ok(v) => hex32(&v).map(|w| u128::from_be_bytes(w[16..].try_into().expect("16")) < cap).unwrap_or(true),
            Err(_) => true,
        }
    }

    /// Scans the next range of settled blocks and records what it finds.
    pub async fn poll(&self, store: &dyn Store, plans: &Plans) -> Result<usize, String> {
        let head = hex_u64(&self.rpc("eth_blockNumber", json!([])).await?).ok_or("bad block number")?;
        let to = head.saturating_sub(self.cfg.confirmations);
        let mut from = self.next_block.load(Ordering::Relaxed);
        if from == 0 {
            from = to.saturating_sub(MAX_RANGE);
        }
        if from > to {
            return Ok(0);
        }
        let to = to.min(from + MAX_RANGE - 1);
        let topics = [trial_started_topic(), charged_topic(), cancelled_topic()].map(|t| format!("0x{}", hex::encode(t)));
        let filter = json!([{
            "address": self.cfg.checkout_hex(),
            "fromBlock": format!("0x{from:x}"),
            "toBlock": format!("0x{to:x}"),
            "topics": [topics],
        }]);
        let logs = self.rpc("eth_getLogs", filter).await?;
        let logs = logs.as_array().cloned().unwrap_or_default();
        let mut n = 0;
        for seen in decode_logs(&logs, &self.cfg.checkout) {
            if record(store, plans, &seen).await? {
                n += 1;
            }
        }
        self.next_block.store(to + 1, Ordering::Relaxed);
        Ok(n)
    }

    /// Polls forever. The block cursor moves only after a range is recorded.
    pub async fn run(self: Arc<Self>, store: Arc<dyn Store>, plans: Arc<Plans>, every: Duration) {
        loop {
            match self.poll(store.as_ref(), &plans).await {
                Ok(0) => {}
                Ok(n) => tracing::info!("recorded {n} subscription event(s) from Base"),
                Err(e) => tracing::warn!("base watcher: {e}"),
            }
            tokio::time::sleep(every).await;
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn selectors_match_the_known_values() {
        assert_eq!(hex::encode(selector("approve(address,uint256)")), "095ea7b3");
        assert_eq!(hex::encode(selector("allowance(address,address)")), "dd62ed3e");
        assert_eq!(hex::encode(selector("transfer(address,uint256)")), "a9059cbb");
    }

    #[test]
    fn the_built_in_plans_are_the_approved_ones() {
        let p = Plans::built_in();
        let pro = p.plan(2).unwrap();
        assert_eq!((pro.tier(), pro.interval(), pro.usd, pro.approval_usd()), (Tier::Pro, Interval::Month, 5, 60));
        assert_eq!(p.slugs(pro), vec!["ram", "threat"]);
        assert_eq!(p.plan(3).unwrap().approval_usd(), 20, "annual approves one year, once");
        assert!(p.plan(1).unwrap().trial_for(false) && !p.plan(1).unwrap().trial_for(true));
        assert!(!pro.trial_for(false), "Pro has no trial");
        assert!(Plans::from_json(r#"{"wardens":{},"plans":[{"id":1,"tier":"ram","name":"x","usd":1,"interval":"month","days":1,"wardens":[3]}]}"#).is_err());
    }

    pub fn base() -> BaseConfig {
        BaseConfig {
            chain_id: BASE_SEPOLIA,
            checkout: [0xcc; 20],
            usdc: BaseConfig::default_usdc(BASE_SEPOLIA).unwrap(),
            rpc_url: None,
            confirmations: 3,
            from_block: None,
        }
    }

    #[test]
    fn trial_transactions_approve_one_year_and_start_the_plan() {
        let plans = Plans::built_in();
        let plan = plans.plan(1).unwrap();
        let txs = base().transactions("acct1", plan, &[0xab; 32], true);
        assert_eq!(txs.len(), 2);
        let approve = txs[0]["data"].as_str().unwrap();
        assert!(approve.starts_with("0x095ea7b3"));
        assert!(approve.ends_with(&format!("{:064x}", 48_000_000)), "12 x $4, never unlimited");
        assert_eq!(txs[0]["to"], base().usdc_hex());

        let start = txs[1]["data"].as_str().unwrap();
        assert_eq!(&start[2..10], hex::encode(selector("startTrial(bytes32,uint32,uint256,bytes32)")));
        let args = &start[10..];
        assert_eq!(&args[..64], hex::encode(chain_key("acct1")));
        assert_eq!(&args[64..128], format!("{:064x}", 1));
        assert_eq!(&args[128..192], format!("{:064x}", 4_000_000), "maxPrice is one period");
        assert_eq!(&args[192..], hex::encode([0xab; 32]));
        assert_eq!(base().transactions("acct1", plan, &[0; 32], false).len(), 1, "no approve when allowed");
    }

    fn log(addr: &Address, topics: &[[u8; 32]], data: &[u8], tx: &str, index: u64) -> Value {
        json!({
            "address": format!("0x{}", hex::encode(addr)),
            "topics": topics.iter().map(|t| format!("0x{}", hex::encode(t))).collect::<Vec<_>>(),
            "data": format!("0x{}", hex::encode(data)),
            "transactionHash": tx,
            "logIndex": format!("0x{index:x}"),
        })
    }

    fn payer_word() -> [u8; 32] {
        let mut w = [0u8; 32];
        w[12..].copy_from_slice(&[0x11; 20]);
        w
    }

    pub fn trial_log(checkout: &Address, account: [u8; 32], plan: u32, trial_ends: u64, payment_ref: [u8; 32]) -> Value {
        let mut data = word_uint(u128::from(trial_ends)).to_vec();
        data.extend_from_slice(&payment_ref);
        log(checkout, &[trial_started_topic(), account, word_uint(u128::from(plan)), payer_word()], &data, "0xt1", 0)
    }

    pub fn charged_log(checkout: &Address, account: [u8; 32], plan: u32, paid_until: u64) -> Value {
        let mut data = word_uint(4_000_000).to_vec();
        data.extend_from_slice(&word_uint(u128::from(paid_until)));
        log(checkout, &[charged_topic(), account, word_uint(u128::from(plan)), payer_word()], &data, "0xt2", 3)
    }

    pub fn cancelled_log(checkout: &Address, account: [u8; 32]) -> Value {
        log(checkout, &[cancelled_topic(), account], &word_uint(1), "0xt3", 1)
    }

    #[test]
    fn logs_decode_into_subscription_events_and_ignore_other_contracts() {
        let c = [0xcc; 20];
        let logs = vec![
            trial_log(&c, chain_key("a"), 2, 500, [7; 32]),
            trial_log(&[0xdd; 20], chain_key("a"), 2, 9_999, [7; 32]),
            charged_log(&c, chain_key("a"), 2, 600),
            cancelled_log(&c, chain_key("a")),
        ];
        let seen = decode_logs(&logs, &c);
        assert_eq!(seen.len(), 3);
        assert_eq!(
            seen[0],
            (
                ChainEvent::TrialStarted {
                    account_key: chain_key("a"),
                    plan: 2,
                    payer: format!("0x{}", "11".repeat(20)),
                    trial_ends: 500,
                    payment_ref: [7; 32]
                },
                "0xt1:0".to_string()
            )
        );
        let plans = Plans::built_in();
        let u = seen[0].0.update(&plans);
        assert_eq!((u.tier, u.paid_until, u.trial_ends_at, u.trial_used), (Some(Tier::Pro), Some(500), Some(500), Some(true)));
        assert_eq!(seen[1].0.update(&plans).paid_until, Some(600));
        assert_eq!(seen[2].0.update(&plans).cancelled, Some(true));

        // Pro, or a used trial: no trial dates, and the trial stays available.
        let paid_now = decode_logs(&[trial_log(&c, chain_key("b"), 2, 0, [8; 32])], &c);
        let u = paid_now[0].0.update(&plans);
        assert_eq!((u.tier, u.paid_until, u.trial_ends_at, u.trial_used), (Some(Tier::Pro), None, None, None));
    }
}
