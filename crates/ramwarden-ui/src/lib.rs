//! RamWarden's window.
//!
//! The GTK parts live behind the `gui` feature so the data layer — the daemon
//! client and the row model — stays testable on a machine without GTK4
//! development headers.

#[cfg(feature = "gui")]
pub mod app;
pub mod client;
pub mod companions;
pub mod model;
#[cfg(feature = "gui")]
pub mod row_object;
#[cfg(feature = "gui")]
pub mod threat;
pub mod wardens;

#[cfg(feature = "gui")]
pub mod browser;
#[cfg(feature = "gui")]
pub mod bubble;
pub mod runtime;
#[cfg(feature = "tray")]
pub mod tray;
pub mod voice;

#[cfg(feature = "tray")]
mod workspace_events;
