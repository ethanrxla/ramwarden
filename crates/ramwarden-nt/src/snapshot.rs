//! The whole process table in one system call.
//!
//! `NtQuerySystemInformation(SystemProcessInformation)` returns every process
//! with its private working set, CPU times, hard-fault count and every thread's
//! state — the figures Task Manager shows — without opening a single process.
//! That is both cheaper than the Toolhelp/`OpenProcess` route and complete:
//! processes this user cannot open are still measured.

use std::ffi::c_void;

use ramwarden_platform::{Error, Result};

use crate::ntdll::{self, SystemProcessInformation, SystemThreadInformation};

/// One process, as the NT process table reports it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NtProcess {
    pub pid: u32,
    pub ppid: u32,
    /// Image name without `.exe`, e.g. `brave`, `Discord`.
    pub name: String,
    pub session: u32,
    pub threads: u32,
    /// Every thread is suspended — what `T` means on Linux.
    pub suspended: bool,
    pub working_set: u64,
    /// Private working set: pages only this process uses. The figure that is
    /// meaningful to sum, as PSS is on Linux.
    pub private_working_set: u64,
    /// Private committed bytes.
    pub private_commit: u64,
    /// User + kernel CPU time, in 100 ns units.
    pub cpu_time: u64,
    /// FILETIME of process creation (100 ns since 1601).
    pub create_time: i64,
    pub hard_faults: u32,
}

impl NtProcess {
    pub fn cpu_seconds(&self) -> f64 {
        self.cpu_time as f64 / 10_000_000.0
    }

    /// Seconds since creation, given the current FILETIME.
    pub fn age_seconds(&self, now_filetime: i64) -> f64 {
        if self.create_time <= 0 {
            return 0.0;
        }
        ((now_filetime - self.create_time) as f64 / 10_000_000.0).max(0.0)
    }

    /// The pseudo-processes the kernel lists alongside real ones.
    pub fn is_kernel(&self) -> bool {
        self.pid <= 4
            || matches!(
                self.name.as_str(),
                "Registry" | "Memory Compression" | "Secure System" | "vmmem" | "MemCompression"
            )
    }
}

/// Strip a trailing `.exe`, case-insensitively.
pub fn short_name(image: &str) -> String {
    let n = image.len();
    if n > 4 && image[n - 4..].eq_ignore_ascii_case(".exe") {
        image[..n - 4].to_string()
    } else {
        image.to_string()
    }
}

/// The current time as a FILETIME, for [`NtProcess::age_seconds`].
pub fn now_filetime() -> i64 {
    // 1601-01-01 to 1970-01-01 in 100 ns units.
    const EPOCH_DIFF: i64 = 116_444_736_000_000_000;
    let since_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    EPOCH_DIFF + (since_unix.as_nanos() / 100) as i64
}

/// Read the whole process table.
pub fn processes() -> Result<Vec<NtProcess>> {
    let buf = query_process_information()?;
    // SAFETY: `buf` holds what the kernel wrote for this information class.
    Ok(unsafe { parse(&buf) })
}

/// Call `NtQuerySystemInformation`, growing the buffer until it fits.
///
/// The table can grow between the sizing call and the real one, so the size
/// the kernel asks for is padded and the call is retried rather than trusted.
fn query_process_information() -> Result<Vec<u64>> {
    // u64 elements, so the buffer is 8-byte aligned for the structs inside it.
    let mut words: usize = 64 * 1024;
    for _ in 0..8 {
        let mut buf = vec![0u64; words];
        let mut needed = 0u32;
        // SAFETY: the buffer is valid for `words * 8` bytes and outlives the call.
        let status = unsafe {
            ntdll::NtQuerySystemInformation(
                ntdll::SYSTEM_PROCESS_INFORMATION,
                buf.as_mut_ptr() as *mut c_void,
                (words * 8) as u32,
                &mut needed,
            )
        };
        if ntdll::success(status) {
            return Ok(buf);
        }
        if status == ntdll::STATUS_INFO_LENGTH_MISMATCH || status == ntdll::STATUS_BUFFER_TOO_SMALL {
            words = (needed as usize / 8 + 1).max(words) + 16 * 1024;
            continue;
        }
        return Err(Error::os(format!(
            "NtQuerySystemInformation(SystemProcessInformation) failed: {status:#x}"
        )));
    }
    Err(Error::os("process table kept growing faster than it could be read"))
}

