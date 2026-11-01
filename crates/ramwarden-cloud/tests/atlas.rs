//! The store contract, checked against a real MongoDB.
//!
//! Skipped unless `RAMWARDEN_TEST_MONGODB_URI` is set, because CI has no
//! cluster. Point it at Atlas (or any MongoDB) and run
//! `cargo test -p ramwarden-cloud --test atlas`. Each run uses a fresh
//! database named `ramwarden-test-<random>` and drops its collections at the
//! end (collections, not the database: an Atlas user with "read and write to
//! any database" may not run `dropDatabase`).

use ramwarden_cloud::secret::Secret;
use ramwarden_cloud::store::{contract, random_token};
use ramwarden_cloud::{CloudConfig, MongoStore};

#[tokio::test]
async fn atlas_keeps_the_store_contract() {
    let Ok(uri) = std::env::var("RAMWARDEN_TEST_MONGODB_URI") else {
        eprintln!("RAMWARDEN_TEST_MONGODB_URI not set; skipping");
        return;
    };
    let cfg = CloudConfig { mongodb_uri: Secret::new(uri), database: format!("ramwarden-test-{}", &random_token("")[..8]) };
    let store = MongoStore::connect(&cfg).await.expect("connect");
    store.ensure_indexes().await.expect("indexes");

    let outcome = futures::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(contract::run(&store))).await;
    for name in store.database().list_collection_names().await.expect("list test collections") {
        store.database().collection::<mongodb::bson::Document>(&name).drop().await.expect("drop test collection");
    }
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}
