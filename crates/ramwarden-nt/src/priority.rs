//! Hints that cost nothing: memory priority and EcoQoS.
//!
//! Neither removes a single page. Memory priority tells the memory manager
//! which standby pages to repurpose first when it needs memory, so an idle
//! background app's pages go before the foreground app's — the same intent as
//! `MADV_COLD`. EcoQoS schedules the process on efficient cores at low clocks.
//! Both are undone by setting them back, which is why the Tidy rung can apply
//! them at any pressure.

use std::ffi::c_void;

use ramwarden_platform::{Error, Result};
use windows::Win32::System::Threading::{
    MEMORY_PRIORITY, MEMORY_PRIORITY_INFORMATION, MEMORY_PRIORITY_LOW, MEMORY_PRIORITY_NORMAL,
    PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
    PROCESS_POWER_THROTTLING_STATE, ProcessMemoryPriority, ProcessPowerThrottling,
    SetProcessInformation,
};

use crate::process::{PRIORITY_ACCESS, open};

/// Memory priority levels RamWarden uses. Windows has five; these are the two
/// that mean "background" and "as launched".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryPriority {
    /// Repurpose this process's standby pages first.
    Low,
    /// The default for a normal process.
    Normal,
}

impl MemoryPriority {
    fn raw(self) -> MEMORY_PRIORITY {
        match self {
            MemoryPriority::Low => MEMORY_PRIORITY_LOW,
            MemoryPriority::Normal => MEMORY_PRIORITY_NORMAL,
        }
    }
}

pub fn set_memory_priority(pid: u32, level: MemoryPriority) -> Result<()> {
    let h = open(pid, PRIORITY_ACCESS)?;
    let info = MEMORY_PRIORITY_INFORMATION {
        MemoryPriority: level.raw(),
    };
    // SAFETY: `info` is the struct this information class expects.
    unsafe {
        SetProcessInformation(
            h.raw(),
            ProcessMemoryPriority,
            &info as *const _ as *const c_void,
            std::mem::size_of::<MEMORY_PRIORITY_INFORMATION>() as u32,
        )
    }
    .map_err(|e| Error::os(format!("memory priority (pid {pid}): {e}")))
}

/// Opt a process into (or out of) EcoQoS.
///
/// `on = false` clears RamWarden's request and hands the decision back to the
/// system, rather than forcing high performance.
pub fn set_eco_qos(pid: u32, on: bool) -> Result<()> {
    let h = open(pid, PRIORITY_ACCESS)?;
    let state = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: if on { PROCESS_POWER_THROTTLING_EXECUTION_SPEED } else { 0 },
        StateMask: if on { PROCESS_POWER_THROTTLING_EXECUTION_SPEED } else { 0 },
    };
    // SAFETY: `state` is the struct this information class expects.
    unsafe {
        SetProcessInformation(
            h.raw(),
            ProcessPowerThrottling,
            &state as *const _ as *const c_void,
            std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        )
    }
    .map_err(|e| Error::os(format!("EcoQoS (pid {pid}): {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn priority_and_eco_qos_apply_and_revert_on_a_child() {
        let mut child = std::process::Command::new("ping")
            .args(["-n", "30", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        set_memory_priority(pid, MemoryPriority::Low).unwrap();
        set_eco_qos(pid, true).unwrap();
        set_memory_priority(pid, MemoryPriority::Normal).unwrap();
        set_eco_qos(pid, false).unwrap();
        child.kill().ok();
        child.wait().ok();
    }

    #[test]
    fn the_kernel_is_refused() {
        assert!(set_memory_priority(4, MemoryPriority::Low).unwrap_err().is_denied());
    }
}
