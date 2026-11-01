//! Selling warden passes: the plan list, the Base transactions the website
//! asks the wallet to send, and the watcher that credits a pass once the
//! chain shows it paid.
//!
//! # The server builds every transaction
//!
//! The page never composes calldata. `POST /v1/checkout/base` returns a USDC
//! `approve` (skipped when the payer's allowance already covers the price)
//! and `RamWardenCheckout.purchase(account, plan, maxPrice, ref)`, where
//! `account` is keccak-256 of the account id, `maxPrice` is the price the user
//! was shown, and `ref` is the payment id. The contract moves the USDC
//! straight to the treasury (see `contracts/README.md`).
//!
//! # The chain is the source of truth
//!
//! The watcher reads `Purchased` and `Extended` logs from the contract a few
//! blocks behind the head, and sets each warden's expiry to what `Extended`
//! says. It does not add days itself, so a log seen twice, a restart that
//! rescans, or a payment made straight to the contract without the website
//! (a gift) all land on the same expiry the contract holds.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::store::{Store, chain_key};
use crate::wallet::keccak;

/// `contracts/plans.json`: the one list of wardens and plans. The deploy
/// script writes the same file on-chain, and a contract test checks every
/// plan in it is valid for `setPlan`.
pub const PLANS_JSON: &str = include_str!("../../../contracts/plans.json");

