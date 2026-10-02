//! RamWarden policy: what the user is actually using, and what may be reclaimed.
//!
//! [`ramwarden_kernel`] measures and actuates; this crate decides. The split
//! matters because the decisions are the part with real consequences — a wrong
//! reclaim costs a few milliseconds of page faults, a wrong kill costs the
//! user's unsaved work.

pub mod actuator;
pub mod config;
pub mod desktop;
pub mod exec;
pub mod helper;
pub mod history;
pub mod ladder;
pub mod terminals;
pub mod workspaces;
pub mod detector;
pub mod pattern;
pub mod private;
pub mod roles;
pub mod signals;
