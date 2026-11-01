//! The accounts, devices, sign-ins and payments RamWarden's hosted service
//! keeps, per `docs/product-spec.md` 4.1.
//!
//! # An account is one identity and one subscription
//!
//! The identity is a wallet or a verified email and never changes. The
//! subscription is a tier (`ram` or `pro`), an interval, and the dates billing
//! last reported: when the trial ends, and how far service is paid. Billing
//! (the Base contract's logs, or Stripe's webhooks) only ever moves those
//! fields; [`Account::status`] derives trial, active, grace, lapsed or
//! cancelled from them at read time, so a status can never be stale because a
//! job that was meant to flip it did not run.
//!
//! RamWarden and Pro users are told apart by `tier`, which every hosted route
//! checks. ThreatWarden needs `pro`.
//!
//! Times are Unix seconds throughout.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha3::{Digest, Keccak256};

pub const ACCOUNTS: &str = "accounts";
pub const DEVICES: &str = "devices";
pub const DEVICE_CODES: &str = "device_codes";
pub const SESSIONS: &str = "sessions";
pub const CHALLENGES: &str = "challenges";
pub const EMAIL_CODES: &str = "email_codes";
pub const PAYMENTS: &str = "payments";
pub const USAGE_DAILY: &str = "usage_daily";

/// After a missed renewal, how long service continues while the user is told
/// to fix their payment (spec 3.3).
pub const GRACE_SECONDS: u64 = 3 * 86_400;
/// Devices one subscription covers (spec 7.1).
pub const DEVICE_LIMIT: u32 = 5;

