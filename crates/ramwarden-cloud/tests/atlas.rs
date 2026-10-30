//! The database rules, checked against a real MongoDB.
//!
//! Skipped unless `RAMWARDEN_TEST_MONGODB_URI` is set, because CI has no
//! cluster. Point it at Atlas (or any replica set; transactions need one) and
//! run `cargo test -p ramwarden-cloud --test atlas`. Each run uses a fresh
//! database named `ramwarden-test-<random>` and drops it at the end.

use std::time::Duration;

use mongodb::bson::{DateTime, oid::ObjectId};
use ramwarden_ai::secret::Secret;
use ramwarden_cloud::db::{Claim, PaymentOutcome, Rotation};
use ramwarden_cloud::model::*;
use ramwarden_cloud::{CloudConfig, Db, DbError};

async fn fresh_db() -> Option<Db> {
    let uri = std::env::var("RAMWARDEN_TEST_MONGODB_URI").ok()?;
    let cfg = CloudConfig {
        mongodb_uri: Secret::new(uri),
        database: format!("ramwarden-test-{}", &ramwarden_cloud::db::random_token()[..8]),
    };
    let db = Db::connect(&cfg).await.expect("connect");
    db.ensure_indexes().await.expect("indexes");
    Some(db)
}

#[tokio::test]
async fn the_database_enforces_the_money_and_abuse_rules() {
    let Some(db) = fresh_db().await else {
        eprintln!("RAMWARDEN_TEST_MONGODB_URI not set; skipping");
        return;
    };
    let result = std::panic::AssertUnwindSafe(run(&db));
    let outcome = futures::FutureExt::catch_unwind(result).await;
    db.database().drop().await.expect("drop test database");
    if let Err(p) = outcome {
        std::panic::resume_unwind(p);
    }
}

async fn run(db: &Db) {
    // One wallet is one account, whatever the case of its address.
    let a = db.account_for_wallet(Chain::Evm, "0xAbC0000000000000000000000000000000000dEf").await.unwrap();
    let b = db.account_for_wallet(Chain::Evm, "0xabc0000000000000000000000000000000000def").await.unwrap();
    assert_eq!(a.id, b.id);
    assert_eq!(a.tier_at(DateTime::now()), Tier::Free);

    // A second account cannot link a wallet that is already taken.
    let other = db.account_for_wallet(Chain::Solana, "9xQeWvG816bUx9EPjHmaT23yvVM2ZWbrrpZb9PusVFin").await.unwrap();
    assert_ne!(other.id, a.id);
    assert!(!db.link_wallet(other.id, Chain::Evm, "0xABC0000000000000000000000000000000000DEF").await.unwrap());
    assert!(db.link_wallet(a.id, Chain::Solana, "So1anaWa11etThatIsFree1111111111111111111111").await.unwrap());

    // A payment extends the pass once, however often it is delivered.
    let p = Payment {
        id: ObjectId::new(),
        account_id: a.id,
        method: PaymentMethod::BaseUsdc,
        external_id: "0xfeed:0".into(),
        plan: Plan::Pro30d,
        amount_minor: 4_000_000,
        currency: "USDC".into(),
        payer: Some("0xabc0000000000000000000000000000000000def".into()),
        created_at: DateTime::now(),
    };
    let PaymentOutcome::Recorded { until } = db.record_payment(&p).await.unwrap() else {
        panic!("first delivery records");
    };
    let replay = Payment { id: ObjectId::new(), ..p.clone() };
    assert_eq!(db.record_payment(&replay).await.unwrap(), PaymentOutcome::AlreadyRecorded);
    let a = db.account(a.id).await.unwrap().unwrap();
    assert_eq!(a.entitlement_until, Some(until));
    assert_eq!(a.tier_at(DateTime::now()), Tier::Pro);
    let days = (until.timestamp_millis() - DateTime::now().timestamp_millis()) / 86_400_000;
    assert!((29..=30).contains(&days), "{days}");

    // Renewing early stacks; an outside source never shortens.
    let later = db.extend_entitlement(a.id, 30).await.unwrap().unwrap();
    assert!(later.timestamp_millis() - until.timestamp_millis() >= 30 * 86_400_000 - 1000);
    let same = db.set_entitlement_until(a.id, DateTime::from_millis(0)).await.unwrap().unwrap();
    assert_eq!(same, later);

    // The free quota stops at five, even under concurrent requests.
    let tries = (0..12).map(|_| db.take_cloud_analysis(other.id, Tier::Free, 4000));
    let results = futures::future::join_all(tries).await;
    let ok = results.iter().filter(|r| r.is_ok()).count();
    assert_eq!(ok, 5, "{results:?}");
    assert!(results.iter().any(|r| matches!(r, Err(DbError::QuotaExceeded { limit: 5 }))));
    assert_eq!(db.usage_today(other.id).await.unwrap(), 5);

    // Device sign-in: pending until approved, claimed exactly once.
    let (raw, dc) = db.start_device_code("ryzen-desktop", Platform::Linux, Duration::from_secs(600)).await.unwrap();
    assert!(matches!(db.claim_device_code(&raw).await.unwrap(), Claim::Pending));
    assert!(db.pending_device_code(&dc.user_code.to_lowercase()).await.unwrap().is_some());
    assert!(db.approve_device_code(&dc.user_code, a.id).await.unwrap());
    let Claim::Approved(claimed) = db.claim_device_code(&raw).await.unwrap() else {
        panic!("approved code claims");
    };
    assert_eq!(claimed.account_id, Some(a.id));
    assert!(matches!(db.claim_device_code(&raw).await.unwrap(), Claim::Expired));

    // Device limit by tier.
    let d1 = db.register_device(other.id, "laptop", Platform::Windows, Tier::Free).await.unwrap();
    assert!(matches!(
        db.register_device(other.id, "desktop", Platform::Linux, Tier::Free).await,
        Err(DbError::DeviceLimit { max: 1 })
    ));

    // Refresh tokens rotate, and reuse revokes the device.
    let t1 = db.issue_refresh_token(other.id, d1.id, Duration::from_secs(3600)).await.unwrap();
    let Rotation::Rotated { refresh_token: t2, .. } = db.rotate_refresh_token(&t1, Duration::from_secs(3600)).await.unwrap() else {
        panic!("first use rotates");
    };
    assert!(matches!(db.rotate_refresh_token(&t1, Duration::from_secs(3600)).await.unwrap(), Rotation::Reused));
    assert!(matches!(db.rotate_refresh_token(&t2, Duration::from_secs(3600)).await.unwrap(), Rotation::Invalid));
    assert!(db.active_devices(other.id).await.unwrap().is_empty());
}
