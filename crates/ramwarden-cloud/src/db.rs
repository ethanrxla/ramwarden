//! RamWarden Cloud's storage on MongoDB Atlas.
//!
//! Every rule that money or abuse depends on is enforced by the database, not
//! by a read-then-write in Rust, because the API runs more than one instance:
//!
//! - a wallet belongs to one account (unique multikey index);
//! - a payment extends a pass once, however often its webhook fires (unique
//!   `(method, external_id)`, inserted in the same transaction as the
//!   extension);
//! - a daily quota cannot be overrun by concurrent requests (a conditional
//!   upsert that collides with the unique `(account_id, day)` index instead of
//!   creating a second counter);
//! - a refresh token is spent exactly once (`find_one_and_update` on
//!   `rotated_at: null`).
//!
//! Secrets the clients hold — device codes and refresh tokens — are stored only
//! as SHA-256 hashes, so a database dump does not sign anyone in.

use std::time::Duration;

use futures::TryStreamExt;
use mongodb::bson::{Bson, DateTime, Document, doc, oid::ObjectId};
use mongodb::error::{ErrorKind, WriteFailure};
use mongodb::options::{ClientOptions, IndexOptions, ReturnDocument};
use mongodb::{Client, Collection, Database, IndexModel};
use sha2::{Digest, Sha256};

use crate::config::CloudConfig;
use crate::model::*;

const DUPLICATE_KEY: i32 = 11000;
const DAY_MS: i64 = 86_400_000;

/// How long a daily usage row is kept. Long enough for a year-over-year chart,
/// short enough that nobody's history accumulates forever.
pub const USAGE_RETENTION_DAYS: i64 = 400;

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error(transparent)]
    Mongo(#[from] mongodb::error::Error),
    #[error("today's {limit} cloud analyses are used up")]
    QuotaExceeded { limit: i32 },
    #[error("this account already has its {max} devices; revoke one first")]
    DeviceLimit { max: usize },
}

pub type Result<T> = std::result::Result<T, DbError>;

#[derive(Clone)]
pub struct Db {
    client: Client,
    db: Database,
}

impl Db {
    /// Connect and check the cluster answers. Fails fast on a wrong password
    /// or an IP missing from the Atlas access list, rather than on the first
    /// request.
    pub async fn connect(cfg: &CloudConfig) -> Result<Self> {
        let mut opts = ClientOptions::parse(cfg.mongodb_uri.expose()).await?;
        opts.app_name = Some("ramwarden-cloud".into());
        opts.server_selection_timeout = Some(Duration::from_secs(10));
        let client = Client::with_options(opts)?;
        let db = client.database(&cfg.database);
        db.run_command(doc! { "ping": 1 }).await?;
        tracing::info!(database = %cfg.database, uri = %cfg.mongodb_uri, "connected to MongoDB");
        Ok(Db { client, db })
    }

    pub fn database(&self) -> &Database {
        &self.db
    }

    pub fn accounts(&self) -> Collection<Account> {
        self.db.collection(ACCOUNTS)
    }
    pub fn devices(&self) -> Collection<Device> {
        self.db.collection(DEVICES)
    }
    pub fn device_codes(&self) -> Collection<DeviceCode> {
        self.db.collection(DEVICE_CODES)
    }
    pub fn refresh_tokens(&self) -> Collection<RefreshToken> {
        self.db.collection(REFRESH_TOKENS)
    }
    pub fn payments(&self) -> Collection<Payment> {
        self.db.collection(PAYMENTS)
    }
    pub fn usage(&self) -> Collection<UsageDay> {
        self.db.collection(USAGE_DAILY)
    }

