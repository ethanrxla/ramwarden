//! The documents RamWarden Cloud stores, one struct per collection.
//!
//! The shape follows the backend design: an account is not a wallet. One
//! account links any number of wallets (and later an email or a Stripe
//! customer), and payments only ever move one field, `entitlement_until`, which
//! the API checks on every request. How someone paid never reaches the daemon.
//!
//! Wallets are embedded in the account because they are few, always read with
//! it, and looked up by a unique multikey index. Everything that grows without
//! bound — devices, tokens, payments, daily usage — is its own collection keyed
//! by `account_id`.

use mongodb::bson::{DateTime, oid::ObjectId};
use serde::{Deserialize, Serialize};

pub const ACCOUNTS: &str = "accounts";
pub const DEVICES: &str = "devices";
pub const DEVICE_CODES: &str = "device_codes";
pub const REFRESH_TOKENS: &str = "refresh_tokens";
pub const PAYMENTS: &str = "payments";
pub const USAGE_DAILY: &str = "usage_daily";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Chain {
    /// Any EVM chain; MetaMask, and Phantom on Base. One address works on all.
    Evm,
    Solana,
}

impl Chain {
    /// EVM addresses are case-insensitive hex (EIP-55 case is only a
    /// checksum), so they are stored lowercased or one wallet becomes two
    /// accounts. Solana addresses are base58 and case-sensitive.
    pub fn normalise(self, address: &str) -> String {
        match self {
            Chain::Evm => address.trim().to_ascii_lowercase(),
            Chain::Solana => address.trim().to_string(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Wallet {
    pub chain: Chain,
    pub address: String,
    pub linked_at: DateTime,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Account {
    #[serde(rename = "_id")]
    pub id: ObjectId,
    pub created_at: DateTime,
    #[serde(default)]
    pub wallets: Vec<Wallet>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stripe_customer_id: Option<String>,
    /// Pro until this instant. `None`, or a time in the past, is the free tier.
    #[serde(default)]
    pub entitlement_until: Option<DateTime>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    Free,
    Pro,
}

impl Account {
    pub fn tier_at(&self, now: DateTime) -> Tier {
        match self.entitlement_until {
            Some(t) if t > now => Tier::Pro,
            _ => Tier::Free,
        }
    }
}

/// Limits from the design doc's pricing table. Kept here so the API and the
/// quota check cannot disagree.
impl Tier {
    pub fn daily_cloud_analyses(self) -> i32 {
        match self {
            Tier::Free => 5,
            Tier::Pro => 200,
        }
    }

    pub fn max_devices(self) -> usize {
        match self {
            Tier::Free => 1,
            Tier::Pro => 3,
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

/// A machine running the daemon. Only the daemon talks to the cloud, so a
/// device is a daemon install, not a browser.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Device {
    #[serde(rename = "_id")]
    pub id: ObjectId,
    pub account_id: ObjectId,
    pub name: String,
    pub platform: Platform,
    pub created_at: DateTime,
    pub last_seen_at: DateTime,
    #[serde(default)]
    pub revoked_at: Option<DateTime>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceCodeStatus {
    Pending,
    Approved,
    /// The daemon has exchanged it for tokens; it cannot be used again.
    Claimed,
}

/// One sign-in attempt in the device flow (RFC 8628). The daemon holds the
/// device code and polls with it; the person types or follows the user code on
/// the link page and signs with their wallet. A TTL index deletes it at
/// `expires_at`, so an abandoned sign-in leaves nothing behind.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeviceCode {
    #[serde(rename = "_id")]
    pub id: ObjectId,
    /// SHA-256 of the device code. The code itself is a bearer secret.
    pub device_code_hash: String,
    /// The short `XXXX-XXXX` code shown to the person.
    pub user_code: String,
    /// Fresh per attempt; the SIWE/SIWS message must quote it.
    pub nonce: String,
    pub device_name: String,
    pub platform: Platform,
    pub status: DeviceCodeStatus,
    #[serde(default)]
    pub account_id: Option<ObjectId>,
    pub created_at: DateTime,
    pub expires_at: DateTime,
}

/// A refresh token, stored only as its hash. Rotated on every use: the old one
/// is marked `rotated_at` and a new one issued, so a stolen token that is used
/// after the real daemon has refreshed is visible as reuse.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RefreshToken {
    #[serde(rename = "_id")]
    pub id: ObjectId,
    pub token_hash: String,
    pub account_id: ObjectId,
    pub device_id: ObjectId,
    pub created_at: DateTime,
    pub expires_at: DateTime,
    #[serde(default)]
    pub rotated_at: Option<DateTime>,
    #[serde(default)]
    pub revoked_at: Option<DateTime>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaymentMethod {
    /// USDC through the `RamWardenCheckout` contract on Base.
    BaseUsdc,
    /// A Solana Pay USDC transfer, matched by its reference key.
    SolanaPay,
    Stripe,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Plan {
    #[serde(rename = "pro_30d")]
    Pro30d,
    #[serde(rename = "pro_365d")]
    Pro365d,
}

impl Plan {
    pub fn days(self) -> i64 {
        match self {
            Plan::Pro30d => 30,
            Plan::Pro365d => 365,
        }
    }
}

/// One confirmed payment. `(method, external_id)` is unique, so a webhook
/// delivered twice, or a chain watcher that replays a block, cannot extend a
/// pass twice.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Payment {
    #[serde(rename = "_id")]
    pub id: ObjectId,
    pub account_id: ObjectId,
    pub method: PaymentMethod,
    /// Base: `<tx hash>:<log index>`. Solana: the reference key. Stripe: the
    /// invoice id.
    pub external_id: String,
    pub plan: Plan,
    /// In the currency's smallest unit: USDC has 6 decimals, USD cents 2.
    pub amount_minor: i64,
    pub currency: String,
    /// The paying wallet, when there is one. Kept for sanctions screening and
    /// refunds; it need not be a wallet linked to the account (gifts).
    #[serde(default)]
    pub payer: Option<String>,
    pub created_at: DateTime,
}

/// Cloud analyses used by one account on one UTC day.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UsageDay {
    #[serde(rename = "_id")]
    pub id: ObjectId,
    pub account_id: ObjectId,
    /// `YYYY-MM-DD`, UTC.
    pub day: String,
    pub cloud_analyses: i32,
    #[serde(default)]
    pub input_tokens: i64,
    /// TTL: usage older than this is deleted. Counts and timings only — the
    /// server never stores request bodies.
    pub expire_at: DateTime,
}

#[cfg(test)]
mod tests {
    use super::*;
    use mongodb::bson;

    #[test]
    fn evm_addresses_are_one_wallet_whatever_their_case() {
        let a = Chain::Evm.normalise("0xAbC0000000000000000000000000000000000dEf");
        let b = Chain::Evm.normalise(" 0xabc0000000000000000000000000000000000def ");
        assert_eq!(a, b);
    }

    #[test]
    fn solana_addresses_keep_their_case() {
        let s = "9xQeWvG816bUx9EPjHmaT23yvVM2ZWbrrpZb9PusVFin";
        assert_eq!(Chain::Solana.normalise(s), s);
    }

    #[test]
    fn an_expired_pass_is_the_free_tier() {
        let now = DateTime::now();
        let mut a = Account {
            id: ObjectId::new(),
            created_at: now,
            wallets: vec![],
            email: None,
            stripe_customer_id: None,
            entitlement_until: None,
        };
        assert_eq!(a.tier_at(now), Tier::Free);
        a.entitlement_until = Some(DateTime::from_millis(now.timestamp_millis() - 1));
        assert_eq!(a.tier_at(now), Tier::Free);
        a.entitlement_until = Some(DateTime::from_millis(now.timestamp_millis() + 1));
        assert_eq!(a.tier_at(now), Tier::Pro);
    }

    #[test]
    fn enums_are_stored_as_readable_strings() {
        let d = bson::to_document(&Payment {
            id: ObjectId::new(),
            account_id: ObjectId::new(),
            method: PaymentMethod::BaseUsdc,
            external_id: "0xabc:3".into(),
            plan: Plan::Pro365d,
            amount_minor: 36_000_000,
            currency: "USDC".into(),
            payer: None,
            created_at: DateTime::now(),
        })
        .unwrap();
        assert_eq!(d.get_str("method").unwrap(), "base_usdc");
        assert_eq!(d.get_str("plan").unwrap(), "pro_365d");
    }

    #[test]
    fn an_account_without_optional_fields_round_trips() {
        let a = Account {
            id: ObjectId::new(),
            created_at: DateTime::now(),
            wallets: vec![Wallet {
                chain: Chain::Solana,
                address: "abc".into(),
                linked_at: DateTime::now(),
            }],
            email: None,
            stripe_customer_id: None,
            entitlement_until: None,
        };
        let d = bson::to_document(&a).unwrap();
        assert!(!d.contains_key("email"), "absent, not null, so the sparse unique index ignores it");
        let back: Account = bson::from_document(d).unwrap();
        assert_eq!(back.wallets, a.wallets);
    }
}
