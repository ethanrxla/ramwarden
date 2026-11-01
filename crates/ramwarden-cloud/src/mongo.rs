//! [`Store`] on MongoDB Atlas.
//!
//! Each rule that money or abuse depends on is one atomic operation, because
//! the API may run more than one instance:
//!
//! - one identity is one account: a unique index on `identity`, and an upsert
//!   that retries once when two first sign-ins race;
//! - billing dates only move forward: `$max` on `paid_until`;
//! - a device code, a challenge and an email code are each used once:
//!   `find_one_and_delete` on the state that may be used;
//! - a payment confirms once: an update filtered on `status: pending`.
//!
//! Short-lived documents carry `expire_at` (a BSON date) under a TTL index, so
//! abandoned sign-ins clean themselves up. Expiry is still checked on every
//! read; the TTL monitor only runs once a minute.

use std::time::Duration;

use async_trait::async_trait;
use mongodb::bson::{self, Bson, DateTime, Document, doc};
use mongodb::error::{ErrorKind, WriteFailure};
use mongodb::options::{ClientOptions, IndexOptions, ReturnDocument};
use mongodb::{Client, Collection, Database, IndexModel};

use crate::config::CloudConfig;
use crate::model::*;
use crate::store::*;

const DUPLICATE_KEY: i32 = 11000;

#[derive(Clone)]
pub struct MongoStore {
    db: Database,
}

fn date(unix: u64) -> DateTime {
    DateTime::from_millis((unix as i64).saturating_mul(1000))
}

fn to_bson<T: serde::Serialize>(v: &T) -> Bson {
    bson::to_bson(v).expect("model types serialize to BSON")
}

fn is_duplicate_key(e: &mongodb::error::Error) -> bool {
    matches!(&*e.kind, ErrorKind::Write(WriteFailure::WriteError(w)) if w.code == DUPLICATE_KEY)
        || matches!(&*e.kind, ErrorKind::Command(c) if c.code == DUPLICATE_KEY)
}

impl MongoStore {
    /// Connect and check the cluster answers, so a wrong password or a
    /// missing Atlas access-list entry fails at start, not on a request.
    pub async fn connect(cfg: &CloudConfig) -> Result<Self> {
        let mut opts = ClientOptions::parse(cfg.mongodb_uri.expose()).await?;
        opts.app_name = Some("ramwarden-api".into());
        opts.server_selection_timeout = Some(Duration::from_secs(10));
        let client = Client::with_options(opts)?;
        let db = client.database(&cfg.database);
        db.run_command(doc! { "ping": 1 }).await?;
        tracing::info!(database = %cfg.database, uri = %cfg.mongodb_uri, "connected to MongoDB");
        Ok(MongoStore { db })
    }

    pub fn database(&self) -> &Database {
        &self.db
    }

    fn accounts(&self) -> Collection<Account> {
        self.db.collection(ACCOUNTS)
    }
    fn devices(&self) -> Collection<Device> {
        self.db.collection(DEVICES)
    }
    fn payments(&self) -> Collection<Payment> {
        self.db.collection(PAYMENTS)
    }
    fn raw(&self, name: &str) -> Collection<Document> {
        self.db.collection(name)
    }

    /// Every index the rules rely on. Idempotent: run at start, or with
    /// `ramwarden-cloud-db init`.
    pub async fn ensure_indexes(&self) -> Result<()> {
        let named = |name: &str| IndexOptions::builder().name(name.to_string()).build();
        let unique = |name: &str| IndexOptions::builder().unique(true).name(name.to_string()).build();
        let ttl = || IndexOptions::builder().expire_after(Duration::ZERO).name("expire".to_string()).build();
        let index = |keys: Document, opts: IndexOptions| IndexModel::builder().keys(keys).options(opts).build();

        self.raw(ACCOUNTS)
            .create_indexes([
                index(doc! { "identity": 1 }, unique("identity_unique")),
                index(doc! { "chain_key": 1 }, unique("chain_key_unique")),
                index(doc! { "tier": 1 }, named("by_tier")),
            ])
            .await?;
        self.raw(DEVICES)
            .create_indexes([
                index(doc! { "token_hash": 1 }, unique("token_unique")),
                index(doc! { "account_id": 1, "created_at": 1 }, named("by_account")),
            ])
            .await?;
        self.raw(DEVICE_CODES)
            .create_indexes([
                index(doc! { "device_code_hash": 1 }, unique("device_code_unique")),
                index(doc! { "expire_at": 1 }, ttl()),
            ])
            .await?;
        for name in [SESSIONS, CHALLENGES, EMAIL_CODES, USAGE_DAILY] {
            self.raw(name).create_index(index(doc! { "expire_at": 1 }, ttl())).await?;
        }
        self.raw(SESSIONS).create_index(index(doc! { "account_id": 1 }, named("by_account"))).await?;
        self.raw(PAYMENTS)
            .create_index(index(doc! { "account_id": 1, "created_at": -1 }, named("by_account")))
            .await?;
        Ok(())
    }
}

