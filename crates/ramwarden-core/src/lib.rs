//! RamWarden policy: what the user is actually using, and what may be reclaimed.
//!
//! [`ramwarden_kernel`] measures and actuates; this crate decides. The split
//! matters because the decisions are the part with real consequences — a wrong
//! reclaim costs a few milliseconds of page faults, a wrong kill costs the
//! user's unsaved work.

pub mod actuator;
pub mod config;
pub mod detector;
pub mod exec;
pub mod history;
pub mod host;
pub mod ladder;
pub mod pattern;
pub mod roles;
pub mod signals;
pub mod tidy;
pub mod workspaces;

// These read `/proc` directly: terminal sessions and private-browsing
// launchers have no platform-neutral shape yet.
#[cfg(target_os = "linux")]
pub mod private;
#[cfg(target_os = "linux")]
pub mod terminals;
