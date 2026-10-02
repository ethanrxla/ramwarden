//! Linux kernel memory primitives for RamWarden.
//!
//! This crate is the reason RamWarden moved to Rust. It is deliberately free of
//! policy: it measures what the kernel reports and applies what it is told, and
//! every decision about *whether* to apply something lives in `ramwarden-core`.
//!
//! Two things here replace the guesswork the Python daemon relied on:
//!
//! * **Accounting.** `psutil`-summed RSS double-counts pages shared between a
//!   browser's renderer processes — measured at 2.3x over on a 75-process Brave.
//!   [`smaps`] reads PSS and [`cgroup`] reads the kernel's own per-scope figure,
//!   both of which are correct by construction.
//!
//! * **Levers.** The Python daemon's strongest action was `SIGSTOP`, which makes
//!   a GUI app look crashed. [`cgroup::Scope::reclaim`] asks the kernel to page an
//!   app's cold memory out to zram instead: reversible, and invisible to the app.
//!
//! Everything is addressed through a [`Root`] so tests can substitute a synthetic
//! `/proc` and `/sys` tree.

mod error;
mod root;

pub mod cgroup;
pub mod madvise;
pub mod meminfo;
pub mod net;
pub mod oom;
pub mod process;
pub mod procfd;
pub mod psi;
pub mod smaps;
pub mod zram;

pub use error::{Error, Result};
pub use root::Root;

/// Kernel memory figures are reported in kibibytes; the rest of RamWarden speaks
/// bytes. Converting at the parse boundary keeps the ambiguity in one place.
pub(crate) const KIB: u64 = 1024;