#[async_trait]
impl Store for MongoStore {
    async fn start_device_code(&self, name: &str, platform: Platform, now: u64, ttl: u64) -> Result<(String, DeviceCode)> {
        for _ in 0..5 {
            let raw = random_token("rwdc_");
            let dc = DeviceCode {
                user_code: user_code(),
                device_code_hash: hash(&raw),
                device_name: name.to_string(),
                platform,
                status: DeviceCodeStatus::Pending,
                expires_at: now + ttl,
            };
            let mut d = bson::to_document(&dc).expect("serializes");
            d.insert("expire_at", date(dc.expires_at));
            match self.raw(DEVICE_CODES).insert_one(d).await {
                Ok(_) => return Ok((raw, dc)),
                // A user code collision (20^8 space): draw another.
                Err(e) if is_duplicate_key(&e) => continue,
                Err(e) => return Err(e.into()),
            }
        }
        Err(StoreError::Unavailable("could not allocate a user code".into()))
    }

    async fn pending_device_code(&self, user_code: &str, now: u64) -> Result<Option<DeviceCode>> {
        let d = self
            .raw(DEVICE_CODES)
            .find_one(doc! { "_id": user_code, "status.state": "pending", "expires_at": { "$gt": now as i64 } })
            .await?;
        Ok(d.and_then(|d| bson::from_document(d).ok()))
    }

    async fn approve_device_code(&self, user_code: &str, account_id: &str, now: u64) -> Result<bool> {
        let r = self
            .raw(DEVICE_CODES)
            .update_one(
                doc! { "_id": user_code, "status.state": "pending", "expires_at": { "$gt": now as i64 } },
                doc! { "$set": { "status": { "state": "approved", "account_id": account_id } } },
            )
            .await?;
        Ok(r.modified_count == 1)
    }

    async fn claim_device_code(&self, raw_device_code: &str, now: u64) -> Result<Claim> {
        let h = hash(raw_device_code);
        let taken = self
            .raw(DEVICE_CODES)
            .find_one_and_delete(doc! { "device_code_hash": &h, "status.state": "approved", "expires_at": { "$gt": now as i64 } })
            .await?;
        if let Some(d) = taken {
            let dc: DeviceCode = bson::from_document(d).map_err(|e| StoreError::Unavailable(e.to_string()))?;
            if let DeviceCodeStatus::Approved { account_id } = dc.status {
                return Ok(Claim::Approved { account_id, device_name: dc.device_name, platform: dc.platform });
            }
        }
        let pending = self
            .raw(DEVICE_CODES)
            .find_one(doc! { "device_code_hash": &h, "status.state": "pending", "expires_at": { "$gt": now as i64 } })
            .await?;
        Ok(if pending.is_some() { Claim::Pending } else { Claim::Gone })
    }

    async fn put_challenge(&self, nonce: &str, c: Challenge, _now: u64) -> Result<()> {
        let mut d = bson::to_document(&c).expect("serializes");
        d.insert("_id", nonce);
        d.insert("expire_at", date(c.expires_at));
        self.raw(CHALLENGES).insert_one(d).await?;
        Ok(())
    }

    async fn take_challenge(&self, nonce: &str, now: u64) -> Result<Option<Challenge>> {
        let d = self.raw(CHALLENGES).find_one_and_delete(doc! { "_id": nonce }).await?;
        Ok(d.and_then(|d| bson::from_document::<Challenge>(d).ok()).filter(|c| c.expires_at > now))
    }

