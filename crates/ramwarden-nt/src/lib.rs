//! Windows NT memory primitives for RamWarden.
//!
//! The Windows counterpart of `ramwarden-kernel`, and held to the same rule: no
//! policy. It measures what the memory manager reports and applies what it is
//! told; every decision about *whether* lives in `ramwarden-core`.
//!
//! Everything a Linux cgroup gives RamWarden has an NT equivalent reachable from
//! user mode, which is why there is no kernel driver here (a test-signed driver
//! would also lock out Vanguard and EasyAntiCheat):
//!
//! | Need | Linux | NT |
//! |---|---|---|
//! | process table | `/proc` | `NtQuerySystemInformation(SystemProcessInformation)` — one call for every process, including its *private* working set |
//! | reclaim | `memory.reclaim` | `EmptyWorkingSet` — pages move to standby/compressed, reversibly |
//! | soft cap | `memory.high` | `SetProcessWorkingSetSizeEx` with a hard maximum |
//! | cold-first eviction | `MADV_COLD` | `ProcessMemoryPriority` = low |
//! | freeze | `SIGSTOP` | `NtSuspendProcess` |
//! | pressure | PSI | an estimate from hard faults, availability and commit — see [`pressure`] |
//! | compression | zram | the Memory Compression store |
//!
//! [`platform`] presents all of it through `ramwarden-platform`'s traits.
#![cfg(windows)]

mod handle;
mod ntdll;

pub mod desktop;
pub mod groups;
pub mod inventory;
pub mod memory;
pub mod net;
pub mod owner;
pub mod platform;
pub mod pressure;
pub mod priority;
pub mod privilege;
pub mod process;
pub mod snapshot;
pub mod standby;
pub mod workingset;

pub use handle::Handle;
