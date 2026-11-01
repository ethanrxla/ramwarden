//! Accounts, devices, sign-in attempts, sessions and payments: the state the
//! website and the daemon's sign-in need, behind one trait.
//!
//! The production store is MongoDB Atlas (`ramwarden-cloud`, which owns the
//! schema and the indexes that make the money rules atomic). Routes see only
//! [`Store`], so they are tested against [`MemoryStore`] and the Atlas store
//! slots in without touching them. `MemoryStore` is also what a single
//! instance runs on before the database is wired: correct, but forgets
//! everything on restart.
//!
//! # Per-warden passes
//!
//! An account holds one expiry per warden (`entitlements`, warden slug to Unix
//! seconds), not one tier, because passes are sold per warden and run out at
//! different times (see `contracts/plans.json`). Expiries only move forward:
//! [`Store::extend_entitlements`] keeps the later of the stored and the new
//! time, so replaying a payment, or the chain watcher seeing a block twice,
//! can never shorten or double a pass.
//!
//! # Secrets are stored hashed
//!
//! Device codes, device tokens and web sessions are bearer secrets. The store
//! keeps only their SHA-256; the raw value is returned once, to its owner.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use async_trait::async_trait;
use rand::RngCore;
use serde::{Deserialize, Serialize};

use crate::config::{TokenHash, hash_token};
use crate::wallet::{Chain, keccak};

pub type Id = String;

/// 12 random bytes as hex: the size of a MongoDB ObjectId, so ids survive the
/// move to Atlas unchanged.
pub fn new_id() -> Id {
    hex::encode(random_bytes::<12>())
}

pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    rand::rng().fill_bytes(&mut b);
    b
}

/// A bearer secret: 32 random bytes, URL-safe.
pub fn random_token(prefix: &str) -> String {
    format!("{prefix}{}", hex::encode(random_bytes::<32>()))
}

/// The site's alphabet (`web/assets/api.js`): consonants only, so a code can't
/// spell a word and has no 0/O or 1/I to misread. 20^8 ≈ 2.6e10.
pub const USER_CODE_ALPHABET: &[u8] = b"BCDFGHJKLMNPQRSTVWXZ";

pub fn user_code() -> String {
    let mut out = String::with_capacity(9);
    for (i, b) in random_bytes::<8>().iter().enumerate() {
        if i == 4 {
            out.push('-');
        }
        // 256 is not a multiple of 20; reject-free modulo bias here is 16/256
        // spread over the first 16 letters, harmless for a 10-minute code.
        out.push(USER_CODE_ALPHABET[*b as usize % USER_CODE_ALPHABET.len()] as char);
    }
    out
}

