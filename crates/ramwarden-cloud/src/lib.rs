//! RamWarden Cloud's storage: accounts, wallets, devices, passes, payments and
//! usage, on MongoDB Atlas.
//!
//! This crate is the database layer only. The HTTP API, sign-in pages and
//! payment watchers sit on top of [`db::Db`]; none of them touch MongoDB
//! directly, so every rule money depends on lives here, enforced by an index
//! or an atomic update. See `docs/cloud-database.md` for the schema and the
//! Atlas setup.

pub mod config;
pub mod db;
pub mod model;
pub mod secret;

pub use config::CloudConfig;
pub use db::{Db, DbError};