#[derive(Clone, Debug, Deserialize)]
pub struct WardenDef {
    pub slug: String,
    pub name: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct PlanDef {
    pub id: u32,
    pub name: String,
    pub usd: u64,
    pub days: u64,
    pub wardens: Vec<u8>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Plans {
    pub wardens: BTreeMap<String, WardenDef>,
    pub plans: Vec<PlanDef>,
}

impl Plans {
    pub fn built_in() -> Self {
        Self::from_json(PLANS_JSON).expect("contracts/plans.json is valid; the contract tests check it")
    }

    pub fn from_json(text: &str) -> Result<Self, String> {
        let p: Plans = serde_json::from_str(text).map_err(|e| format!("plans.json: {e}"))?;
        for plan in &p.plans {
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

    /// The slug of on-chain warden id `w`.
    pub fn slug(&self, w: u8) -> Option<&str> {
        self.wardens.get(&w.to_string()).map(|d| d.slug.as_str())
    }

    pub fn slugs(&self, plan: &PlanDef) -> Vec<String> {
        plan.wardens.iter().filter_map(|&w| self.slug(w)).map(str::to_string).collect()
    }

    /// `GET /v1/checkout/config`.
    pub fn config_json(&self, base: Option<&BaseConfig>) -> Value {
        let plans: Vec<Value> = self
            .plans
            .iter()
            .map(|p| json!({"id": p.id, "name": p.name, "usd": p.usd, "days": p.days, "wardens": self.slugs(p)}))
            .collect();
        let wardens: Vec<Value> =
            self.wardens.values().map(|w| json!({"slug": w.slug, "name": w.name})).collect();
        json!({
            "plans": plans,
            "wardens": wardens,
            "base": base.map(|b| json!({"chain_id": b.chain_id, "network": b.network(),
                                        "contract": b.checkout_hex(), "usdc": b.usdc_hex()})),
            // Phantom pays on Base too, through its EVM account. Native Solana
            // Pay and Stripe are listed once the server can verify them.
            "solana": Value::Null,
            "stripe": false,
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

    /// The transactions for one purchase, in the order to send them.
    pub fn transactions(&self, account_id: &str, plan: &PlanDef, payment_ref: &[u8; 32], needs_approve: bool) -> Vec<Value> {
        let price = usdc_minor(plan.usd);
        let mut txs = Vec::new();
        if needs_approve {
            // Exactly the price, not unlimited: a wallet shows the amount, and
            // a later bug in the contract could not pull more.
            let data = abi_call("approve(address,uint256)", &[word_address(&self.checkout), word_uint(price)]);
            txs.push(json!({"to": self.usdc_hex(), "data": data, "value": "0x0"}));
        }
        let data = abi_call(
            "purchase(bytes32,uint32,uint256,bytes32)",
            &[chain_key(account_id), word_uint(u128::from(plan.id)), word_uint(price), *payment_ref],
        );
        txs.push(json!({"to": self.checkout_hex(), "data": data, "value": "0x0"}));
        txs
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

pub fn purchased_topic() -> [u8; 32] {
    keccak(b"Purchased(bytes32,uint32,address,uint256,uint32,bytes32)")
}

pub fn extended_topic() -> [u8; 32] {
    keccak(b"Extended(bytes32,uint8,uint64)")
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

/// One paid purchase, decoded from a transaction's logs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paid {
    pub account_key: [u8; 32],
    pub plan: u32,
    pub payment_ref: [u8; 32],
    pub payer: String,
    /// Warden slug → the expiry the contract now holds.
    pub until: BTreeMap<String, u64>,
    /// `<tx hash>:<log index>` of the `Purchased` log.
    pub external_id: String,
}

/// `Purchased` logs with their transaction's `Extended` logs for the same
/// account. Logs from anything but the checkout contract are ignored even if
/// the node returned them.
pub fn decode_logs(logs: &[Value], checkout: &Address, plans: &Plans) -> Vec<Paid> {
    let contract = format!("0x{}", hex::encode(checkout));
    let (purchased, extended) = (purchased_topic(), extended_topic());
    let ours = |l: &&Value| l["address"].as_str().is_some_and(|a| a.eq_ignore_ascii_case(&contract));
    let mut out = Vec::new();
    for l in logs.iter().filter(ours) {
        let topics: Vec<[u8; 32]> =
            l["topics"].as_array().map(|t| t.iter().filter_map(hex32).collect()).unwrap_or_default();
        if topics.len() != 4 || topics[0] != purchased {
            continue;
        }
        let data = hex::decode(l["data"].as_str().unwrap_or("").trim_start_matches("0x")).unwrap_or_default();
        if data.len() != 96 {
            continue;
        }
        let (tx, account) = (l["transactionHash"].as_str().unwrap_or(""), topics[1]);
        let mut until = BTreeMap::new();
        for e in logs.iter().filter(ours).filter(|e| e["transactionHash"].as_str() == Some(tx)) {
            let t: Vec<[u8; 32]> =
                e["topics"].as_array().map(|t| t.iter().filter_map(hex32).collect()).unwrap_or_default();
            let d = hex::decode(e["data"].as_str().unwrap_or("").trim_start_matches("0x")).unwrap_or_default();
            if t.len() == 3
                && t[0] == extended
                && t[1] == account
                && d.len() == 32
                && let Some(slug) = plans.slug(t[2][31])
            {
                until.insert(slug.to_string(), tail_u64(&d));
            }
        }
        out.push(Paid {
            account_key: account,
            plan: tail_u64(&topics[2]) as u32,
            payment_ref: data[64..96].try_into().expect("32 bytes"),
            payer: format!("0x{}", hex::encode(&topics[3][12..])),
            until,
            external_id: format!("{tx}:{}", hex_u64(&l["logIndex"]).unwrap_or(0)),
        });
    }
    out
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

    /// Whether `payer` must approve before paying `price`. Any doubt (no RPC,
    /// node down, bad address) answers yes: an extra approval costs a click, a
    /// missing one fails the purchase.
    pub async fn needs_approve(&self, payer: &str, price: u128) -> bool {
        let Ok(payer) = parse_address(payer) else { return true };
        let data = abi_call("allowance(address,address)", &[word_address(&payer), word_address(&self.cfg.checkout)]);
        match self.rpc("eth_call", json!([{"to": self.cfg.usdc_hex(), "data": data}, "latest"])).await {
            Ok(v) => hex32(&v).map(|w| u128::from_be_bytes(w[16..].try_into().expect("16")) < price).unwrap_or(true),
            Err(_) => true,
        }
    }

    /// Scans the next range of settled blocks and credits what it finds.
    /// Returns how many purchases were credited.
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
        let filter = json!([{
            "address": self.cfg.checkout_hex(),
            "fromBlock": format!("0x{from:x}"),
            "toBlock": format!("0x{to:x}"),
            "topics": [[format!("0x{}", hex::encode(purchased_topic())), format!("0x{}", hex::encode(extended_topic()))]],
        }]);
        let logs = self.rpc("eth_getLogs", filter).await?;
        let logs = logs.as_array().cloned().unwrap_or_default();
        let mut credited = 0;
        for paid in decode_logs(&logs, &self.cfg.checkout, plans) {
            if credit(store, &paid).await? {
                credited += 1;
            }
        }
        self.next_block.store(to + 1, Ordering::Relaxed);
        Ok(credited)
    }

    /// Polls forever. Errors are logged and retried; the block cursor only
    /// moves after a range is fully credited.
    pub async fn run(self: Arc<Self>, store: Arc<dyn Store>, plans: Arc<Plans>, every: Duration) {
        loop {
            match self.poll(store.as_ref(), &plans).await {
                Ok(0) => {}
                Ok(n) => tracing::info!("credited {n} Base purchase(s)"),
                Err(e) => tracing::warn!("base watcher: {e}"),
            }
            tokio::time::sleep(every).await;
        }
    }
}

/// Applies one purchase: expiries from the chain onto the account, and the
/// payment record (when the website made one) to confirmed.
pub async fn credit(store: &dyn Store, paid: &Paid) -> Result<bool, String> {
    let Some(account) = store.account_by_chain_key(&paid.account_key).await.map_err(|e| e.to_string())? else {
        // Paid for an account this server never issued. The USDC is in the
        // treasury; support can match it by the payer and the log.
        tracing::warn!(external_id = %paid.external_id, payer = %paid.payer, "purchase for an unknown account");
        return Ok(false);
    };
    store.extend_entitlements(&account.id, &paid.until).await.map_err(|e| e.to_string())?;
    let id = hex::encode(paid.payment_ref);
    if let Some(p) = store.payment(&id).await.map_err(|e| e.to_string())?
        && p.account_id == account.id
    {
        store.confirm_payment(&id, &paid.external_id, &paid.until).await.map_err(|e| e.to_string())?;
    }
    Ok(true)
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
    fn the_built_in_plans_load_and_name_their_wardens() {
        let p = Plans::built_in();
        let bundle = p.plans.iter().find(|p| p.wardens.len() == 2).unwrap();
        assert_eq!(p.slugs(bundle), vec!["ram", "threat"]);
        assert!(Plans::from_json(r#"{"wardens":{},"plans":[{"id":1,"name":"x","usd":1,"days":1,"wardens":[3]}]}"#).is_err());
    }

    fn base() -> BaseConfig {
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
    fn purchase_transactions_carry_the_account_key_price_and_ref() {
        let plans = Plans::built_in();
        let plan = plans.plan(1).unwrap();
        let txs = base().transactions("acct1", plan, &[0xab; 32], true);
        assert_eq!(txs.len(), 2);
        let approve = txs[0]["data"].as_str().unwrap();
        assert!(approve.starts_with("0x095ea7b3"));
        assert!(approve.ends_with(&format!("{:064x}", usdc_minor(plan.usd))), "approves exactly the price");
        assert_eq!(txs[0]["to"], base().usdc_hex());

        let purchase = txs[1]["data"].as_str().unwrap();
        assert_eq!(&purchase[2..10], hex::encode(selector("purchase(bytes32,uint32,uint256,bytes32)")));
        let args = &purchase[10..];
        assert_eq!(args.len(), 4 * 64);
        assert_eq!(&args[..64], hex::encode(chain_key("acct1")));
        assert_eq!(&args[64..128], format!("{:064x}", 1));
        assert_eq!(&args[128..192], format!("{:064x}", 4_000_000));
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

    pub fn purchase_logs(checkout: &Address, account: [u8; 32], plan: u32, payment_ref: [u8; 32], until: &[(u8, u64)], tx: &str) -> Vec<Value> {
        let mut logs = Vec::new();
        for (i, (w, t)) in until.iter().enumerate() {
            let mut warden = [0u8; 32];
            warden[31] = *w;
            logs.push(log(checkout, &[extended_topic(), account, warden], &word_uint(u128::from(*t)), tx, i as u64));
        }
        let mut payer = [0u8; 32];
        payer[12..].copy_from_slice(&[0x11; 20]);
        let mut data = Vec::new();
        data.extend_from_slice(&word_uint(4_000_000));
        data.extend_from_slice(&word_uint(3));
        data.extend_from_slice(&payment_ref);
        logs.push(log(checkout, &[purchased_topic(), account, word_uint(u128::from(plan)), payer], &data, tx, until.len() as u64));
        logs
    }

    #[test]
    fn logs_decode_into_purchases_with_the_contracts_expiries() {
        let plans = Plans::built_in();
        let c = [0xcc; 20];
        let mut logs = purchase_logs(&c, chain_key("a"), 5, [7; 32], &[(0, 100), (1, 200)], "0xt1");
        // A forged event from another contract in the same transaction.
        logs.extend(purchase_logs(&[0xdd; 20], chain_key("a"), 5, [7; 32], &[(0, 999)], "0xt1"));
        let paid = decode_logs(&logs, &c, &plans);
        assert_eq!(paid.len(), 1);
        let p = &paid[0];
        assert_eq!((p.plan, p.payment_ref, p.account_key), (5, [7; 32], chain_key("a")));
        assert_eq!(p.until, BTreeMap::from([("ram".into(), 100), ("threat".into(), 200)]));
        assert_eq!(p.payer, format!("0x{}", "11".repeat(20)));
        assert_eq!(p.external_id, "0xt1:2");
    }
}
