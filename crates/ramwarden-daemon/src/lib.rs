//! The RamWarden daemon, as a library.
//!
//! Split from the binary so the HTTP surface can be exercised by integration
//! tests: the tab registry and the route handlers are the parts most likely to
//! break the shipped browser extensions, and those contracts deserve tests that
//! run the real router rather than poking at internals.

pub mod ai;
pub mod account;
pub mod desktop;
pub mod embedder;
pub mod extras;
pub mod freeze_journal;
pub mod gpu;
pub mod guard;
pub mod hub;
pub mod ladderjudge;
pub mod monitor;
pub mod outcomes;
pub mod predict;
pub mod routes;
pub mod tabpolicy;
pub mod appintel;
pub mod appquit;
pub mod tab_analysis;
pub mod tabintel;
pub mod tabrank;
pub mod tabs;
pub mod voice;
pub mod wardens;
pub mod worker;
pub mod ws;

pub mod browser;

pub mod assistant;

pub mod companion;

pub mod assistant_prompt;

pub mod tabreview;

pub mod decisionlog;
pub mod recall;
