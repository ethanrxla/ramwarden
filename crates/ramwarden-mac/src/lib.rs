//! The macOS implementation of `ramwarden-platform`.
//!
//! * **Memory** — `host_statistics64` and `sysctl`; the compressor stands in for zram.
//! * **Pressure** — stall time estimated from swap-in, decompression and swap-out
//!   rates (smoothed at 10/60/300 s), floored by the kernel's pressure level and
//!   lifted early by a least-squares forecast of available memory. Flagged
//!   `estimated`: macOS has no stall accounting. See `pressure.rs`.
//! * **Processes** — `libproc`. "Proportional" is `ri_phys_footprint`, the figure
//!   Activity Monitor shows as *Memory*; it counts compressed pages and does not
//!   double-count shared mappings the way summed RSS does.
//! * **Control** — signals, guarded against PID reuse by start time (no pidfd).
//! * **Pager / groups / background** — macOS offers no per-process page-out, no
//!   cgroup-style reclaim and no safe memory-priority hint, so these report
//!   unavailable and the ladder uses tab unloading and suspend instead.
#![cfg(target_os = "macos")]
#![allow(deprecated)] // libc points at mach2 for the few mach calls used here

mod desktop;
mod forecast;
mod memory;
mod pressure;
mod process;
mod storage;
mod sys;

use std::sync::Arc;

use ramwarden_platform::{Error, GroupSource, NoBackground, NoPager, Parts, Platform, Result};

pub use desktop::MacDesktop;
pub use memory::MacMemory;
pub use pressure::MacPressure;
pub use process::{MacControl, MacHandle, MacProcesses};
pub use storage::MacStorage;

/// Build the macOS platform over the live system.
pub fn platform() -> Platform {
    Platform::new(
        Parts {
            name: "macos",
            memory: Arc::new(MacMemory),
            pressure: Arc::new(MacPressure::new()),
            control: Arc::new(MacControl),
            pager: Arc::new(NoPager),
            background: Arc::new(NoBackground),
            groups: Arc::new(NoGroups),
        },
        || Box::new(MacProcesses::new()),
        || Box::new(MacDesktop::new()),
    )
    .with_hardware(Arc::new(MacHardware))
    .with_storage(Arc::new(MacStorage))
}

/// CPU model and core counts from `sysctl`. On Apple silicon the
/// performance levels give the P- and E-core split (`hw.perflevel0` is the
/// performance cluster).
pub struct MacHardware;

impl ramwarden_platform::HardwareSource for MacHardware {
    fn read(&self) -> Result<ramwarden_platform::Hardware> {
        let count = |name: &str| sys::sysctl_int(name).map(|n| n as u32);
        let levels = count("hw.nperflevels").unwrap_or(0);
        Ok(ramwarden_platform::Hardware {
            cpu_model: sys::sysctl_string("machdep.cpu.brand_string"),
            physical_cores: count("hw.physicalcpu"),
            logical_cores: count("hw.logicalcpu").or_else(ramwarden_platform::hardware::logical_cores),
            performance_cores: (levels > 1).then(|| count("hw.perflevel0.physicalcpu")).flatten(),
            efficiency_cores: (levels > 1).then(|| count("hw.perflevel1.physicalcpu")).flatten(),
        })
    }
}

/// macOS has no reclaim unit like a cgroup scope, so group reclaim is off.
pub struct NoGroups;

impl GroupSource for NoGroups {
    fn ready(&self) -> Result<()> {
        Err(Error::Unsupported("group reclaim"))
    }

    fn groups(&self) -> Result<Vec<Box<dyn ramwarden_platform::AppGroup>>> {
        Err(Error::Unsupported("group reclaim"))
    }
}