pub type Id = String;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Chain {
    /// Any EVM chain: MetaMask, and Phantom's EVM accounts. One address works
    /// on all of them.
    Evm,
    Solana,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Identity {
    /// The address is normalised: lowercase hex for EVM, base58 as given for
    /// Solana.
    Wallet { chain: Chain, address: String },
    /// Lowercased and trimmed.
    Email { email: String },
}

impl Identity {
    pub fn email(raw: &str) -> Identity {
        Identity::Email { email: raw.trim().to_ascii_lowercase() }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// RamWarden: the RAM warden's hosted features.
    Ram,
    /// RamWarden Pro: everything in RamWarden, plus ThreatWarden.
    Pro,
}

impl Tier {
    /// The warden slugs a tier unlocks.
    pub fn wardens(self) -> &'static [&'static str] {
        match self {
            Tier::Ram => &["ram"],
            Tier::Pro => &["ram", "threat"],
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Ram => "ram",
            Tier::Pro => "pro",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Interval {
    Month,
    Year,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Trial,
    Active,
    /// A renewal failed; service continues for [`GRACE_SECONDS`].
    Grace,
    /// Paid time and grace are over: the app runs in local mode.
    Lapsed,
    /// Will not renew; service runs to `paid_until`.
    Cancelled,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Trial => "trial",
            Status::Active => "active",
            Status::Grace => "grace",
            Status::Lapsed => "lapsed",
            Status::Cancelled => "cancelled",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BillingMethod {
    Base,
    Solana,
    Card,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Billing {
    pub method: BillingMethod,
    /// Base: the paying wallet. Card: the Stripe subscription id.
    pub reference: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Account {
    #[serde(rename = "_id")]
    pub id: Id,
    pub created_at: u64,
    pub identity: Identity,
    #[serde(default)]
    pub tier: Option<Tier>,
    #[serde(default)]
    pub interval: Option<Interval>,
    #[serde(default)]
    pub trial_ends_at: Option<u64>,
    #[serde(default)]
    pub paid_until: Option<u64>,
    /// Set when the subscriber cancels; cleared when they subscribe again.
    #[serde(default)]
    pub cancelled: bool,
    /// One trial per account, ever (spec 4.2).
    #[serde(default)]
    pub trial_used: bool,
    #[serde(default)]
    pub billing: Option<Billing>,
    #[serde(default = "default_device_limit")]
    pub device_limit: u32,
    /// What the site and the apps share: warden colors, the pet-comment mute.
    #[serde(default)]
    pub prefs: serde_json::Map<String, serde_json::Value>,
}

fn default_device_limit() -> u32 {
    DEVICE_LIMIT
}

impl Account {
    pub fn new(id: Id, identity: Identity, now: u64) -> Self {
        Account {
            id,
            created_at: now,
            identity,
            tier: None,
            interval: None,
            trial_ends_at: None,
            paid_until: None,
            cancelled: false,
            trial_used: false,
            billing: None,
            device_limit: DEVICE_LIMIT,
            prefs: Default::default(),
        }
    }

    /// The subscription's state at `now`; `None` before anything was bought.
    pub fn status(&self, now: u64) -> Option<Status> {
        self.tier?;
        let until = self.paid_until.unwrap_or(0);
        Some(if now < until {
            if self.cancelled {
                Status::Cancelled
            } else if self.trial_ends_at.is_some_and(|t| now < t) {
                Status::Trial
            } else {
                Status::Active
            }
        } else if !self.cancelled && now < until + GRACE_SECONDS {
            Status::Grace
        } else {
            Status::Lapsed
        })
    }

    /// Until when hosted features are served: paid time, plus grace unless
    /// the subscriber cancelled.
    pub fn served_until(&self) -> u64 {
        match (self.tier, self.paid_until) {
            (Some(_), Some(t)) if self.cancelled => t,
            (Some(_), Some(t)) => t + GRACE_SECONDS,
            _ => 0,
        }
    }

    /// Each unlocked warden's expiry, for the signed entitlement.
    pub fn wardens(&self) -> BTreeMap<String, u64> {
        let until = self.served_until();
        match self.tier {
            Some(t) if until > 0 => t.wardens().iter().map(|w| (w.to_string(), until)).collect(),
            _ => BTreeMap::new(),
        }
    }

    /// Whether `warden` is served at `now`.
    pub fn active(&self, warden: &str, now: u64) -> bool {
        self.wardens().get(warden).is_some_and(|&t| t > now)
    }

    /// The account as the checkout contract knows it.
    pub fn chain_key(&self) -> [u8; 32] {
        chain_key(&self.id)
    }
}

/// keccak-256 of the account id: the chain never carries the id or a wallet
/// that signed in.
pub fn chain_key(id: &str) -> [u8; 32] {
    Keccak256::digest(id.as_bytes()).into()
}

/// What billing reports. `None` fields are left as they are.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BillingUpdate {
    pub tier: Option<Tier>,
    pub interval: Option<Interval>,
    pub trial_ends_at: Option<u64>,
    /// Moves forward only: a replayed event can't shorten service.
    pub paid_until: Option<u64>,
    pub cancelled: Option<bool>,
    pub billing: Option<Billing>,
    pub trial_used: Option<bool>,
}

impl BillingUpdate {
    pub fn apply(&self, a: &mut Account) {
        if let Some(t) = self.tier {
            a.tier = Some(t);
        }
        if let Some(i) = self.interval {
            a.interval = Some(i);
        }
        if let Some(t) = self.trial_ends_at {
            a.trial_ends_at = Some(t);
        }
        if let Some(t) = self.paid_until {
            a.paid_until = Some(a.paid_until.unwrap_or(0).max(t));
        }
        if let Some(c) = self.cancelled {
            a.cancelled = c;
        }
        if let Some(b) = &self.billing {
            a.billing = Some(b.clone());
        }
        if let Some(u) = self.trial_used {
            a.trial_used = a.trial_used || u;
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    Linux,
    Windows,
    Macos,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Device {
    #[serde(rename = "_id")]
    pub id: Id,
    pub account_id: Id,
    pub name: String,
    pub platform: Platform,
    pub created_at: u64,
    pub last_seen_at: u64,
    /// SHA-256 of the device token, hex.
    pub token_hash: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum DeviceCodeStatus {
    Pending,
    Approved { account_id: Id },
}

/// One daemon sign-in (RFC 8628).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeviceCode {
    /// `XXXX-XXXX`.
    #[serde(rename = "_id")]
    pub user_code: String,
    pub device_code_hash: String,
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

/// A sign-in message the server wrote and is waiting to see signed.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Challenge {
    pub chain: Chain,
    pub address: String,
    pub message: String,
    pub user_code: Option<String>,
    pub expires_at: u64,
}

/// A one-time code mailed to an address.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EmailCode {
    pub code_hash: String,
    pub user_code: Option<String>,
    pub created_at: u64,
    pub expires_at: u64,
    pub attempts: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EmailCheck {
    Ok { user_code: Option<String> },
    Wrong,
    /// Expired, used, or too many wrong tries: send a new one.
    Gone,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionKind {
    Web,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaymentStatus {
    Pending,
    Confirmed,
    Failed,
}

/// One checkout the website started, matched to what billing later reports.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Payment {
    /// 32 random bytes as hex. On Base the same bytes are `startTrial`'s
    /// `ref`, which is how the watcher matches the event to it.
    #[serde(rename = "_id")]
    pub id: Id,
    pub account_id: Id,
    pub plan: u32,
    pub method: BillingMethod,
    pub status: PaymentStatus,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub payer: Option<String>,
    pub created_at: u64,
    /// Where it was seen, e.g. `<tx hash>:<log index>`.
    #[serde(default)]
    pub external_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_793_000_000;
    const DAY: u64 = 86_400;

    fn account() -> Account {
        Account::new("a".into(), Identity::email(" Ethan@Example.com "), NOW)
    }

    #[test]
    fn emails_are_one_identity_whatever_their_case() {
        assert_eq!(account().identity, Identity::Email { email: "ethan@example.com".into() });
    }

    #[test]
    fn status_follows_the_dates_billing_reported() {
        let mut a = account();
        assert_eq!(a.status(NOW), None, "nothing bought");
        assert!(!a.active("ram", NOW));

        BillingUpdate {
            tier: Some(Tier::Pro),
            interval: Some(Interval::Month),
            trial_ends_at: Some(NOW + 14 * DAY),
            paid_until: Some(NOW + 14 * DAY),
            ..Default::default()
        }
        .apply(&mut a);
        assert_eq!(a.status(NOW), Some(Status::Trial));
        assert!(a.active("threat", NOW), "Pro unlocks ThreatWarden");

        let end = NOW + 14 * DAY;
        assert_eq!(a.status(end), Some(Status::Grace), "the first charge has not landed");
        assert!(a.active("ram", end + GRACE_SECONDS - 1));
        assert_eq!(a.status(end + GRACE_SECONDS), Some(Status::Lapsed));
        assert!(!a.active("ram", end + GRACE_SECONDS));

        BillingUpdate { paid_until: Some(end + 30 * DAY), ..Default::default() }.apply(&mut a);
        assert_eq!(a.status(end + 1), Some(Status::Active));
        BillingUpdate { cancelled: Some(true), ..Default::default() }.apply(&mut a);
        assert_eq!(a.status(end + 1), Some(Status::Cancelled));
        assert!(!a.active("ram", end + 30 * DAY), "no grace after a cancel");
    }

    #[test]
    fn ramwarden_does_not_unlock_threatwarden_and_dates_only_move_forward() {
        let mut a = account();
        let u = BillingUpdate { tier: Some(Tier::Ram), paid_until: Some(NOW + 100), ..Default::default() };
        u.apply(&mut a);
        assert!(a.active("ram", NOW) && !a.active("threat", NOW));
        BillingUpdate { paid_until: Some(NOW + 50), ..Default::default() }.apply(&mut a);
        assert_eq!(a.paid_until, Some(NOW + 100));
    }
}
