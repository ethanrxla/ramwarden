//! The storage the hosted API needs, behind one trait.
//!
//! [`crate::mongo::MongoStore`] is production (MongoDB Atlas). [`MemoryStore`]
//! backs the tests and a single development instance; it is correct but
//! forgets everything on restart. Routes see only [`Store`], so both pass the
//! same route tests.
//!
//! Device codes, device tokens, web sessions and email codes are bearer
//! secrets: only their SHA-256 is stored, and the raw value is returned once,
//! to its owner.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use rand::RngCore;
use sha2::{Digest, Sha256};

use crate::model::*;

/// 12 random bytes as hex, the size of a MongoDB ObjectId.
pub fn new_id() -> Id {
    hex(&random_bytes::<12>())
}

pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    rand::rng().fill_bytes(&mut b);
    b
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A bearer secret: 32 random bytes.
pub fn random_token(prefix: &str) -> String {
    format!("{prefix}{}", hex(&random_bytes::<32>()))
}

/// SHA-256 of a secret, hex. What the store keeps instead of the secret.
pub fn hash(raw: &str) -> String {
    hex(&Sha256::digest(raw.trim().as_bytes()))
}

/// The site's alphabet (`web/assets/api.js`): consonants only, so a code
/// can't spell a word and has no 0/O or 1/I to misread.
pub const USER_CODE_ALPHABET: &[u8] = b"BCDFGHJKLMNPQRSTVWXZ";

pub fn user_code() -> String {
    let mut out = String::with_capacity(9);
    for (i, b) in random_bytes::<8>().iter().enumerate() {
        if i == 4 {
            out.push('-');
        }
        out.push(USER_CODE_ALPHABET[*b as usize % USER_CODE_ALPHABET.len()] as char);
    }
    out
}

/// `XXXX-XXXX` in the site's alphabet, from whatever the person typed.
pub fn normalise_user_code(raw: &str) -> Option<String> {
    let s = raw.chars().filter(|c| !matches!(c, '-' | ' ')).collect::<String>().to_ascii_uppercase();
    if s.len() != 8 || !s.bytes().all(|b| USER_CODE_ALPHABET.contains(&b)) {
        return None;
    }
    Some(format!("{}-{}", &s[..4], &s[4..]))
}

/// Six digits for an email sign-in.
pub fn email_code() -> String {
    let n = u32::from_be_bytes(random_bytes::<4>()) % 1_000_000;
    format!("{n:06}")
}

/// Wrong guesses allowed on one email code before it is burned.
pub const EMAIL_CODE_ATTEMPTS: u32 = 5;
/// Minimum seconds between two codes to one address.
pub const EMAIL_RESEND_SECONDS: u64 = 60;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("storage is unavailable: {0}")]
    Unavailable(String),
}

