//! Reversible Windows privacy and background tweaks.
//!
//! Every change is a [`tweak::Tweak`] — a registry value, a service start type,
//! a scheduled task, or a startup entry — with `detect`, `snapshot`, `apply`
//! and `revert`. The [`engine::Engine`] records the snapshot in a journal
//! *before* changing anything, so every change RamWarden makes can be undone,
//! and a setting that was already the way a tweak wants is never claimed.
//!
//! Profiles are compiled in ([`catalog`]); a list of things no profile may
//! touch — Defender, Windows Update, SysMain, Search, anti-cheat — is checked
//! when they load. Machine-wide tweaks need administrator rights and run in
//! `ramwarden-service` unless the caller is already elevated.
#![cfg(windows)]

pub mod catalog;
pub mod engine;
pub mod journal;
pub mod registry;
pub mod service;
pub mod startup;
pub mod task;
pub mod tweak;

pub use engine::{Applied, Debloat, Engine, Line, Reverted, Status};
pub use tweak::{Scope, State, Tweak};
