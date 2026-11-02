//! RamWarden's hosted service storage: accounts, sign-ins, devices,
//! subscriptions, payments, and the site wardens' support conversations.
//!
//! The HTTP API (`ramwarden-api`) sees only the [`store::Store`] trait.
//! [`mongo::MongoStore`] implements it on MongoDB Atlas, enforcing each money
//! or abuse rule with an index or one atomic update; [`store::MemoryStore`]
//! implements it in memory for tests and development. The schema and the
//! Atlas setup are in `docs/cloud-database.md`; the product rules behind it in
//! `docs/product-spec.md` 4.

pub mod config;
pub mod model;
pub mod mongo;
pub mod secret;
pub mod store;
pub mod support;

pub use config::CloudConfig;
pub use mongo::MongoStore;
pub use store::{MemoryStore, Store, StoreError};