impl From<mongodb::error::Error> for StoreError {
    fn from(e: mongodb::error::Error) -> Self {
        StoreError::Unavailable(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// Encrypted short-lived OAuth state. Raw browser tickets are never stored.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct OAuthState {
    pub encrypted: String,
    pub expires_at: u64,
    pub next_poll: u64,
    pub lease: Option<String>,
}
pub enum OAuthClaim { Ready(OAuthState), Wait(u64), Gone }

#[async_trait]
/// Everything the hosted API stores. The site wardens' conversations are a
/// trait of their own ([`crate::support::SupportStore`]) so this one stays about
/// accounts and money.
pub trait Store: crate::support::SupportStore + crate::usage::UsageStore + Send + Sync {
    async fn put_oauth(&self, ticket: &str, state: OAuthState, now:u64) -> Result<()>;
    /// Atomically leases one poll. Abandoned leases expire after 90 seconds.
    async fn claim_oauth(&self, ticket: &str, now: u64, lease: &str) -> Result<OAuthClaim>;
    /// Only the current lease may replace or consume this attempt.
    async fn finish_oauth(&self, ticket: &str, lease: &str, state: Option<OAuthState>) -> Result<bool>;
    // --- device sign-in (RFC 8628)
    /// A new attempt. Returns the raw device code, for the daemon only.
    async fn start_device_code(&self, name: &str, platform: Platform, now: u64, ttl: u64) -> Result<(String, DeviceCode)>;
    async fn pending_device_code(&self, user_code: &str, now: u64) -> Result<Option<DeviceCode>>;
    /// Pending → approved. False if it was not pending.
    async fn approve_device_code(&self, user_code: &str, account_id: &str, now: u64) -> Result<bool>;
    /// Approved → gone, once. The daemon's poll.
    async fn claim_device_code(&self, raw_device_code: &str, now: u64) -> Result<Claim>;

    // --- wallet sign-in
    async fn put_challenge(&self, nonce: &str, c: Challenge, now: u64) -> Result<()>;
    /// Removes it: a challenge is good for one verify, right or wrong.
    async fn take_challenge(&self, nonce: &str, now: u64) -> Result<Option<Challenge>>;

    // --- email sign-in
    /// Stores a code for `email`. False when one was sent less than
    /// [`EMAIL_RESEND_SECONDS`] ago.
    async fn start_email_code(&self, email: &str, code: &str, user_code: Option<&str>, now: u64, ttl: u64) -> Result<bool> {
        self.start_email_code_for(email, code, user_code, None, now, ttl).await
    }
    async fn start_email_code_for(&self, email: &str, code: &str, user_code: Option<&str>, link_account: Option<&str>, now: u64, ttl: u64) -> Result<bool>;
    /// A right code is used up; [`EMAIL_CODE_ATTEMPTS`] wrong ones burn it.
    async fn check_email_code(&self, email: &str, code: &str, now: u64) -> Result<EmailCheck>;

    // --- accounts
    /// The account for this identity, created on first sign-in. The flag is
    /// true when it was just created.
    async fn account_for_identity(&self, identity: &Identity, now: u64) -> Result<(Account, bool)>;
    /// Add a verified identity, refusing ownership by another account.
    async fn link_identity(&self, account_id: &str, identity: &Identity) -> Result<bool>;
    async fn identities(&self, account_id: &str) -> Result<Vec<Identity>>;
    async fn account(&self, id: &str) -> Result<Option<Account>>;
    async fn account_by_chain_key(&self, key: &[u8; 32]) -> Result<Option<Account>>;
    async fn update_billing(&self, account_id: &str, u: &BillingUpdate) -> Result<Option<Account>>;
    async fn set_prefs(&self, account_id: &str, prefs: serde_json::Map<String, serde_json::Value>) -> Result<Option<Account>>;

    // --- web sessions
    async fn create_session(&self, account_id: &str, now: u64, ttl: u64) -> Result<String>;
    async fn session_account(&self, raw: &str, now: u64) -> Result<Option<Id>>;
    async fn end_session(&self, raw: &str) -> Result<()>;

    // --- devices
    /// The device and its raw token, or `None` when the account already has
    /// `device_limit` devices.
    async fn register_device(&self, account_id: &str, name: &str, platform: Platform, now: u64) -> Result<Option<(Device, String)>>;
    /// The live device a token belongs to; records that it was seen.
    async fn device_for_token(&self, raw: &str, now: u64) -> Result<Option<Device>>;
    async fn bind_security(&self, account_id:&str, device_id:&str, binding:&SecurityBinding)->Result<bool>;
    async fn devices(&self, account_id: &str) -> Result<Vec<Device>>;
    /// Deletes the device and so its token. False if not this account's.
    async fn revoke_device(&self, account_id: &str, device_id: &str) -> Result<bool>;

    // --- payments
    async fn create_payment(&self, p: Payment) -> Result<()>;
    async fn payment(&self, id: &str) -> Result<Option<Payment>>;
    /// Pending → confirmed. False if it was not pending.
    async fn confirm_payment(&self, id: &str, external_id: &str) -> Result<bool>;

    // --- usage
    /// Counts one hosted decision today and returns today's count.
    async fn count_use(&self, account_id: &str, now: u64) -> Result<u32>;
    async fn used_today(&self, account_id: &str, now: u64) -> Result<u32>;
}

#[derive(Default)]
struct Inner {
    oauth: HashMap<String, OAuthState>,
    accounts: HashMap<Id, Account>,
    identities: HashMap<Identity, Id>,
    device_codes: HashMap<String, DeviceCode>,
    challenges: HashMap<String, Challenge>,
    email_codes: HashMap<String, EmailCode>,
    sessions: HashMap<String, (Id, u64)>,
    devices: HashMap<Id, Device>,
    payments: HashMap<Id, Payment>,
    usage: HashMap<(Id, u64), u32>,
}

/// Everything in one mutex. Fine for one instance and for tests.
#[derive(Default)]
pub struct MemoryStore {
    inner: Mutex<Inner>,
    support: crate::support::MemorySupport,
    pub(crate) usage: crate::usage::MemoryUsage,
}

impl MemoryStore {
    pub(crate) fn support(&self) -> std::sync::MutexGuard<'_, HashMap<Id, crate::support::Conversation>> {
        self.support.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }
}

#[async_trait]
impl Store for MemoryStore {
    async fn put_oauth(&self,ticket:&str,state:OAuthState,now:u64)->Result<()> {
        let mut g=self.lock();
        g.oauth.retain(|_,p|p.expires_at>now);
        if g.oauth.len()>=128 || g.oauth.contains_key(&hash(ticket)) {return Err(StoreError::Unavailable("OAuth capacity reached".into()))}
        if g.oauth.insert(hash(ticket),state).is_some() {return Err(StoreError::Unavailable("Duplicate OAuth attempt".into()))}
        Ok(())
    }
    async fn claim_oauth(&self,ticket:&str,now:u64,lease:&str)->Result<OAuthClaim> {
        let mut g=self.lock();let key=hash(ticket);
        g.oauth.retain(|_,p|p.expires_at>now);
        let Some(p)=g.oauth.get_mut(&key) else{return Ok(OAuthClaim::Gone)};
        if p.next_poll>now {return Ok(OAuthClaim::Wait(p.next_poll-now))}
        p.next_poll=now+90;p.lease=Some(lease.into());Ok(OAuthClaim::Ready(p.clone()))
    }
    async fn finish_oauth(&self,ticket:&str,lease:&str,state:Option<OAuthState>)->Result<bool> {
        let mut g=self.lock();let key=hash(ticket);
        if !g.oauth.get(&key).is_some_and(|p|p.lease.as_deref()==Some(lease)){return Ok(false)}
        if let Some(state)=state {g.oauth.insert(key,state);} else {g.oauth.remove(&key);}Ok(true)
    }

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
            device_code_hash: hash(&raw),
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
                c.status = DeviceCodeStatus::Approved { account_id: account_id.to_string() };
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn claim_device_code(&self, raw_device_code: &str, now: u64) -> Result<Claim> {
        let h = hash(raw_device_code);
        let mut g = self.lock();
        let Some(key) = g.device_codes.iter().find(|(_, c)| c.device_code_hash == h).map(|(k, _)| k.clone()) else {
            return Ok(Claim::Gone);
        };
        let c = g.device_codes[&key].clone();
        if c.expires_at <= now {
            g.device_codes.remove(&key);
            return Ok(Claim::Gone);
        }
        match c.status {
            DeviceCodeStatus::Pending => Ok(Claim::Pending),
            DeviceCodeStatus::Approved { account_id } => {
                g.device_codes.remove(&key);
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

    async fn start_email_code_for(&self, email: &str, code: &str, user_code: Option<&str>, link_account: Option<&str>, now: u64, ttl: u64) -> Result<bool> {
        let mut g = self.lock();
        g.email_codes.retain(|_, c| c.expires_at > now);
        if g.email_codes.get(email).is_some_and(|c| c.created_at + EMAIL_RESEND_SECONDS > now) {
            return Ok(false);
        }
        g.email_codes.insert(
            email.to_string(),
            EmailCode {
                link_account: link_account.map(str::to_string),
                code_hash: hash(code),
                user_code: user_code.map(str::to_string),
                created_at: now,
                expires_at: now + ttl,
                attempts: 0,
            },
        );
        Ok(true)
    }

    async fn check_email_code(&self, email: &str, code: &str, now: u64) -> Result<EmailCheck> {
        let mut g = self.lock();
        let Some(c) = g.email_codes.get_mut(email) else { return Ok(EmailCheck::Gone) };
        if c.expires_at <= now || c.attempts >= EMAIL_CODE_ATTEMPTS {
            g.email_codes.remove(email);
            return Ok(EmailCheck::Gone);
        }
        if c.code_hash == hash(code) {
            let c = g.email_codes.remove(email).expect("present");
            return Ok(EmailCheck::Ok { user_code: c.user_code, link_account: c.link_account });
        }
        c.attempts += 1;
        Ok(EmailCheck::Wrong)
    }

    async fn account_for_identity(&self, identity: &Identity, now: u64) -> Result<(Account, bool)> {
        let mut g = self.lock();
        if let Some(id) = g.identities.get(identity) {
            return Ok((g.accounts[id].clone(), false));
        }
        let a = Account::new(new_id(), identity.clone(), now);
        g.identities.insert(identity.clone(), a.id.clone());
        g.accounts.insert(a.id.clone(), a.clone());
        Ok((a, true))
    }

    async fn link_identity(&self, account_id: &str, identity: &Identity) -> Result<bool> {
        let mut g = self.lock();
        if !g.accounts.contains_key(account_id) { return Ok(false); }
        if let Some(owner) = g.identities.get(identity) { return Ok(owner == account_id); }
        g.identities.insert(identity.clone(), account_id.to_string());
        Ok(true)
    }
    async fn identities(&self, account_id: &str) -> Result<Vec<Identity>> {
        Ok(self.lock().identities.iter().filter(|(_, owner)| owner.as_str() == account_id)
            .map(|(identity, _)| identity.clone()).collect())
    }

    async fn account(&self, id: &str) -> Result<Option<Account>> {
        Ok(self.lock().accounts.get(id).cloned())
    }

    async fn account_by_chain_key(&self, key: &[u8; 32]) -> Result<Option<Account>> {
        Ok(self.lock().accounts.values().find(|a| &a.chain_key() == key).cloned())
    }

    async fn update_billing(&self, account_id: &str, u: &BillingUpdate) -> Result<Option<Account>> {
        let mut g = self.lock();
        Ok(g.accounts.get_mut(account_id).map(|a| {
            u.apply(a);
            a.clone()
        }))
    }

    async fn set_prefs(&self, account_id: &str, prefs: serde_json::Map<String, serde_json::Value>) -> Result<Option<Account>> {
        let mut g = self.lock();
        Ok(g.accounts.get_mut(account_id).map(|a| {
            a.prefs = prefs;
            a.clone()
        }))
    }

    async fn create_session(&self, account_id: &str, now: u64, ttl: u64) -> Result<String> {
        let raw = random_token("rwse_");
        let mut g = self.lock();
        g.sessions.retain(|_, (_, exp)| *exp > now);
        g.sessions.insert(hash(&raw), (account_id.to_string(), now + ttl));
        Ok(raw)
    }

    async fn session_account(&self, raw: &str, now: u64) -> Result<Option<Id>> {
        Ok(self.lock().sessions.get(&hash(raw)).filter(|(_, exp)| *exp > now).map(|(id, _)| id.clone()))
    }

    async fn end_session(&self, raw: &str) -> Result<()> {
        self.lock().sessions.remove(&hash(raw));
        Ok(())
    }

    async fn register_device(&self, account_id: &str, name: &str, platform: Platform, now: u64) -> Result<Option<(Device, String)>> {
        let mut g = self.lock();
        let Some(account) = g.accounts.get(account_id) else { return Ok(None) };
        let limit = account.effective_device_limit(now) as usize;
        if g.devices.values().filter(|d| d.account_id == account_id).count() >= limit {
            return Ok(None);
        }
        let raw = random_token("rwdt_");
        let d = Device {
            security:None,
            slot: None,
            id: new_id(),
            account_id: account_id.to_string(),
            name: name.to_string(),
            platform,
            created_at: now,
            last_seen_at: now,
            token_hash: hash(&raw),
        };
        g.devices.insert(d.id.clone(), d.clone());
        Ok(Some((d, raw)))
    }

    async fn device_for_token(&self, raw: &str, now: u64) -> Result<Option<Device>> {
        let h = hash(raw);
        let mut g = self.lock();
        Ok(g.devices.values_mut().find(|d| d.token_hash == h).map(|d| {
            d.last_seen_at = now;
            d.clone()
        }))
    }

    async fn bind_security(&self, account_id:&str, device_id:&str, binding:&SecurityBinding)->Result<bool> {
        let mut g=self.lock();
        if g.devices.values().any(|d|d.id!=device_id && d.security.as_ref().is_some_and(|b|b.agent_id==binding.agent_id)) {return Ok(false);}
        let Some(d)=g.devices.get_mut(device_id).filter(|d|d.account_id==account_id) else {return Ok(false)};
        if let Some(current)=&d.security {return Ok(current==binding);}
        d.security=Some(binding.clone());Ok(true)
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
        Ok(true)
    }

    async fn create_payment(&self, p: Payment) -> Result<()> {
        self.lock().payments.insert(p.id.clone(), p);
        Ok(())
    }

    async fn payment(&self, id: &str) -> Result<Option<Payment>> {
        Ok(self.lock().payments.get(id).cloned())
    }

    async fn confirm_payment(&self, id: &str, external_id: &str) -> Result<bool> {
        let mut g = self.lock();
        match g.payments.get_mut(id) {
            Some(p) if p.status == PaymentStatus::Pending => {
                p.status = PaymentStatus::Confirmed;
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

/// The behaviour every store must have, run against [`MemoryStore`] here and
/// against Atlas in `tests/atlas.rs`.
pub mod contract {
    use super::*;

    const NOW: u64 = 1_793_000_000;

    fn wallet(n: u8) -> Identity {
        Identity::Wallet { chain: Chain::Evm, address: format!("0x{:040x}", n) }
    }

    pub async fn run(s: &dyn Store) {
        let attempt=OAuthState{encrypted:"encrypted-fixture".into(),expires_at:NOW+1000,next_poll:NOW+5,lease:None};
        s.put_oauth("oauth-fixture",attempt.clone(),NOW).await.unwrap();
        assert!(matches!(s.claim_oauth("oauth-fixture",NOW,"lease-a").await.unwrap(),OAuthClaim::Wait(5)));
        assert!(matches!(s.claim_oauth("oauth-fixture",NOW+5,"lease-a").await.unwrap(),OAuthClaim::Ready(_)));
        assert!(matches!(s.claim_oauth("oauth-fixture",NOW+5,"lease-b").await.unwrap(),OAuthClaim::Wait(_)));
        assert!(!s.finish_oauth("oauth-fixture","lease-b",None).await.unwrap());
        assert!(matches!(s.claim_oauth("oauth-fixture",NOW+96,"lease-b").await.unwrap(),OAuthClaim::Ready(_)));
        assert!(!s.finish_oauth("oauth-fixture","lease-a",Some(attempt.clone())).await.unwrap());
        assert!(s.finish_oauth("oauth-fixture","lease-b",Some(attempt.clone())).await.unwrap());
        assert!(matches!(s.claim_oauth("oauth-fixture",NOW+1001,"lease-c").await.unwrap(),OAuthClaim::Gone));
        s.put_oauth("oauth-consume",attempt,NOW).await.unwrap();
        assert!(matches!(s.claim_oauth("oauth-consume",NOW+5,"lease-z").await.unwrap(),OAuthClaim::Ready(_)));
        assert!(s.finish_oauth("oauth-consume","lease-z",None).await.unwrap());
        assert!(matches!(s.claim_oauth("oauth-consume",NOW+6,"lease-y").await.unwrap(),OAuthClaim::Gone));

        let concurrent=OAuthState{encrypted:"opaque".into(),expires_at:NOW+1000,next_poll:NOW,lease:None};
        s.put_oauth("oauth-race",concurrent,NOW).await.unwrap();
        let (a,b)=tokio::join!(s.claim_oauth("oauth-race",NOW,"a"),s.claim_oauth("oauth-race",NOW,"b"));
        assert_eq!(usize::from(matches!(a.unwrap(),OAuthClaim::Ready(_)))+usize::from(matches!(b.unwrap(),OAuthClaim::Ready(_))),1,"one instance owns the poll");
        device_codes(s).await;
        email_codes(s).await;
        accounts_and_billing(s).await;
        identity_claims_are_atomic(s).await;
        diagnostics_retry_without_double_counting(s).await;
        devices_and_sessions(s).await;
        concurrent_seat_limits(s).await;
        security_bindings(s).await;
        challenges_and_payments(s).await;
        crate::support::contract::run(s).await;
    }

    async fn diagnostics_retry_without_double_counting(s:&dyn Store) {
        use crate::usage::{UsageRecord,UsageDay};
        use ramwarden_core::usage::Metric;
        let(account,_)=s.account_for_identity(&wallet(89),NOW).await.unwrap();
        for count in [2,1] {
            s.save_usage(UsageRecord {account_id:account.id.clone(),device_id:"contract-device".into(),platform:Platform::Macos,
                data:UsageDay {day:NOW/86400*86400,since:NOW,counters:[(Metric::AppsPaused,count)].into(),bytes_recovered:2048},received_at:NOW}).await.unwrap();
        }
        let rows=s.account_usage(&account.id).await.unwrap();assert_eq!(rows.len(),1);
        assert_eq!(rows[0].data.counters[&Metric::AppsPaused],2);
        s.save_usage(UsageRecord {account_id:account.id.clone(),device_id:"empty-counters".into(),platform:Platform::Windows,
            data:UsageDay {day:NOW/86400*86400,since:NOW,..Default::default()},received_at:NOW}).await.unwrap();
        assert_eq!(s.account_usage(&account.id).await.unwrap().len(),2,"empty summaries round trip");
    }

    async fn identity_claims_are_atomic(s: &dyn Store) {
        let identity = Identity::email("parallel-signin@example.com");
        let (first, second) = tokio::join!(s.account_for_identity(&identity,NOW),s.account_for_identity(&identity,NOW));
        let (first, second) = (first.unwrap(),second.unwrap());
        assert_eq!(first.0.id,second.0.id);
        assert_ne!(first.1,second.1,"only one creator");
        let target=s.account_for_identity(&wallet(77),NOW).await.unwrap().0;
        let identity=Identity::email("parallel-link@example.com");
        let (linked,signed)=tokio::join!(s.link_identity(&target.id,&identity),s.account_for_identity(&identity,NOW));
        let linked=linked.unwrap();let signed=signed.unwrap().0;
        assert_eq!(signed.id == target.id,linked,"a racing link cannot split ownership");
        assert_eq!(s.account_for_identity(&identity,NOW).await.unwrap().0.id,signed.id);
    }

    async fn device_codes(s: &dyn Store) {
        let (raw, dc) = s.start_device_code("desk", Platform::Linux, NOW, 600).await.unwrap();
        assert_eq!(s.claim_device_code(&raw, NOW + 1).await.unwrap(), Claim::Pending);
        assert!(s.approve_device_code(&dc.user_code, "acct", NOW + 2).await.unwrap());
        assert!(!s.approve_device_code(&dc.user_code, "other", NOW + 3).await.unwrap(), "approved once");
        assert!(s.pending_device_code(&dc.user_code, NOW + 3).await.unwrap().is_none());
        assert_eq!(
            s.claim_device_code(&raw, NOW + 4).await.unwrap(),
            Claim::Approved { account_id: "acct".into(), device_name: "desk".into(), platform: Platform::Linux }
        );
        assert_eq!(s.claim_device_code(&raw, NOW + 5).await.unwrap(), Claim::Gone, "claimed once");

        let (raw, dc) = s.start_device_code("desk", Platform::Macos, NOW, 600).await.unwrap();
        assert!(s.pending_device_code(&dc.user_code, NOW + 600).await.unwrap().is_none());
        assert!(!s.approve_device_code(&dc.user_code, "acct", NOW + 600).await.unwrap());
        assert_eq!(s.claim_device_code(&raw, NOW + 600).await.unwrap(), Claim::Gone, "expired");
    }

    async fn email_codes(s: &dyn Store) {
        let e = "contract@example.com";
        assert!(s.start_email_code(e, "123456", Some("BCDF-GHJK"), NOW, 600).await.unwrap());
        assert!(!s.start_email_code(e, "654321", None, NOW + 10, 600).await.unwrap(), "resend too soon");
        assert_eq!(s.check_email_code(e, "000000", NOW + 20).await.unwrap(), EmailCheck::Wrong);
        assert_eq!(
            s.check_email_code(e, "123456", NOW + 21).await.unwrap(),
            EmailCheck::Ok { user_code: Some("BCDF-GHJK".into()), link_account: None }
        );
        assert_eq!(s.check_email_code(e, "123456", NOW + 22).await.unwrap(), EmailCheck::Gone, "used once");

        assert!(s.start_email_code(e, "111111", None, NOW + 100, 600).await.unwrap());
        for _ in 0..EMAIL_CODE_ATTEMPTS {
            assert_eq!(s.check_email_code(e, "999999", NOW + 101).await.unwrap(), EmailCheck::Wrong);
        }
        assert_eq!(s.check_email_code(e, "111111", NOW + 102).await.unwrap(), EmailCheck::Gone, "burned");
    }

    async fn accounts_and_billing(s: &dyn Store) {
        let (a, created) = s.account_for_identity(&wallet(1), NOW).await.unwrap();
        assert!(created);
        let (b, created) = s.account_for_identity(&wallet(1), NOW + 9).await.unwrap();
        assert!(!created);
        assert_eq!(a.id, b.id, "one wallet, one account");
        let (c, _) = s.account_for_identity(&Identity::email("x@example.com"), NOW).await.unwrap();
        assert_ne!(a.id, c.id);
        assert_eq!(s.account_by_chain_key(&a.chain_key()).await.unwrap().unwrap().id, a.id);

        let second = Identity::email("linked@example.com");
        assert!(s.link_identity(&a.id, &second).await.unwrap());
        assert!(s.link_identity(&a.id, &second).await.unwrap(), "idempotent");
        assert!(!s.link_identity(&c.id, &second).await.unwrap(), "cannot take an identity");
        assert!(!s.link_identity(&a.id, &c.identity).await.unwrap(), "cannot merge another account");
        assert_eq!(s.account_for_identity(&second, NOW).await.unwrap().0.id, a.id);
        assert_eq!(s.identities(&a.id).await.unwrap().len(), 2);

        let start = BillingUpdate {
            tier: Some(Tier::Pro),
            interval: Some(Interval::TwoMonths),
            trial_ends_at: Some(NOW + 100),
            paid_until: Some(NOW + 100),
            trial_used: Some(true),
            billing: Some(Billing { method: BillingMethod::Base, reference: "0xpayer".into() }),
            ..Default::default()
        };
        let a = s.update_billing(&a.id, &start).await.unwrap().unwrap();
        assert_eq!(a.status(NOW), Some(Status::Trial));
        let a = s
            .update_billing(&a.id, &BillingUpdate { paid_until: Some(NOW + 50), ..Default::default() })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(a.paid_until, Some(NOW + 100), "a replayed older event cannot shorten service");
        assert!(a.trial_used);

        let mut prefs = serde_json::Map::new();
        prefs.insert("wardens".into(), serde_json::json!({"ram": {"color": "#ff8800"}}));
        let a = s.set_prefs(&a.id, prefs.clone()).await.unwrap().unwrap();
        assert_eq!(a.prefs, prefs);
        assert_eq!(s.account(&a.id).await.unwrap().unwrap().prefs, prefs);
    }

    async fn security_bindings(s:&dyn Store) {
        let (a,_)=s.account_for_identity(&wallet(85),NOW).await.unwrap();
        let (b,_)=s.account_for_identity(&wallet(86),NOW).await.unwrap();
        let da=s.register_device(&a.id,"a",Platform::Macos,NOW).await.unwrap().unwrap().0;
        let db=s.register_device(&b.id,"b",Platform::Macos,NOW).await.unwrap().unwrap().0;
        let binding=SecurityBinding{agent_id:"042".into(),agent_name:"private-agent".into(),group:"acct-a".into()};
        assert!(!s.bind_security(&b.id,&da.id,&binding).await.unwrap());
        assert!(s.bind_security(&a.id,&da.id,&binding).await.unwrap());
        assert!(s.bind_security(&a.id,&da.id,&binding).await.unwrap());
        assert!(!s.bind_security(&b.id,&db.id,&binding).await.unwrap(),"one manager agent cannot belong to two devices/accounts");
    }
    async fn concurrent_seat_limits(s: &dyn Store) {
        let (a, _) = s.account_for_identity(&wallet(88), NOW).await.unwrap();
        for (tier, expected) in [(None,1),(Some(Tier::Ram),3),(Some(Tier::Pro),5)] {
            if let Some(tier) = tier {
                s.update_billing(&a.id,&BillingUpdate {tier:Some(tier),paid_until:Some(NOW+100),..Default::default()}).await.unwrap();
            }
            let attempts = (0..8).map(|_|s.register_device(&a.id,"parallel",Platform::Macos,NOW));
            for result in futures::future::join_all(attempts).await { result.unwrap(); }
            assert_eq!(s.devices(&a.id).await.unwrap().len(),expected,"simultaneous registration must respect plan seats");
        }
    }

    async fn devices_and_sessions(s: &dyn Store) {
        let (a, _) = s.account_for_identity(&wallet(2), NOW).await.unwrap();
        let mut tokens = vec![];
        for i in 0..DEVICE_LIMIT {
            if i == 1 {
                assert!(s.register_device(&a.id, "Lite overflow", Platform::Linux, NOW).await.unwrap().is_none());
                s.update_billing(&a.id, &BillingUpdate {tier:Some(Tier::Ram),paid_until:Some(NOW+100),..Default::default()}).await.unwrap();
            }
            if i == 3 {
                assert!(s.register_device(&a.id, "Plus overflow", Platform::Linux, NOW).await.unwrap().is_none());
                s.update_billing(&a.id, &BillingUpdate {tier:Some(Tier::Pro),paid_until:Some(NOW+100),..Default::default()}).await.unwrap();
            }
            let (d, tok) = s.register_device(&a.id, &format!("pc{i}"), Platform::Windows, NOW).await.unwrap().unwrap();
            assert!(!d.id.is_empty());
            tokens.push((d, tok));
        }
        assert!(s.register_device(&a.id, "one too many", Platform::Linux, NOW).await.unwrap().is_none());
        let (d, tok) = &tokens[0];
        assert_eq!(s.device_for_token(tok, NOW + 5).await.unwrap().unwrap().last_seen_at, NOW + 5);
        assert!(!s.revoke_device("someone-else", &d.id).await.unwrap());
        assert!(s.revoke_device(&a.id, &d.id).await.unwrap());
        assert!(s.device_for_token(tok, NOW + 6).await.unwrap().is_none(), "a removed device's token stops");
        assert_eq!(s.devices(&a.id).await.unwrap().len(), DEVICE_LIMIT as usize - 1);
        assert!(s.register_device(&a.id, "fits again", Platform::Linux, NOW).await.unwrap().is_some());

        let raw = s.create_session(&a.id, NOW, 60).await.unwrap();
        assert_eq!(s.session_account(&raw, NOW + 59).await.unwrap(), Some(a.id.clone()));
        assert_eq!(s.session_account(&raw, NOW + 60).await.unwrap(), None);
        let raw = s.create_session(&a.id, NOW, 60).await.unwrap();
        s.end_session(&raw).await.unwrap();
        assert_eq!(s.session_account(&raw, NOW).await.unwrap(), None);
    }

    async fn challenges_and_payments(s: &dyn Store) {
        let c = Challenge { link_account: None, chain: Chain::Evm, address: "0xabc".into(), message: "m".into(), user_code: None, expires_at: NOW + 10 };
        s.put_challenge("n1", c.clone(), NOW).await.unwrap();
        assert!(s.take_challenge("n1", NOW).await.unwrap().is_some());
        assert!(s.take_challenge("n1", NOW).await.unwrap().is_none(), "single use");
        s.put_challenge("n2", c, NOW).await.unwrap();
        assert!(s.take_challenge("n2", NOW + 10).await.unwrap().is_none(), "expired");

        let id = hex(&random_bytes::<32>());
        s.create_payment(Payment {
            id: id.clone(),
            account_id: "a".into(),
            plan: 1,
            method: BillingMethod::Base,
            status: PaymentStatus::Pending,
            reason: None,
            payer: None,
            created_at: NOW,
            external_id: None,
        })
        .await
        .unwrap();
        assert!(s.confirm_payment(&id, "0xt:1").await.unwrap());
        assert!(!s.confirm_payment(&id, "0xt:1").await.unwrap(), "confirmed once");
        assert_eq!(s.payment(&id).await.unwrap().unwrap().status, PaymentStatus::Confirmed);

        assert_eq!(s.count_use("u", NOW).await.unwrap(), 1);
        assert_eq!(s.count_use("u", NOW + 1).await.unwrap(), 2);
        assert_eq!(s.used_today("u", NOW + 2).await.unwrap(), 2);
        assert_eq!(s.used_today("u", NOW + 86_400).await.unwrap(), 0, "a new UTC day");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_codes_are_in_the_sites_alphabet() {
        for _ in 0..200 {
            let c = user_code();
            assert_eq!(normalise_user_code(&c).as_deref(), Some(c.as_str()));
        }
        assert_eq!(normalise_user_code("bcdf ghjk").as_deref(), Some("BCDF-GHJK"));
        assert_eq!(normalise_user_code("BCDF-GHJ0"), None);
        assert_eq!(email_code().len(), 6);
    }

    #[tokio::test]
    async fn the_memory_store_keeps_the_store_contract() {
        contract::run(&MemoryStore::new()).await;
    }
}
