//! The RamWarden daemon, as a library.
//!
//! Split from the binary so the HTTP surface can be exercised by integration
//! tests: the tab registry and the route handlers are the parts most likely to
//! break the shipped browser extensions, and those contracts deserve tests that
//! run the real router rather than poking at internals.

pub mod ai;
pub mod hub;
pub mod monitor;
pub mod routes;
pub mod tabpolicy;
pub mod tabs;
pub mod ws;

pub mod browser;
