//! The seam between RamWarden's policy and an operating system.
//!
//! `ramwarden-core` decides; a mechanism crate measures and actuates. On Linux
//! that is `ramwarden-kernel` (cgroup v2, PSI, smaps, pidfd); on Windows it is
//! `ramwarden-nt` (NT native APIs). Both implement the traits here, and core
//! never names either directly.
//!
//! The traits are deliberately shaped after what the ladder *does* rather than
//! after either kernel's interface, so the policy reads the same on both:
//!
//! | Trait | Linux | Windows |
//! |---|---|---|
//! | [`ProcessSource`] | `/proc` + `smaps_rollup` | `NtQuerySystemInformation` |
//! | [`GroupSource`] / [`AppGroup`] | cgroup scopes | process trees |
//! | [`PressureSource`] | PSI | a fault/commit pressure estimate |
//! | [`MemorySource`] | `/proc/meminfo`, zram | `GlobalMemoryStatusEx`, compression store |
//! | [`DesktopSource`] | `pactl`, `wmctrl` | Win32 windows, WASAPI sessions |
//! | [`ProcessControl`] | pidfd | process `HANDLE` |
//! | [`Pager`] | `process_madvise` | working-set trim |
//! | [`Background`] | — | memory priority + EcoQoS |
//!
//! Nothing here touches an OS. That is what lets [`fake`] drive the whole ladder
//! in a unit test on either platform.

mod background;
mod desktop;
mod error;
pub mod fake;
mod group;
mod memory;
mod outcome;
mod pager;
mod platform;
mod pressure;
mod process;

pub use background::{Background, NoBackground};
pub use desktop::{Desktop, DesktopSource};
pub use error::{Error, Result};
pub use group::{AppGroup, GroupSource};
pub use memory::{Compression, MemorySource, MemoryStatus};
pub use outcome::Outcome;
pub use pager::{NoPager, Pager};
pub use platform::{Parts, Platform};
pub use pressure::{Pressure, PressureSource, PressureWaker};
pub use process::{Owner, ProcessControl, ProcessHandle, ProcessSample, ProcessSource};