    /// Create every index the rules above rely on. Idempotent; run it on each
    /// deploy (`ramwarden-cloud-db init`) or at server start.
    pub async fn ensure_indexes(&self) -> Result<()> {
        let unique = |name: &str| IndexOptions::builder().unique(true).name(name.to_string());
        let ttl = |name: &str| {
            IndexOptions::builder()
                .expire_after(Duration::ZERO)
                .name(name.to_string())
                .build()
        };
        let index = |keys: Document, opts: IndexOptions| IndexModel::builder().keys(keys).options(opts).build();

        self.accounts()
            .create_indexes([
                // Partial, so accounts with no wallet (email or card only) do
                // not all collide on a missing key.
                index(
                    doc! { "wallets.chain": 1, "wallets.address": 1 },
                    unique("wallet_unique")
                        .partial_filter_expression(doc! { "wallets.address": { "$exists": true } })
                        .build(),
                ),
                index(
                    doc! { "email": 1 },
                    unique("email_unique")
                        .partial_filter_expression(doc! { "email": { "$type": "string" } })
                        .build(),
                ),
                index(
                    doc! { "stripe_customer_id": 1 },
                    unique("stripe_customer_unique")
                        .partial_filter_expression(doc! { "stripe_customer_id": { "$type": "string" } })
                        .build(),
                ),
            ])
            .await?;

        self.devices()
            .create_index(index(
                doc! { "account_id": 1, "revoked_at": 1 },
                IndexOptions::builder().name("by_account".to_string()).build(),
            ))
            .await?;

        self.device_codes()
            .create_indexes([
                index(doc! { "device_code_hash": 1 }, unique("device_code_unique").build()),
                index(doc! { "user_code": 1 }, unique("user_code_unique").build()),
                index(doc! { "expires_at": 1 }, ttl("expire")),
            ])
            .await?;

        self.refresh_tokens()
            .create_indexes([
                index(doc! { "token_hash": 1 }, unique("token_unique").build()),
                index(
                    doc! { "device_id": 1 },
                    IndexOptions::builder().name("by_device".to_string()).build(),
                ),
                index(doc! { "expires_at": 1 }, ttl("expire")),
            ])
            .await?;

        self.payments()
            .create_indexes([
                index(doc! { "method": 1, "external_id": 1 }, unique("payment_once").build()),
                index(
                    doc! { "account_id": 1, "created_at": -1 },
                    IndexOptions::builder().name("by_account".to_string()).build(),
                ),
            ])
            .await?;

        self.usage()
            .create_indexes([
                index(doc! { "account_id": 1, "day": 1 }, unique("account_day").build()),
                index(doc! { "expire_at": 1 }, ttl("expire")),
            ])
            .await?;

        Ok(())
    }

    // ---- accounts and wallets ------------------------------------------------

    /// The account a signed-in wallet belongs to, creating one on first sign-in.
    pub async fn account_for_wallet(&self, chain: Chain, address: &str) -> Result<Account> {
        let address = chain.normalise(address);
        let filter = doc! {
            "wallets": { "$elemMatch": { "chain": bson(&chain), "address": &address } }
        };
        let now = DateTime::now();
        let insert = doc! { "$setOnInsert": {
            "created_at": now,
            "wallets": [ { "chain": bson(&chain), "address": &address, "linked_at": now } ],
            "entitlement_until": Bson::Null,
        }};
        // Two first sign-ins racing: the loser's upsert hits the unique wallet
        // index, and its retry finds the winner's account.
        for _ in 0..2 {
            match self
                .accounts()
                .find_one_and_update(filter.clone(), insert.clone())
                .upsert(true)
                .return_document(ReturnDocument::After)
                .await
            {
                Ok(Some(a)) => return Ok(a),
                Ok(None) => unreachable!("upsert with ReturnDocument::After returns the document"),
                Err(e) if is_duplicate_key(&e) => continue,
                Err(e) => return Err(e.into()),
            }
        }
        Ok(self
            .accounts()
            .find_one(filter)
            .await?
            .expect("the wallet's account exists after a duplicate-key collision"))
    }

    pub async fn account(&self, id: ObjectId) -> Result<Option<Account>> {
        Ok(self.accounts().find_one(doc! { "_id": id }).await?)
    }

