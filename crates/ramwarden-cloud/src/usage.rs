//! Device-authenticated, consent-gated daily diagnostics, separate from billing.
use crate::{MemoryStore, MongoStore, store::Result};
use async_trait::async_trait;
use futures::TryStreamExt;
use mongodb::{
    IndexModel,
    bson::{self, DateTime, doc},
    options::IndexOptions,
};
pub use ramwarden_core::usage::UsageDay;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Mutex, time::Duration};
const COLLECTION: &str = "device_usage_daily";
pub const RETENTION_SECONDS: u64 = 90 * 86400;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UsageRecord {
    pub account_id: String,
    pub device_id: String,
    pub platform: crate::model::Platform,
    pub data: UsageDay,
    pub received_at: u64,
}
#[derive(Default)]
pub(crate) struct MemoryUsage(Mutex<HashMap<String, UsageRecord>>,Mutex<HashMap<String, DeviceConsent>>);
#[derive(Clone,Debug,Serialize,Deserialize)]
pub struct DeviceConsent {pub account_id:String,pub device_id:String,pub enabled:bool,pub since:u64}
#[async_trait]
pub trait UsageStore: Send + Sync {
    async fn set_device_consent(&self,choice:DeviceConsent)->Result<()>;
    async fn device_consents(&self,account:&str)->Result<Vec<DeviceConsent>>;
    async fn device_consent(&self,account:&str,device:&str)->Result<Option<DeviceConsent>>;
    async fn disable_device_consents(&self,account:&str)->Result<()>;
    async fn save_usage(&self, record: UsageRecord) -> Result<()>;
    async fn account_usage(&self, account: &str) -> Result<Vec<UsageRecord>>;
}
#[async_trait]
impl UsageStore for MemoryStore {
    async fn set_device_consent(&self,choice:DeviceConsent)->Result<()> {self.usage.1.lock().unwrap().insert(format!("{}:{}",choice.account_id,choice.device_id),choice);Ok(())}
    async fn device_consent(&self,account:&str,device:&str)->Result<Option<DeviceConsent>> {Ok(self.usage.1.lock().unwrap().get(&format!("{account}:{device}")).cloned())}
    async fn device_consents(&self,account:&str)->Result<Vec<DeviceConsent>> {Ok(self.usage.1.lock().unwrap().values().filter(|c|c.account_id==account).cloned().collect())}
    async fn disable_device_consents(&self,account:&str)->Result<()> {for c in self.usage.1.lock().unwrap().values_mut().filter(|c|c.account_id==account){c.enabled=false}Ok(())}

    async fn save_usage(&self, record: UsageRecord) -> Result<()> {
        let key = format!(
            "{}:{}:{}",
            record.device_id, record.data.day, record.data.since
        );
        let mut rows = self.usage.0.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(previous) = rows.get_mut(&key) {
            for (metric, count) in record.data.counters {
                let old = previous.data.counters.entry(metric).or_default();
                *old = (*old).max(count);
            }
            previous.data.bytes_recovered = previous
                .data
                .bytes_recovered
                .max(record.data.bytes_recovered);
            previous.received_at = previous.received_at.max(record.received_at);
        } else {
            rows.insert(key, record);
        }
        Ok(())
    }
    async fn account_usage(&self, account: &str) -> Result<Vec<UsageRecord>> {
        Ok(self
            .usage
            .0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .values()
            .filter(|r| r.account_id == account)
            .cloned()
            .collect())
    }
}
impl MongoStore {
    pub async fn ensure_usage_indexes(&self) -> Result<()> {
        self.database().collection::<bson::Document>("device_usage_consent").create_index(IndexModel::builder().keys(doc!{"account_id":1}).options(IndexOptions::builder().name("consent_by_account".to_string()).build()).build()).await?;
        self.database()
            .collection::<bson::Document>(COLLECTION)
            .create_indexes([
                IndexModel::builder()
                    .keys(doc! {"expire_at":1})
                    .options(
                        IndexOptions::builder()
                            .name("retention".to_string())
                            .expire_after(Duration::ZERO)
                            .build(),
                    )
                    .build(),
                IndexModel::builder()
                    .keys(doc! {"account_id":1,"data.day":-1})
                    .options(
                        IndexOptions::builder()
                            .name("account_day".to_string())
                            .build(),
                    )
                    .build(),
            ])
            .await?;
        Ok(())
    }
}
#[async_trait]
impl UsageStore for MongoStore {
    async fn set_device_consent(&self,choice:DeviceConsent)->Result<()> {
        let query=doc!{"_id":format!("{}:{}",choice.account_id,choice.device_id)};
        let mut record=bson::to_document(&choice).expect("consent serializes");record.insert("_id",format!("{}:{}",choice.account_id,choice.device_id));
        self.database().collection::<bson::Document>("device_usage_consent").replace_one(query,record).upsert(true).await?;Ok(())
    }
    async fn device_consent(&self,account:&str,device:&str)->Result<Option<DeviceConsent>> {Ok(self.database().collection::<DeviceConsent>("device_usage_consent").find_one(doc!{"_id":format!("{account}:{device}")}).await?)}
    async fn device_consents(&self,account:&str)->Result<Vec<DeviceConsent>> {Ok(self.database().collection::<DeviceConsent>("device_usage_consent").find(doc!{"account_id":account}).limit(20).await?.try_collect().await?)}
    async fn disable_device_consents(&self,account:&str)->Result<()> {self.database().collection::<bson::Document>("device_usage_consent").update_many(doc!{"account_id":account},doc!{"$set":{"enabled":false}}).await?;Ok(())}

    async fn save_usage(&self, record: UsageRecord) -> Result<()> {
        let key = format!(
            "{}:{}:{}",
            record.device_id, record.data.day, record.data.since
        );
        let mut maximum = doc! {"data.bytes_recovered":record.data.bytes_recovered as i64,"received_at":record.received_at as i64};
        for (metric, count) in &record.data.counters {
            maximum.insert(format!("data.counters.{}", metric.key()), *count as i64);
        }
        let expires = record
            .data
            .day
            .saturating_add(86400 + RETENTION_SECONDS)
            .saturating_mul(1000)
            .min(i64::MAX as u64) as i64;
        self.database().collection::<bson::Document>(COLLECTION).update_one(doc!{"_id":key},doc!{
            "$setOnInsert":{"account_id":record.account_id,"device_id":record.device_id,"platform":bson::to_bson(&record.platform).expect("platform"),"data.day":record.data.day as i64,"data.since":record.data.since as i64,"expire_at":DateTime::from_millis(expires)},
            "$max":maximum,
        }).upsert(true).await?;
        Ok(())
    }
    async fn account_usage(&self, account: &str) -> Result<Vec<UsageRecord>> {
        Ok(self
            .database()
            .collection::<UsageRecord>(COLLECTION)
            .find(doc! {"account_id":account})
            .sort(doc! {"data.day":-1})
            .limit(450)
            .await?
            .try_collect()
            .await?)
    }
}