/// `XXXX-XXXX` in the site's alphabet, from whatever the person typed.
pub fn normalise_user_code(raw: &str) -> Option<String> {
    let s: String = raw.chars().filter(|c| !matches!(c, '-' | ' ')).collect::<String>().to_ascii_uppercase();
    if s.len() != 8 || !s.bytes().all(|b| USER_CODE_ALPHABET.contains(&b)) {
        return None;
    }
    Some(format!("{}-{}", &s[..4], &s[4..]))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    Linux,
    Windows,
    Macos,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WalletLink {
    pub chain: Chain,
    /// Normalised (see [`Chain::normalise`]).
    pub address: String,
    pub linked_at: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Account {
    pub id: Id,
    pub created_at: u64,
    pub wallets: Vec<WalletLink>,
    /// Warden slug → paid until (Unix seconds).
    pub entitlements: BTreeMap<String, u64>,
}

impl Account {
    pub fn active(&self, warden: &str, now: u64) -> bool {
        self.entitlements.get(warden).is_some_and(|&t| t > now)
    }

    /// The account as the checkout contract knows it: keccak-256 of the id, so
    /// the chain never carries the id or a wallet that signed in.
    pub fn chain_key(&self) -> [u8; 32] {
        chain_key(&self.id)
    }
}

pub fn chain_key(id: &str) -> [u8; 32] {
    keccak(id.as_bytes())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeviceCodeStatus {
    Pending,
    Approved(Id),
}

/// One daemon sign-in (RFC 8628). The daemon holds the device code and polls;
/// the person approves the user code on the website with their wallet.
#[derive(Clone, Debug)]
pub struct DeviceCode {
    pub user_code: String,
    pub device_code_hash: TokenHash,
    pub device_name: String,
    pub platform: Platform,
    pub status: DeviceCodeStatus,
    pub expires_at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Claim {
    /// Not approved yet: keep polling.
    Pending,
    /// Unknown, expired or already claimed: start again.
    Gone,
    Approved { account_id: Id, device_name: String, platform: Platform },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Device {
    pub id: Id,
    pub account_id: Id,
    pub name: String,
    pub platform: Platform,
    pub created_at: u64,
    pub last_seen_at: u64,
}

/// A sign-in message the server wrote and is waiting to see signed.
#[derive(Clone, Debug)]
pub struct Challenge {
    pub chain: Chain,
    pub address: String,
    pub message: String,
    pub user_code: Option<String>,
    pub expires_at: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaymentMethod {
    Base,
    Solana,
    Stripe,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaymentStatus {
    Pending,
    Confirmed,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Payment {
    /// 32 random bytes as hex. On Base the same bytes are the contract's
    /// `ref`, which is how the watcher matches a `Purchased` event to it.
    pub id: Id,
    pub account_id: Id,
    pub plan: u32,
    pub method: PaymentMethod,
    pub status: PaymentStatus,
    pub reason: Option<String>,
    pub payer: Option<String>,
    pub created_at: u64,
    /// Set on confirmation: the expiries the payment produced.
    pub wardens_until: BTreeMap<String, u64>,
    /// Where it was seen, e.g. `<tx hash>:<log index>`.
    pub external_id: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("storage is unavailable: {0}")]
    Unavailable(String),
}

pub type Result<T> = std::result::Result<T, StoreError>;

#[async_trait]
pub trait Store: Send + Sync {
    // --- device sign-in
    /// A new sign-in attempt. Returns the raw device code (for the daemon only).
    async fn start_device_code(&self, name: &str, platform: Platform, now: u64, ttl: u64) -> Result<(String, DeviceCode)>;
    /// A pending, unexpired attempt by its user code.
    async fn pending_device_code(&self, user_code: &str, now: u64) -> Result<Option<DeviceCode>>;
    /// Pending → approved by `account_id`. False if it was not pending.
    async fn approve_device_code(&self, user_code: &str, account_id: &str, now: u64) -> Result<bool>;
    /// Approved → claimed, once. The daemon's poll.
    async fn claim_device_code(&self, raw_device_code: &str, now: u64) -> Result<Claim>;

    // --- wallet sign-in
    async fn put_challenge(&self, nonce: &str, c: Challenge, now: u64) -> Result<()>;
    /// Removes it: a challenge is good for one verify, right or wrong.
    async fn take_challenge(&self, nonce: &str, now: u64) -> Result<Option<Challenge>>;
    /// The account that owns this wallet, created on first sign-in.
    async fn account_for_wallet(&self, chain: Chain, address: &str, now: u64) -> Result<Account>;
    async fn create_session(&self, account_id: &str, now: u64, ttl: u64) -> Result<String>;
    async fn session_account(&self, raw: &str, now: u64) -> Result<Option<Id>>;

    // --- accounts and devices
    async fn account(&self, id: &str) -> Result<Option<Account>>;
    async fn account_by_chain_key(&self, key: &[u8; 32]) -> Result<Option<Account>>;
    /// Moves each named warden's expiry to the later of stored and given.
    async fn extend_entitlements(&self, account_id: &str, until: &BTreeMap<String, u64>) -> Result<Option<Account>>;
    /// Returns the device and its raw token (for the daemon only).
    async fn register_device(&self, account_id: &str, name: &str, platform: Platform, now: u64) -> Result<(Device, String)>;
    /// The live device a token belongs to; records that it was seen.
    async fn device_for_token(&self, raw: &str, now: u64) -> Result<Option<Device>>;
    async fn devices(&self, account_id: &str) -> Result<Vec<Device>>;
    /// Deletes the device and with it its token. False if not this account's.
    async fn revoke_device(&self, account_id: &str, device_id: &str) -> Result<bool>;

    // --- payments
    async fn create_payment(&self, p: Payment) -> Result<()>;
    async fn payment(&self, id: &str) -> Result<Option<Payment>>;
    /// Pending → confirmed. False if it was not pending.
    async fn confirm_payment(&self, id: &str, external_id: &str, until: &BTreeMap<String, u64>) -> Result<bool>;

    // --- usage
    /// Counts one hosted decision for the account today and returns today's
    /// count including it.
    async fn count_use(&self, account_id: &str, now: u64) -> Result<u32>;
    async fn used_today(&self, account_id: &str, now: u64) -> Result<u32>;
}

#[derive(Default)]
struct Inner {
    accounts: HashMap<Id, Account>,
    wallets: HashMap<(Chain, String), Id>,
    chain_keys: HashMap<[u8; 32], Id>,
    device_codes: HashMap<String, DeviceCode>,
    challenges: HashMap<String, Challenge>,
    sessions: HashMap<TokenHash, (Id, u64)>,
    devices: HashMap<Id, Device>,
    device_tokens: HashMap<TokenHash, Id>,
    payments: HashMap<Id, Payment>,
    usage: HashMap<(Id, u64), u32>,
}

/// Everything in one mutex. Fine for one instance and for tests.
#[derive(Default)]
pub struct MemoryStore {
    inner: Mutex<Inner>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }
}

#[async_trait]
impl Store for MemoryStore {
    async fn start_device_code(&self, name: &str, platform: Platform, now: u64, ttl: u64) -> Result<(String, DeviceCode)> {
        let raw = random_token("rwdc_");
        let mut g = self.lock();
        g.device_codes.retain(|_, c| c.expires_at > now);
        let mut code = user_code();
        while g.device_codes.contains_key(&code) {
            code = user_code();
        }
        let dc = DeviceCode {
            user_code: code.clone(),
            device_code_hash: hash_token(&raw),
            device_name: name.to_string(),
            platform,
            status: DeviceCodeStatus::Pending,
            expires_at: now + ttl,
        };
        g.device_codes.insert(code, dc.clone());
        Ok((raw, dc))
    }

    async fn pending_device_code(&self, user_code: &str, now: u64) -> Result<Option<DeviceCode>> {
        Ok(self
            .lock()
            .device_codes
            .get(user_code)
            .filter(|c| c.expires_at > now && c.status == DeviceCodeStatus::Pending)
            .cloned())
    }

    async fn approve_device_code(&self, user_code: &str, account_id: &str, now: u64) -> Result<bool> {
        let mut g = self.lock();
        match g.device_codes.get_mut(user_code) {
            Some(c) if c.expires_at > now && c.status == DeviceCodeStatus::Pending => {
                c.status = DeviceCodeStatus::Approved(account_id.to_string());
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn claim_device_code(&self, raw_device_code: &str, now: u64) -> Result<Claim> {
        let h = hash_token(raw_device_code);
        let mut g = self.lock();
        let Some(key) = g.device_codes.iter().find(|(_, c)| c.device_code_hash == h).map(|(k, _)| k.clone()) else {
            return Ok(Claim::Gone);
        };
        let c = &g.device_codes[&key];
        if c.expires_at <= now {
            g.device_codes.remove(&key);
            return Ok(Claim::Gone);
        }
        match &c.status {
            DeviceCodeStatus::Pending => Ok(Claim::Pending),
            DeviceCodeStatus::Approved(_) => {
                let c = g.device_codes.remove(&key).expect("present");
                let DeviceCodeStatus::Approved(account_id) = c.status else { unreachable!() };
                Ok(Claim::Approved { account_id, device_name: c.device_name, platform: c.platform })
            }
        }
    }

    async fn put_challenge(&self, nonce: &str, c: Challenge, now: u64) -> Result<()> {
        let mut g = self.lock();
        g.challenges.retain(|_, c| c.expires_at > now);
        g.challenges.insert(nonce.to_string(), c);
        Ok(())
    }

    async fn take_challenge(&self, nonce: &str, now: u64) -> Result<Option<Challenge>> {
        Ok(self.lock().challenges.remove(nonce).filter(|c| c.expires_at > now))
    }

    async fn account_for_wallet(&self, chain: Chain, address: &str, now: u64) -> Result<Account> {
        let mut g = self.lock();
        let key = (chain, address.to_string());
        if let Some(id) = g.wallets.get(&key) {
            return Ok(g.accounts[id].clone());
        }
        let a = Account {
            id: new_id(),
            created_at: now,
            wallets: vec![WalletLink { chain, address: address.to_string(), linked_at: now }],
            entitlements: BTreeMap::new(),
        };
        g.wallets.insert(key, a.id.clone());
        g.chain_keys.insert(a.chain_key(), a.id.clone());
        g.accounts.insert(a.id.clone(), a.clone());
        Ok(a)
    }

    async fn create_session(&self, account_id: &str, now: u64, ttl: u64) -> Result<String> {
        let raw = random_token("rwse_");
        let mut g = self.lock();
        g.sessions.retain(|_, (_, exp)| *exp > now);
        g.sessions.insert(hash_token(&raw), (account_id.to_string(), now + ttl));
        Ok(raw)
    }

    async fn session_account(&self, raw: &str, now: u64) -> Result<Option<Id>> {
        Ok(self.lock().sessions.get(&hash_token(raw)).filter(|(_, exp)| *exp > now).map(|(id, _)| id.clone()))
    }

    async fn account(&self, id: &str) -> Result<Option<Account>> {
        Ok(self.lock().accounts.get(id).cloned())
    }

    async fn account_by_chain_key(&self, key: &[u8; 32]) -> Result<Option<Account>> {
        let g = self.lock();
        Ok(g.chain_keys.get(key).and_then(|id| g.accounts.get(id)).cloned())
    }

    async fn extend_entitlements(&self, account_id: &str, until: &BTreeMap<String, u64>) -> Result<Option<Account>> {
        let mut g = self.lock();
        let Some(a) = g.accounts.get_mut(account_id) else { return Ok(None) };
        for (w, &t) in until {
            let e = a.entitlements.entry(w.clone()).or_insert(0);
            *e = (*e).max(t);
        }
        Ok(Some(a.clone()))
    }

    async fn register_device(&self, account_id: &str, name: &str, platform: Platform, now: u64) -> Result<(Device, String)> {
        let raw = random_token("rwdt_");
        let d = Device {
            id: new_id(),
            account_id: account_id.to_string(),
            name: name.to_string(),
            platform,
            created_at: now,
            last_seen_at: now,
        };
        let mut g = self.lock();
        g.device_tokens.insert(hash_token(&raw), d.id.clone());
        g.devices.insert(d.id.clone(), d.clone());
        Ok((d, raw))
    }

    async fn device_for_token(&self, raw: &str, now: u64) -> Result<Option<Device>> {
        let mut g = self.lock();
        let Some(id) = g.device_tokens.get(&hash_token(raw)).cloned() else { return Ok(None) };
        Ok(g.devices.get_mut(&id).map(|d| {
            d.last_seen_at = now;
            d.clone()
        }))
    }

    async fn devices(&self, account_id: &str) -> Result<Vec<Device>> {
        let mut v: Vec<Device> = self.lock().devices.values().filter(|d| d.account_id == account_id).cloned().collect();
        v.sort_by_key(|d| d.created_at);
        Ok(v)
    }

    async fn revoke_device(&self, account_id: &str, device_id: &str) -> Result<bool> {
        let mut g = self.lock();
        if g.devices.get(device_id).is_none_or(|d| d.account_id != account_id) {
            return Ok(false);
        }
        g.devices.remove(device_id);
        g.device_tokens.retain(|_, id| id != device_id);
        Ok(true)
    }

    async fn create_payment(&self, p: Payment) -> Result<()> {
        self.lock().payments.insert(p.id.clone(), p);
        Ok(())
    }

    async fn payment(&self, id: &str) -> Result<Option<Payment>> {
        Ok(self.lock().payments.get(id).cloned())
    }

    async fn confirm_payment(&self, id: &str, external_id: &str, until: &BTreeMap<String, u64>) -> Result<bool> {
        let mut g = self.lock();
        match g.payments.get_mut(id) {
            Some(p) if p.status == PaymentStatus::Pending => {
                p.status = PaymentStatus::Confirmed;
                p.wardens_until = until.clone();
                p.external_id = Some(external_id.to_string());
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn count_use(&self, account_id: &str, now: u64) -> Result<u32> {
        let mut g = self.lock();
        let day = now / 86_400;
        g.usage.retain(|(_, d), _| *d + 2 > day);
        let n = g.usage.entry((account_id.to_string(), day)).or_insert(0);
        *n += 1;
        Ok(*n)
    }

    async fn used_today(&self, account_id: &str, now: u64) -> Result<u32> {
        Ok(self.lock().usage.get(&(account_id.to_string(), now / 86_400)).copied().unwrap_or(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_793_000_000;

    #[test]
    fn user_codes_are_in_the_sites_alphabet() {
        for _ in 0..200 {
            let c = user_code();
            assert_eq!(normalise_user_code(&c).as_deref(), Some(c.as_str()));
        }
        assert_eq!(normalise_user_code("bcdf ghjk").as_deref(), Some("BCDF-GHJK"));
        assert_eq!(normalise_user_code("BCDF-GHJ0"), None);
        assert_eq!(normalise_user_code("BCDF-GHJKL"), None);
    }

    #[tokio::test]
    async fn a_device_code_is_claimed_once_and_only_after_approval() {
        let s = MemoryStore::new();
        let (raw, dc) = s.start_device_code("desk", Platform::Linux, NOW, 600).await.unwrap();
        assert_eq!(s.claim_device_code(&raw, NOW + 1).await.unwrap(), Claim::Pending);
        assert!(s.approve_device_code(&dc.user_code, "acct", NOW + 2).await.unwrap());
        assert!(!s.approve_device_code(&dc.user_code, "other", NOW + 3).await.unwrap(), "approved once");
        assert!(s.pending_device_code(&dc.user_code, NOW + 3).await.unwrap().is_none());
        assert_eq!(
            s.claim_device_code(&raw, NOW + 4).await.unwrap(),
            Claim::Approved { account_id: "acct".into(), device_name: "desk".into(), platform: Platform::Linux }
        );
        assert_eq!(s.claim_device_code(&raw, NOW + 5).await.unwrap(), Claim::Gone);
    }

    #[tokio::test]
    async fn device_codes_expire() {
        let s = MemoryStore::new();
        let (raw, dc) = s.start_device_code("desk", Platform::Macos, NOW, 600).await.unwrap();
        assert!(s.pending_device_code(&dc.user_code, NOW + 600).await.unwrap().is_none());
        assert!(!s.approve_device_code(&dc.user_code, "acct", NOW + 600).await.unwrap());
        assert_eq!(s.claim_device_code(&raw, NOW + 600).await.unwrap(), Claim::Gone);
    }

    #[tokio::test]
    async fn one_wallet_is_one_account_and_passes_only_move_forward() {
        let s = MemoryStore::new();
        let a = s.account_for_wallet(Chain::Evm, "0xabc", NOW).await.unwrap();
        let b = s.account_for_wallet(Chain::Evm, "0xabc", NOW + 9).await.unwrap();
        assert_eq!(a.id, b.id);
        assert_eq!(s.account_by_chain_key(&a.chain_key()).await.unwrap().unwrap().id, a.id);

        let later = BTreeMap::from([("ram".to_string(), NOW + 100)]);
        let earlier = BTreeMap::from([("ram".to_string(), NOW + 50), ("wazuh".to_string(), NOW + 10)]);
        s.extend_entitlements(&a.id, &later).await.unwrap();
        let a = s.extend_entitlements(&a.id, &earlier).await.unwrap().unwrap();
        assert_eq!(a.entitlements["ram"], NOW + 100, "a replayed older payment cannot shorten a pass");
        assert!(a.active("wazuh", NOW) && !a.active("wazuh", NOW + 10));
    }

    #[tokio::test]
    async fn a_revoked_device_token_stops_working() {
        let s = MemoryStore::new();
        let (d, tok) = s.register_device("acct", "desk", Platform::Windows, NOW).await.unwrap();
        assert_eq!(s.device_for_token(&tok, NOW + 5).await.unwrap().unwrap().last_seen_at, NOW + 5);
        assert!(!s.revoke_device("someone-else", &d.id).await.unwrap());
        assert!(s.revoke_device("acct", &d.id).await.unwrap());
        assert!(s.device_for_token(&tok, NOW + 6).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn challenges_and_sessions_expire_and_challenges_are_single_use() {
        let s = MemoryStore::new();
        let c = Challenge {
            chain: Chain::Evm,
            address: "0xabc".into(),
            message: "m".into(),
            user_code: None,
            expires_at: NOW + 10,
        };
        s.put_challenge("n", c.clone(), NOW).await.unwrap();
        assert!(s.take_challenge("n", NOW).await.unwrap().is_some());
        assert!(s.take_challenge("n", NOW).await.unwrap().is_none());
        s.put_challenge("n", c, NOW).await.unwrap();
        assert!(s.take_challenge("n", NOW + 10).await.unwrap().is_none());

        let tok = s.create_session("acct", NOW, 60).await.unwrap();
        assert_eq!(s.session_account(&tok, NOW + 59).await.unwrap().as_deref(), Some("acct"));
        assert_eq!(s.session_account(&tok, NOW + 60).await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_payment_confirms_once() {
        let s = MemoryStore::new();
        s.create_payment(Payment {
            id: "p".into(),
            account_id: "a".into(),
            plan: 1,
            method: PaymentMethod::Base,
            status: PaymentStatus::Pending,
            reason: None,
            payer: None,
            created_at: NOW,
            wardens_until: BTreeMap::new(),
            external_id: None,
        })
        .await
        .unwrap();
        let u = BTreeMap::from([("ram".to_string(), NOW + 1)]);
        assert!(s.confirm_payment("p", "0xt:1", &u).await.unwrap());
        assert!(!s.confirm_payment("p", "0xt:1", &u).await.unwrap());
        assert_eq!(s.payment("p").await.unwrap().unwrap().status, PaymentStatus::Confirmed);
    }
}
