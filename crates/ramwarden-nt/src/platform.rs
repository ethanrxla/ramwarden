//! The Windows implementation of `ramwarden-platform`.

use std::sync::Arc;

use ramwarden_platform::{
    Background, Outcome, Pager, Parts, Platform, ProcessControl, ProcessHandle, Result,
};

use crate::desktop::NtDesktop;
use crate::groups::NtGroups;
use crate::inventory::NtProcesses;
use crate::memory::NtMemory;
use crate::pressure::NtPressure;
use crate::priority::{self, MemoryPriority};
use crate::process::NtProcessHandle;
use crate::workingset;

/// Build the Windows platform over the live system.
pub fn platform() -> Platform {
    Platform::new(
        Parts {
            name: "windows",
            memory: Arc::new(NtMemory),
            pressure: Arc::new(NtPressure::new()),
            control: Arc::new(NtControl),
            pager: Arc::new(NtPager),
            background: Arc::new(NtBackground),
            groups: Arc::new(NtGroups::new()),
        },
        || Box::new(NtProcesses::new()),
        || Box::new(NtDesktop::new()),
    )
}

/// Opens process handles; answers point queries on single processes.
pub struct NtControl;

impl ProcessControl for NtControl {
    fn open(&self, pid: i32) -> Result<Box<dyn ProcessHandle>> {
        Ok(Box::new(NtProcessHandle::open(pid as u32)?))
    }

    fn proportional(&self, pid: i32) -> Option<u64> {
        workingset::private_working_set_of(pid as u32).ok()
    }
}

/// Per-process page-out: trim one process's working set.
///
/// There is no Windows equivalent of paging out only the *cold* mappings, so
/// `referenced_ratio` is ignored: the whole working set is trimmed and the
/// process faults back what it is actually using. `min_bytes` still applies —
/// a process holding less is not worth the faults.
pub struct NtPager;

impl Pager for NtPager {
    fn available(&self) -> bool {
        true
    }

    fn route(&self) -> &'static str {
        "working-set trim (EmptyWorkingSet)"
    }

    fn page_out(&self, pid: i32, _referenced_ratio: f64, min_bytes: u64) -> Outcome {
        let pid_u = pid as u32;
        match workingset::private_working_set_of(pid_u) {
            Ok(ws) if ws < min_bytes => {
                return Outcome::refused(format!("pid {pid}: {} MB is too small to trim", ws / 1_000_000));
            }
            Err(e) => return Outcome::refused(format!("pid {pid}: {e}")),
            Ok(_) => {}
        }
        match workingset::trim(pid_u) {
            Ok(0) => Outcome::refused(format!("pid {pid}: nothing was trimmed")),
            Ok(bytes) => Outcome::freed(vec![pid], bytes),
            // An elevated app in the user's session: ask the service, which
            // applies its own checks before touching anything.
            Err(e) if e.is_denied() => match crate::pipe::call(&crate::pipe::Request::Trim { pid: pid_u }) {
                Ok(crate::pipe::Response::Ok { bytes, .. }) if bytes > 0 => Outcome::freed(vec![pid], bytes),
                Ok(crate::pipe::Response::Ok { .. }) => Outcome::refused(format!("pid {pid}: nothing was trimmed")),
                Ok(crate::pipe::Response::Error { message }) => Outcome::refused(format!("pid {pid}: {message}")),
                Err(_) => Outcome::refused(format!("pid {pid}: {e}")),
            },
            Err(e) => Outcome::refused(format!("pid {pid}: {e}")),
        }
    }
}

/// Low memory priority plus EcoQoS — the Tidy rung's free hint.
pub struct NtBackground;

impl Background for NtBackground {
    fn available(&self) -> bool {
        true
    }

    fn set(&self, pid: i32, background: bool) -> Result<()> {
        let pid = pid as u32;
        let level = if background { MemoryPriority::Low } else { MemoryPriority::Normal };
        priority::set_memory_priority(pid, level)?;
        // EcoQoS is best-effort: some processes (games, elevated apps) refuse
        // it, and the memory priority alone is what matters for memory.
        let _ = priority::set_eco_qos(pid, background);
        Ok(())
    }
}