    async fn start_email_code(&self, email: &str, code: &str, user_code: Option<&str>, now: u64, ttl: u64) -> Result<bool> {
        let c = EmailCode {
            code_hash: hash(code),
            user_code: user_code.map(str::to_string),
            created_at: now,
            expires_at: now + ttl,
            attempts: 0,
        };
        let mut d = bson::to_document(&c).expect("serializes");
        d.insert("expire_at", date(c.expires_at));
        // Replace only a code old enough to resend (or none): the filter
        // matches nothing for a fresh one, and the upsert then collides on
        // `_id`, which is the refusal.
        let r = self
            .raw(EMAIL_CODES)
            .replace_one(
                doc! { "_id": email, "created_at": { "$lte": now.saturating_sub(EMAIL_RESEND_SECONDS) as i64 } },
                d,
            )
            .upsert(true)
            .await;
        match r {
            Ok(_) => Ok(true),
            Err(e) if is_duplicate_key(&e) => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    async fn check_email_code(&self, email: &str, code: &str, now: u64) -> Result<EmailCheck> {
        let live = doc! {
            "_id": email,
            "expires_at": { "$gt": now as i64 },
            "attempts": { "$lt": EMAIL_CODE_ATTEMPTS as i64 },
        };
        let mut right = live.clone();
        right.insert("code_hash", hash(code));
        if let Some(d) = self.raw(EMAIL_CODES).find_one_and_delete(right).await? {
            let c: EmailCode = bson::from_document(d).map_err(|e| StoreError::Unavailable(e.to_string()))?;
            return Ok(EmailCheck::Ok { user_code: c.user_code });
        }
        let r = self.raw(EMAIL_CODES).update_one(live, doc! { "$inc": { "attempts": 1 } }).await?;
        if r.matched_count == 1 {
            Ok(EmailCheck::Wrong)
        } else {
            self.raw(EMAIL_CODES).delete_one(doc! { "_id": email }).await?;
            Ok(EmailCheck::Gone)
        }
    }

    async fn account_for_identity(&self, identity: &Identity, now: u64) -> Result<(Account, bool)> {
        let fresh = Account::new(new_id(), identity.clone(), now);
        let mut insert = bson::to_document(&fresh).expect("serializes");
        insert.remove("identity"); // comes from the upsert's filter
        insert.insert("chain_key", hex(&fresh.chain_key()));
        let filter = doc! { "identity": to_bson(identity) };
        for _ in 0..2 {
            match self
                .accounts()
                .find_one_and_update(filter.clone(), doc! { "$setOnInsert": insert.clone() })
                .upsert(true)
                .return_document(ReturnDocument::After)
                .await
            {
                Ok(Some(a)) => {
                    let created = a.id == fresh.id;
                    return Ok((a, created));
                }
                Ok(None) => return Err(StoreError::Unavailable("upsert returned nothing".into())),
                // Two first sign-ins racing: the loser's retry finds the winner's.
                Err(e) if is_duplicate_key(&e) => continue,
                Err(e) => return Err(e.into()),
            }
        }
        let a = self.accounts().find_one(filter).await?;
        a.map(|a| (a, false)).ok_or_else(|| StoreError::Unavailable("account vanished".into()))
    }

    async fn account(&self, id: &str) -> Result<Option<Account>> {
        Ok(self.accounts().find_one(doc! { "_id": id }).await?)
    }

    async fn account_by_chain_key(&self, key: &[u8; 32]) -> Result<Option<Account>> {
        Ok(self.accounts().find_one(doc! { "chain_key": hex(key) }).await?)
    }

    async fn update_billing(&self, account_id: &str, u: &BillingUpdate) -> Result<Option<Account>> {
        let mut set = Document::new();
        if let Some(t) = u.tier {
            set.insert("tier", to_bson(&t));
        }
        if let Some(i) = u.interval {
            set.insert("interval", to_bson(&i));
        }
        if let Some(t) = u.trial_ends_at {
            set.insert("trial_ends_at", t as i64);
        }
        if let Some(c) = u.cancelled {
            set.insert("cancelled", c);
        }
        if let Some(b) = &u.billing {
            set.insert("billing", to_bson(b));
        }
        if u.trial_used == Some(true) {
            set.insert("trial_used", true);
        }
        let mut update = Document::new();
        if !set.is_empty() {
            update.insert("$set", set);
        }
        if let Some(t) = u.paid_until {
            update.insert("$max", doc! { "paid_until": t as i64 });
        }
        if update.is_empty() {
            return self.account(account_id).await;
        }
        Ok(self
            .accounts()
            .find_one_and_update(doc! { "_id": account_id }, update)
            .return_document(ReturnDocument::After)
            .await?)
    }

    async fn set_prefs(&self, account_id: &str, prefs: serde_json::Map<String, serde_json::Value>) -> Result<Option<Account>> {
        Ok(self
            .accounts()
            .find_one_and_update(doc! { "_id": account_id }, doc! { "$set": { "prefs": to_bson(&prefs) } })
            .return_document(ReturnDocument::After)
            .await?)
    }

    async fn create_session(&self, account_id: &str, now: u64, ttl: u64) -> Result<String> {
        let raw = random_token("rwse_");
        self.raw(SESSIONS)
            .insert_one(doc! {
                "_id": hash(&raw),
                "account_id": account_id,
                "kind": to_bson(&SessionKind::Web),
                "created_at": now as i64,
                "expires_at": (now + ttl) as i64,
                "expire_at": date(now + ttl),
            })
            .await?;
        Ok(raw)
    }

    async fn session_account(&self, raw: &str, now: u64) -> Result<Option<Id>> {
        let d = self
            .raw(SESSIONS)
            .find_one(doc! { "_id": hash(raw), "expires_at": { "$gt": now as i64 } })
            .await?;
        Ok(d.and_then(|d| d.get_str("account_id").ok().map(str::to_string)))
    }

    async fn end_session(&self, raw: &str) -> Result<()> {
        self.raw(SESSIONS).delete_one(doc! { "_id": hash(raw) }).await?;
        Ok(())
    }

    async fn register_device(&self, account_id: &str, name: &str, platform: Platform, now: u64) -> Result<Option<(Device, String)>> {
        let limit = self.account(account_id).await?.map_or(DEVICE_LIMIT, |a| a.device_limit);
        // Two devices linking in the same instant could both pass; the cost is
        // one device over the limit until the user removes one.
        let n = self.devices().count_documents(doc! { "account_id": account_id }).await?;
        if n >= u64::from(limit) {
            return Ok(None);
        }
        let raw = random_token("rwdt_");
        let d = Device {
            id: new_id(),
            account_id: account_id.to_string(),
            name: name.to_string(),
            platform,
            created_at: now,
            last_seen_at: now,
            token_hash: hash(&raw),
        };
        self.devices().insert_one(&d).await?;
        Ok(Some((d, raw)))
    }

    async fn device_for_token(&self, raw: &str, now: u64) -> Result<Option<Device>> {
        Ok(self
            .devices()
            .find_one_and_update(doc! { "token_hash": hash(raw) }, doc! { "$set": { "last_seen_at": now as i64 } })
            .return_document(ReturnDocument::After)
            .await?)
    }

    async fn devices(&self, account_id: &str) -> Result<Vec<Device>> {
        use futures::TryStreamExt;
        Ok(self
            .devices()
            .find(doc! { "account_id": account_id })
            .sort(doc! { "created_at": 1 })
            .await?
            .try_collect()
            .await?)
    }

    async fn revoke_device(&self, account_id: &str, device_id: &str) -> Result<bool> {
        let r = self.devices().delete_one(doc! { "_id": device_id, "account_id": account_id }).await?;
        Ok(r.deleted_count == 1)
    }

    async fn create_payment(&self, p: Payment) -> Result<()> {
        self.payments().insert_one(&p).await?;
        Ok(())
    }

    async fn payment(&self, id: &str) -> Result<Option<Payment>> {
        Ok(self.payments().find_one(doc! { "_id": id }).await?)
    }

    async fn confirm_payment(&self, id: &str, external_id: &str) -> Result<bool> {
        let r = self
            .payments()
            .update_one(
                doc! { "_id": id, "status": "pending" },
                doc! { "$set": { "status": "confirmed", "external_id": external_id } },
            )
            .await?;
        Ok(r.modified_count == 1)
    }

    async fn count_use(&self, account_id: &str, now: u64) -> Result<u32> {
        let day = now / 86_400;
        let d = self
            .raw(USAGE_DAILY)
            .find_one_and_update(
                doc! { "_id": format!("{account_id}:{day}") },
                doc! {
                    "$inc": { "count": 1 },
                    "$setOnInsert": { "account_id": account_id, "day": day as i64, "expire_at": date((day + 400) * 86_400) },
                },
            )
            .upsert(true)
            .return_document(ReturnDocument::After)
            .await?;
        Ok(d.and_then(|d| d.get_i32("count").ok()).unwrap_or(1) as u32)
    }

    async fn used_today(&self, account_id: &str, now: u64) -> Result<u32> {
        let d = self.raw(USAGE_DAILY).find_one(doc! { "_id": format!("{account_id}:{}", now / 86_400) }).await?;
        Ok(d.and_then(|d| d.get_i32("count").ok()).unwrap_or(0) as u32)
    }
}