    /// Link another wallet to an account the person is already signed in to.
    /// Returns false when that wallet already belongs to a different account.
    pub async fn link_wallet(&self, account_id: ObjectId, chain: Chain, address: &str) -> Result<bool> {
        let address = chain.normalise(address);
        let wallet = doc! { "chain": bson(&chain), "address": &address, "linked_at": DateTime::now() };
        let r = self
            .accounts()
            .update_one(
                doc! {
                    "_id": account_id,
                    "wallets": { "$not": { "$elemMatch": { "chain": bson(&chain), "address": &address } } },
                },
                doc! { "$push": { "wallets": wallet } },
            )
            .await;
        match r {
            Ok(_) => Ok(true),
            Err(e) if is_duplicate_key(&e) => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    // ---- passes and payments -------------------------------------------------

    /// Add `days` to a pass: from now if it has lapsed, from its end if not, so
    /// renewing early never loses days.
    pub async fn extend_entitlement(&self, account_id: ObjectId, days: i64) -> Result<Option<DateTime>> {
        extend(&self.accounts(), account_id, days, None).await
    }

    /// Set the pass end from an outside source of truth, such as
    /// `paidUntil[account]` read back from the Base contract. Never shortens a
    /// pass, so a card subscription and an on-chain pass cannot cancel out.
    pub async fn set_entitlement_until(&self, account_id: ObjectId, until: DateTime) -> Result<Option<DateTime>> {
        let a = self
            .accounts()
            .find_one_and_update(
                doc! { "_id": account_id },
                vec![doc! { "$set": { "entitlement_until": {
                    "$max": [ { "$ifNull": [ "$entitlement_until", until ] }, until ]
                }}}],
            )
            .return_document(ReturnDocument::After)
            .await?;
        Ok(a.and_then(|a| a.entitlement_until))
    }

    /// Record a confirmed payment and extend the pass, once.
    pub async fn record_payment(&self, p: &Payment) -> Result<PaymentOutcome> {
        let mut s = self.client.start_session().await?;
        s.start_transaction().await?;
        if let Err(e) = self.payments().insert_one(p).session(&mut s).await {
            let _ = s.abort_transaction().await;
            return if is_duplicate_key(&e) {
                Ok(PaymentOutcome::AlreadyRecorded)
            } else {
                Err(e.into())
            };
        }
        let until = match extend(&self.accounts(), p.account_id, p.plan.days(), Some(&mut s)).await {
            Ok(Some(u)) => u,
            Ok(None) => {
                let _ = s.abort_transaction().await;
                return Ok(PaymentOutcome::UnknownAccount);
            }
            Err(e) => {
                let _ = s.abort_transaction().await;
                return Err(e);
            }
        };
        s.commit_transaction().await?;
        Ok(PaymentOutcome::Recorded { until })
    }

    // ---- usage and quotas ----------------------------------------------------

    /// Spend one of today's cloud analyses, or refuse if none are left.
    pub async fn take_cloud_analysis(&self, account_id: ObjectId, tier: Tier, input_tokens: i64) -> Result<Quota> {
        let limit = tier.daily_cloud_analyses();
        let now = DateTime::now();
        let day = utc_day(now);
        // When today's row is already at the limit the filter matches nothing,
        // the upsert tries to insert a second row for the same day, and the
        // unique index refuses it. That refusal is the quota.
        //
        // The same refusal also happens when two requests both create the
        // day's first row, so a collision is only final once the row is full.
        for _ in 0..5 {
            let r = self
                .usage()
                .find_one_and_update(
                    doc! { "account_id": account_id, "day": &day, "cloud_analyses": { "$lt": limit } },
                    doc! {
                        "$inc": { "cloud_analyses": 1, "input_tokens": input_tokens },
                        "$setOnInsert": { "expire_at": plus_days(now, USAGE_RETENTION_DAYS) },
                    },
                )
                .upsert(true)
                .return_document(ReturnDocument::After)
                .await;
            match r {
                Ok(Some(u)) => return Ok(Quota { used: u.cloud_analyses, limit }),
                Ok(None) => unreachable!("upsert with ReturnDocument::After returns the document"),
                Err(e) if is_duplicate_key(&e) => {
                    let used = self
                        .usage()
                        .find_one(doc! { "account_id": account_id, "day": &day })
                        .await?
                        .map_or(0, |u| u.cloud_analyses);
                    if used >= limit {
                        return Err(DbError::QuotaExceeded { limit });
                    }
                }
                Err(e) => return Err(e.into()),
            }
        }
        Err(DbError::QuotaExceeded { limit })
    }

    pub async fn usage_today(&self, account_id: ObjectId) -> Result<i32> {
        let day = utc_day(DateTime::now());
        Ok(self
            .usage()
            .find_one(doc! { "account_id": account_id, "day": day })
            .await?
            .map_or(0, |u| u.cloud_analyses))
    }

    // ---- device sign-in (RFC 8628) ------------------------------------------

    /// Begin a sign-in. Returns the device code for the daemon to poll with
    /// (shown once, stored hashed) and the stored record, whose `user_code`
    /// goes in the link page URL and whose `nonce` the wallet signs.
    pub async fn start_device_code(&self, device_name: &str, platform: Platform, ttl: Duration) -> Result<(String, DeviceCode)> {
        let now = DateTime::now();
        loop {
            let raw = random_token();
            let dc = DeviceCode {
                id: ObjectId::new(),
                device_code_hash: hash(&raw),
                user_code: user_code(),
                nonce: random_token(),
                device_name: device_name.chars().take(64).collect(),
                platform,
                status: DeviceCodeStatus::Pending,
                account_id: None,
                created_at: now,
                expires_at: DateTime::from_millis(now.timestamp_millis() + ttl.as_millis() as i64),
            };
            match self.device_codes().insert_one(&dc).await {
                Ok(_) => return Ok((raw, dc)),
                // A user code collision among the live ones: draw another.
                Err(e) if is_duplicate_key(&e) => continue,
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// The pending sign-in a link page is showing, if it has not expired.
    /// The TTL monitor runs about once a minute, so expiry is checked here too.
    pub async fn pending_device_code(&self, user_code: &str) -> Result<Option<DeviceCode>> {
        Ok(self
            .device_codes()
            .find_one(doc! {
                "user_code": user_code.trim().to_ascii_uppercase(),
                "status": bson(&DeviceCodeStatus::Pending),
                "expires_at": { "$gt": DateTime::now() },
            })
            .await?)
    }

    /// The wallet signature checked out: tie the sign-in to its account.
    pub async fn approve_device_code(&self, user_code: &str, account_id: ObjectId) -> Result<bool> {
        let r = self
            .device_codes()
            .update_one(
                doc! {
                    "user_code": user_code.trim().to_ascii_uppercase(),
                    "status": bson(&DeviceCodeStatus::Pending),
                    "expires_at": { "$gt": DateTime::now() },
                },
                doc! { "$set": { "status": bson(&DeviceCodeStatus::Approved), "account_id": account_id } },
            )
            .await?;
        Ok(r.modified_count == 1)
    }

    /// The daemon's poll. An approved code is claimed exactly once.
    pub async fn claim_device_code(&self, raw_device_code: &str) -> Result<Claim> {
        let h = hash(raw_device_code);
        let now = DateTime::now();
        let claimed = self
            .device_codes()
            .find_one_and_update(
                doc! {
                    "device_code_hash": &h,
                    "status": bson(&DeviceCodeStatus::Approved),
                    "expires_at": { "$gt": now },
                },
                doc! { "$set": { "status": bson(&DeviceCodeStatus::Claimed) } },
            )
            .await?;
        if let Some(dc) = claimed {
            return Ok(Claim::Approved(dc));
        }
        let pending = self
            .device_codes()
            .find_one(doc! {
                "device_code_hash": &h,
                "status": bson(&DeviceCodeStatus::Pending),
                "expires_at": { "$gt": now },
            })
            .await?;
        Ok(if pending.is_some() { Claim::Pending } else { Claim::Expired })
    }

    // ---- devices ---------------------------------------------------------------

    pub async fn register_device(&self, account_id: ObjectId, name: &str, platform: Platform, tier: Tier) -> Result<Device> {
        let max = tier.max_devices();
        let active = self
            .devices()
            .count_documents(doc! { "account_id": account_id, "revoked_at": Bson::Null })
            .await?;
        if active as usize >= max {
            return Err(DbError::DeviceLimit { max });
        }
        let now = DateTime::now();
        let d = Device {
            id: ObjectId::new(),
            account_id,
            name: name.chars().take(64).collect(),
            platform,
            created_at: now,
            last_seen_at: now,
            revoked_at: None,
        };
        self.devices().insert_one(&d).await?;
        Ok(d)
    }

    pub async fn active_devices(&self, account_id: ObjectId) -> Result<Vec<Device>> {
        Ok(self
            .devices()
            .find(doc! { "account_id": account_id, "revoked_at": Bson::Null })
            .sort(doc! { "last_seen_at": -1 })
            .await?
            .try_collect()
            .await?)
    }

    /// Revoke a lost machine: the device and every refresh token it holds.
    /// Its current access token lapses within its 15 minutes.
    pub async fn revoke_device(&self, account_id: ObjectId, device_id: ObjectId) -> Result<bool> {
        let now = DateTime::now();
        let r = self
            .devices()
            .update_one(
                doc! { "_id": device_id, "account_id": account_id, "revoked_at": Bson::Null },
                doc! { "$set": { "revoked_at": now } },
            )
            .await?;
        self.refresh_tokens()
            .update_many(
                doc! { "device_id": device_id, "revoked_at": Bson::Null },
                doc! { "$set": { "revoked_at": now } },
            )
            .await?;
        Ok(r.modified_count == 1)
    }

    // ---- refresh tokens --------------------------------------------------------

    /// Issue a refresh token for a device. The raw token is returned once and
    /// only its hash is stored.
    pub async fn issue_refresh_token(&self, account_id: ObjectId, device_id: ObjectId, ttl: Duration) -> Result<String> {
        let raw = random_token();
        let now = DateTime::now();
        self.refresh_tokens()
            .insert_one(RefreshToken {
                id: ObjectId::new(),
                token_hash: hash(&raw),
                account_id,
                device_id,
                created_at: now,
                expires_at: DateTime::from_millis(now.timestamp_millis() + ttl.as_millis() as i64),
                rotated_at: None,
                revoked_at: None,
            })
            .await?;
        Ok(raw)
    }

    /// Spend a refresh token and issue its successor. A token presented a
    /// second time means two parties hold it, so the whole device is revoked.
    pub async fn rotate_refresh_token(&self, raw: &str, ttl: Duration) -> Result<Rotation> {
        let h = hash(raw);
        let now = DateTime::now();
        let spent = self
            .refresh_tokens()
            .find_one_and_update(
                doc! {
                    "token_hash": &h,
                    "rotated_at": Bson::Null,
                    "revoked_at": Bson::Null,
                    "expires_at": { "$gt": now },
                },
                doc! { "$set": { "rotated_at": now } },
            )
            .await?;
        if let Some(t) = spent {
            let device_ok = self
                .devices()
                .count_documents(doc! { "_id": t.device_id, "revoked_at": Bson::Null })
                .await?
                == 1;
            if !device_ok {
                return Ok(Rotation::Invalid);
            }
            self.devices()
                .update_one(doc! { "_id": t.device_id }, doc! { "$set": { "last_seen_at": now } })
                .await?;
            let next = self.issue_refresh_token(t.account_id, t.device_id, ttl).await?;
            return Ok(Rotation::Rotated { account_id: t.account_id, device_id: t.device_id, refresh_token: next });
        }
        if let Some(t) = self
            .refresh_tokens()
            .find_one(doc! { "token_hash": &h, "rotated_at": { "$ne": Bson::Null } })
            .await?
        {
            tracing::warn!(device = %t.device_id, "refresh token reused; revoking the device");
            self.revoke_device(t.account_id, t.device_id).await?;
            return Ok(Rotation::Reused);
        }
        Ok(Rotation::Invalid)
    }
}

#[derive(Debug, PartialEq)]
pub enum PaymentOutcome {
    Recorded { until: DateTime },
    /// Seen before: a replayed webhook or block. Nothing changed.
    AlreadyRecorded,
    /// No such account; the payment was not recorded and needs a human.
    UnknownAccount,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quota {
    pub used: i32,
    pub limit: i32,
}

#[derive(Debug)]
pub enum Claim {
    Approved(DeviceCode),
    /// RFC 8628 `authorization_pending`: keep polling.
    Pending,
    /// RFC 8628 `expired_token`, also used for unknown and claimed codes.
    Expired,
}

#[derive(Debug)]
pub enum Rotation {
    Rotated { account_id: ObjectId, device_id: ObjectId, refresh_token: String },
    Reused,
    Invalid,
}

async fn extend(
    accounts: &Collection<Account>,
    account_id: ObjectId,
    days: i64,
    session: Option<&mut mongodb::ClientSession>,
) -> Result<Option<DateTime>> {
    let update = vec![doc! { "$set": { "entitlement_until": { "$add": [
        { "$max": [ "$$NOW", { "$ifNull": [ "$entitlement_until", "$$NOW" ] } ] },
        days * DAY_MS,
    ]}}}];
    let op = accounts
        .find_one_and_update(doc! { "_id": account_id }, update)
        .return_document(ReturnDocument::After);
    let a = match session {
        Some(s) => op.session(s).await?,
        None => op.await?,
    };
    Ok(a.and_then(|a| a.entitlement_until))
}

fn bson<T: serde::Serialize>(v: &T) -> Bson {
    mongodb::bson::to_bson(v).expect("model enums serialise")
}

pub fn is_duplicate_key(e: &mongodb::error::Error) -> bool {
    match e.kind.as_ref() {
        ErrorKind::Write(WriteFailure::WriteError(w)) => w.code == DUPLICATE_KEY,
        ErrorKind::Command(c) => c.code == DUPLICATE_KEY,
        ErrorKind::InsertMany(m) => m
            .write_errors
            .as_ref()
            .is_some_and(|es| es.iter().any(|w| w.code == DUPLICATE_KEY)),
        _ => false,
    }
}

/// SHA-256, hex. Tokens are 256 random bits, so no salt is needed.
pub fn hash(raw: &str) -> String {
    hex(&Sha256::digest(raw.as_bytes()))
}

/// 256 random bits, hex encoded.
pub fn random_token() -> String {
    hex(&rand::random::<[u8; 32]>())
}

/// `XXXX-XXXX` from consonants only, as RFC 8628 suggests: no vowels means no
/// accidental words, and no 0/O or 1/I confusion when typed by hand.
pub fn user_code() -> String {
    const ALPHABET: &[u8] = b"BCDFGHJKLMNPQRSTVWXZ";
    let mut s = String::with_capacity(9);
    for i in 0..8 {
        if i == 4 {
            s.push('-');
        }
        s.push(ALPHABET[rand::random_range(0..ALPHABET.len())] as char);
    }
    s
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn plus_days(t: DateTime, days: i64) -> DateTime {
    DateTime::from_millis(t.timestamp_millis() + days * DAY_MS)
}

/// `YYYY-MM-DD` in UTC, the key a daily quota resets on.
pub fn utc_day(t: DateTime) -> String {
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let z = t.timestamp_millis().div_euclid(DAY_MS) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_day_matches_known_dates() {
        assert_eq!(utc_day(DateTime::from_millis(0)), "1970-01-01");
        // 2026-10-30T21:22:41Z
        assert_eq!(utc_day(DateTime::from_millis(1_793_395_361_000)), "2026-10-30");
        // 2024-02-29T23:59:59.999Z, a leap day's last millisecond.
        assert_eq!(utc_day(DateTime::from_millis(1_709_251_199_999)), "2024-02-29");
        assert_eq!(utc_day(DateTime::from_millis(1_709_251_200_000)), "2024-03-01");
    }

    #[test]
    fn user_codes_are_typeable() {
        for _ in 0..200 {
            let c = user_code();
            assert_eq!(c.len(), 9);
            assert_eq!(&c[4..5], "-");
            assert!(c.chars().all(|ch| ch == '-' || "BCDFGHJKLMNPQRSTVWXZ".contains(ch)), "{c}");
        }
    }

    #[test]
    fn tokens_are_random_and_stored_only_as_hashes() {
        let a = random_token();
        assert_eq!(a.len(), 64);
        assert_ne!(a, random_token());
        assert_eq!(hash(&a), hash(&a));
        assert_ne!(hash(&a), a);
        assert_eq!(hash("abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}