/// Walk the entries the kernel wrote.
///
/// # Safety
/// `buf` must contain a complete `SystemProcessInformation` result.
unsafe fn parse(buf: &[u64]) -> Vec<NtProcess> {
    let base = buf.as_ptr() as *const u8;
    let end = buf.len() * 8;
    let mut out = Vec::with_capacity(512);
    let mut offset = 0usize;

    loop {
        if offset + std::mem::size_of::<SystemProcessInformation>() > end {
            break;
        }
        // SAFETY: bounds checked above; the kernel aligns entries to 8 bytes.
        let p = unsafe { &*(base.add(offset) as *const SystemProcessInformation) };

        let threads_at = offset + std::mem::size_of::<SystemProcessInformation>();
        let n = p.number_of_threads as usize;
        let mut suspended = n > 0;
        for i in 0..n {
            let at = threads_at + i * std::mem::size_of::<SystemThreadInformation>();
            if at + std::mem::size_of::<SystemThreadInformation>() > end {
                suspended = false;
                break;
            }
            // SAFETY: bounds checked on the line above.
            let t = unsafe { &*(base.add(at) as *const SystemThreadInformation) };
            if t.thread_state != ntdll::THREAD_STATE_WAITING
                || t.wait_reason != ntdll::WAIT_REASON_SUSPENDED
            {
                suspended = false;
                break;
            }
        }

        let pid = p.unique_process_id as u32;
        // SAFETY: the image name buffer points into `buf`.
        let image = unsafe { p.image_name.to_string_lossy() };
        let name = match (pid, image.is_empty()) {
            (0, _) => "Idle".to_string(),
            (_, true) => "System".to_string(),
            _ => short_name(&image),
        };

        out.push(NtProcess {
            pid,
            ppid: p.inherited_from_unique_process_id as u32,
            name,
            session: p.session_id,
            threads: p.number_of_threads,
            suspended,
            working_set: p.working_set_size as u64,
            private_working_set: p.working_set_private_size.max(0) as u64,
            private_commit: p.private_page_count as u64,
            cpu_time: (p.user_time.max(0) + p.kernel_time.max(0)) as u64,
            create_time: p.create_time,
            hard_faults: p.hard_fault_count,
        });

        if p.next_entry_offset == 0 {
            break;
        }
        offset += p.next_entry_offset as usize;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exe_is_stripped_case_insensitively() {
        assert_eq!(short_name("brave.exe"), "brave");
        assert_eq!(short_name("Discord.EXE"), "Discord");
        assert_eq!(short_name("Memory Compression"), "Memory Compression");
        assert_eq!(short_name(".exe"), ".exe");
    }

    /// Against the live kernel: this process must be in the table, with a
    /// plausible private working set and its parent recorded.
    #[test]
    fn the_live_table_contains_this_process() {
        let table = processes().expect("process table");
        assert!(table.len() > 20, "only {} processes", table.len());
        let me = std::process::id();
        let entry = table.iter().find(|p| p.pid == me).expect("this process");
        assert!(entry.private_working_set > 0);
        assert!(entry.working_set >= entry.private_working_set);
        assert!(entry.threads > 0);
        assert!(!entry.suspended, "a running test is not suspended");
        assert!(entry.age_seconds(now_filetime()) < 3600.0);
    }

    #[test]
    fn kernel_pseudo_processes_are_recognised() {
        let table = processes().unwrap();
        assert!(table.iter().any(|p| p.pid == 4 && p.is_kernel()));
        assert!(table.iter().any(|p| p.pid == 0 && p.name == "Idle"));
    }
}
