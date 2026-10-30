//! RamWarden's hosted API: the daemon's Jev questions, answered with the
//! operator's key instead of one the user has to bring.
//!
//! # Speaking Jev's protocol, so the daemon does not change
//!
//! The server exposes the same two endpoints as TypeSafe (`GET /v1/models`,
//! `POST /v1/systemone`). A daemon uses it by setting `[jev] base_url` to this
//! server and `api_key` to its RamWarden token; [`ramwarden_ai::jev::Jev`] works
//! unmodified.
//!
//! # Not a free Jev relay
//!
//! A stolen RamWarden token must not be a way to run arbitrary questions on the
//! operator's bill. So:
//!
//! - every question must match the [`catalog`] exactly, name and instructions —
//!   the only questions answered are the ones RamWarden itself asks;
//! - the model is fixed by the server, whatever the request names;
//! - `state` is size-capped, and each token has a per-minute and a daily quota;
//! - a global daily input-token cap stops all upstream spend when crossed, and the
//!   daemon falls back to its closed-form estimate as it does when Jev is off.
//!
//! # What is never logged
//!
//! `state` describes the user's apps and tabs. It is forwarded and forgotten:
//! logs carry the token's fingerprint, the question names, sizes and timings.

pub mod catalog;
pub mod config;
pub mod limits;
pub mod routes;
pub mod upstream;

pub use config::Config;
pub use routes::{AppState, router};
