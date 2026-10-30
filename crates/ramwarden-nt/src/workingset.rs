//! Working-set levers: the Windows counterpart of cgroup reclaim and soft caps.
//!
//! `EmptyWorkingSet` removes every page from a process's working set. Nothing is
//! freed in the destructive sense: private pages move to the modified list (and
//! from there to the compression store or the page file), shared pages stay
//! resident for whoever else maps them, and anything the process touches again
//! soft-faults straight back. That is the same contract as `memory.reclaim` —
//! reversible and invisible to the application — and why the ladder reaches
//! for it first.
//!
//! Every function measures rather than assumes: the bytes reported are the
//! private working set before minus after, so pages the process immediately
//! touched again are not claimed as freed.

use ramwarden_platform::{Error, Result};
use windows::Win32::System::ProcessStatus::{
    EmptyWorkingSet, GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX2,
};
use windows::Win32::System::Memory::{
    GetProcessWorkingSetSizeEx, QUOTA_LIMITS_HARDWS_MAX_DISABLE, QUOTA_LIMITS_HARDWS_MAX_ENABLE,
    QUOTA_LIMITS_HARDWS_MIN_DISABLE, SETPROCESSWORKINGSETSIZEEX_FLAGS, SetProcessWorkingSetSizeEx,
};

use crate::handle::Handle;
use crate::process::{MEMORY_ACCESS, QUERY_ACCESS, open};

/// Private working set of an open process, in bytes.
pub fn private_working_set(h: &Handle) -> Result<u64> {
    let mut c = PROCESS_MEMORY_COUNTERS_EX2 {
        cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX2>() as u32,
        ..Default::default()
    };
    // SAFETY: EX2 begins with PROCESS_MEMORY_COUNTERS and `cb` says how much
    // the kernel may write.
    unsafe {
        GetProcessMemoryInfo(
            h.raw(),
            &mut c as *mut PROCESS_MEMORY_COUNTERS_EX2 as *mut PROCESS_MEMORY_COUNTERS,
            c.cb,
        )
    }
    .map_err(|e| Error::os(format!("GetProcessMemoryInfo: {e}")))?;
    Ok(c.PrivateWorkingSetSize as u64)
}

/// Private working set of `pid`, opening it briefly.
pub fn private_working_set_of(pid: u32) -> Result<u64> {
    private_working_set(&open(pid, QUERY_ACCESS)?)
}

/// Trim one process's working set. Returns the private bytes measurably moved
/// out of it.
pub fn trim(pid: u32) -> Result<u64> {
    let h = open(pid, MEMORY_ACCESS)?;
    let before = private_working_set(&h)?;
    // SAFETY: the handle is open with the query and set-quota rights.
    unsafe { EmptyWorkingSet(h.raw()) }
        .map_err(|e| Error::os(format!("EmptyWorkingSet(pid {pid}): {e}")))?;
    let after = private_working_set(&h).unwrap_or(before);
    Ok(before.saturating_sub(after))
}

/// A process's working-set limits, as they were before RamWarden touched them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub min: usize,
    pub max: usize,
    pub flags: u32,
}

pub fn limits(pid: u32) -> Result<Limits> {
    let h = open(pid, QUERY_ACCESS)?;
    let (mut min, mut max, mut flags) = (0usize, 0usize, 0u32);
    // SAFETY: out-pointers to locals.
    unsafe { GetProcessWorkingSetSizeEx(h.raw(), &mut min, &mut max, &mut flags) }
        .ok()
        .map_err(|e| Error::os(format!("GetProcessWorkingSetSizeEx(pid {pid}): {e}")))?;
    Ok(Limits { min, max, flags })
}

/// Cap a process's working set at `max_bytes`.
///
/// A *hard* maximum: above it the process pages against itself — slower, never
/// killed — which is what `memory.high` does to a cgroup on Linux. The minimum
/// stays soft so the memory manager may still trim below it.
pub fn cap(pid: u32, max_bytes: u64) -> Result<()> {
    let h = open(pid, MEMORY_ACCESS)?;
    let current = limits(pid)?;
    let max = (max_bytes as usize).max(current.min + 4 * 1024 * 1024);
    set(&h, pid, current.min.min(max), max, QUOTA_LIMITS_HARDWS_MIN_DISABLE.0 | QUOTA_LIMITS_HARDWS_MAX_ENABLE.0)
}

/// Put back the limits recorded before the first cap.
pub fn restore(pid: u32, previous: Limits) -> Result<()> {
    let h = open(pid, MEMORY_ACCESS)?;
    // Reinstate soft limits even if the original flags were unreadable: a
    // stray hard cap is the one outcome that must never outlive pressure.
    let flags = if previous.flags == 0 {
        QUOTA_LIMITS_HARDWS_MIN_DISABLE.0 | QUOTA_LIMITS_HARDWS_MAX_DISABLE.0
    } else {
        previous.flags
    };
    set(&h, pid, previous.min, previous.max, flags)
}

fn set(h: &Handle, pid: u32, min: usize, max: usize, flags: u32) -> Result<()> {
    // SAFETY: the handle is open with PROCESS_SET_QUOTA.
    unsafe { SetProcessWorkingSetSizeEx(h.raw(), min, max, SETPROCESSWORKINGSETSIZEEX_FLAGS(flags)) }
        .map_err(|e| Error::os(format!("SetProcessWorkingSetSizeEx(pid {pid}): {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// A child that dirties ~200 MB of private memory, then idles.
    fn hog() -> std::process::Child {
        Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                "$b = New-Object byte[] (200MB); for ($i=0; $i -lt $b.Length; $i+=4096) { $b[$i]=1 }; Start-Sleep 60",
            ])
            .stdout(std::process::Stdio::null())
            .spawn()
            .expect("spawn powershell")
    }

    fn wait_for_ws(pid: u32, at_least: u64) -> u64 {
        for _ in 0..100 {
            if let Ok(ws) = private_working_set_of(pid)
                && ws >= at_least
            {
                return ws;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        private_working_set_of(pid).unwrap_or(0)
    }

    #[test]
    fn trimming_an_idle_process_measurably_moves_its_private_pages() {
        let mut child = hog();
        let pid = child.id();
        let before = wait_for_ws(pid, 150 * 1024 * 1024);
        assert!(before >= 150 * 1024 * 1024, "hog only reached {before}");
        std::thread::sleep(std::time::Duration::from_millis(500));

        let moved = trim(pid).unwrap();
        assert!(moved > 100 * 1024 * 1024, "only {moved} bytes moved");
        assert!(private_working_set_of(pid).unwrap() < before);

        child.kill().ok();
        child.wait().ok();
    }

    #[test]
    fn a_cap_is_set_and_then_restored_exactly() {
        let mut child = Command::new("ping")
            .args(["-n", "30", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        let original = limits(pid).unwrap();

        cap(pid, 64 * 1024 * 1024).unwrap();
        let capped = limits(pid).unwrap();
        assert_ne!(capped.flags & QUOTA_LIMITS_HARDWS_MAX_ENABLE.0, 0, "max must be hard");

        restore(pid, original).unwrap();
        assert_eq!(limits(pid).unwrap(), original);

        child.kill().ok();
        child.wait().ok();
    }
}
