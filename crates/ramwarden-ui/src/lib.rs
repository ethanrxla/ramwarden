//! RamWarden's window.
//!
//! The GTK parts live behind the `gui` feature so the data layer — the daemon
//! client and the row model — stays testable on a machine without GTK4
//! development headers.

#[cfg(feature = "gui")]
pub mod app;
pub mod client;
pub mod model;
#[cfg(feature = "gui")]
pub mod row_object;

pub mod runtime;
#[cfg(feature = "gui")]
pub mod browser;
