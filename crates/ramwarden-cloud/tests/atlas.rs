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
    // Reproduce earlier deployment schemas without dropping their indexes.
    use mongodb::{IndexModel, bson::{Document, doc}, options::IndexOptions};
    for (collection, keys, name, ttl) in [
        ("devices", doc! {"account_id":1,"revoked_at":1}, "by_account", false),
        ("device_codes", doc! {"expires_at":1}, "expire", true),
        ("sessions", doc! {"expire_at":1}, "expire", true),
    ] {
        let mut options = IndexOptions::builder().name(name.to_string()).build();
        if ttl { options.expire_after = Some(std::time::Duration::ZERO); }
        store.database().collection::<Document>(collection)
            .create_index(IndexModel::builder().keys(keys).options(options).build()).await.expect("legacy index");
    }
    store.database().collection::<Document>("device_codes")
        .create_index(IndexModel::builder().keys(doc! {"user_code":1})
            .options(IndexOptions::builder().name("user_code_unique".to_string()).unique(true).build()).build())
        .await.expect("legacy user-code uniqueness");
    store.ensure_indexes().await.expect("indexes");
    store.ensure_indexes().await.expect("idempotent indexes");

    let outcome = futures::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(async { contract::run(&store).await; decisions_sync_is_idempotent_and_takes_late_outcomes(&store).await; })).await;
    for name in store.database().list_collection_names().await.expect("list test collections") {
        store.database().collection::<mongodb::bson::Document>(&name).drop().await.expect("drop test collection");
    }
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

async fn decisions_sync_is_idempotent_and_takes_late_outcomes(db: &MongoStore) {
    use ramwarden_cloud::decision_sync::{Syncer, Cursor};
    use ramwarden_core::decisions::NewDecision;
    use ramwarden_core::history::History;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.db");
    let h = History::open(&path).unwrap();
    let id = h
        .record_decision(
            &NewDecision {
                kind: "tab".into(),
                action: "close".into(),
                result: "confirmed".into(),
                subject: "docs.example.com".into(),
                title: "Plan".into(),
                url: "https://docs.example.com/d/secret".into(),
                vector: vec![0.5; 16],
                ..Default::default()
            },
            1_700_000_000,
        )
        .unwrap();
    let syncer = Syncer { db, history_path: path.clone(), device: "test".into(), titles: true, embedder: None };
    assert_eq!(syncer.sync().await.unwrap().decisions, 1);
    assert_eq!(syncer.sync().await.unwrap().decisions, 0, "nothing new, nothing sent");

    h.mark_decision_outcome(id, "reopened", 1_700_000_100).unwrap();
    assert_eq!(syncer.sync().await.unwrap().decisions, 1);
    let doc = db.decision(&format!("test:{id}")).await.unwrap().unwrap();
    assert_eq!(doc.outcome.as_deref(), Some("reopened"));
    assert_eq!(db.decisions().count_documents(mongodb::bson::doc! {}).await.unwrap(), 1);

    // Starting over from an empty cursor resends, and still makes one document.
    std::fs::remove_file(Cursor::path(&path)).unwrap();
    syncer.sync().await.unwrap();
    assert_eq!(db.decisions().count_documents(mongodb::bson::doc! {}).await.unwrap(), 1);
}
