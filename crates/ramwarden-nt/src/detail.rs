//! What is inside one process: threads, handles and hosted services.
//!
//! Threads come from the same process table the sampler reads. The handle
//! count needs only `PROCESS_QUERY_LIMITED_INFORMATION`, which an unelevated
//! user holds on their own processes. Services are the Service Control
//! Manager's own pid-to-service map, which is what tells one `svchost` from
//! another. Naming the files behind the handles needs a system-wide handle
//! walk and is not attempted.

use ramwarden_platform::ProcessDetail;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Services::{
    CloseServiceHandle, ENUM_SERVICE_STATUS_PROCESSW, EnumServicesStatusExW, OpenSCManagerW, SC_ENUM_PROCESS_INFO,
    SC_MANAGER_ENUMERATE_SERVICE, SERVICE_ACTIVE, SERVICE_WIN32,
};
use windows::Win32::System::Threading::{GetProcessHandleCount, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

use crate::handle::Handle;

/// Services listed per process: a shared `svchost` can host dozens.
const MAX_SERVICES: usize = 24;

pub fn detail(pid: u32) -> Option<ProcessDetail> {
    let threads = crate::snapshot::processes().ok()?.into_iter().find(|p| p.pid == pid).map(|p| p.threads)?;
    Some(ProcessDetail { threads: Some(threads), open_handles: handle_count(pid), open_files: Vec::new(), services: services_of(pid) })
}

fn handle_count(pid: u32) -> Option<u32> {
    // SAFETY: plain FFI; the handle is owned and closed by `Handle`.
    let raw: HANDLE = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let handle = Handle::new(raw)?;
    let mut count = 0u32;
    // SAFETY: `handle` is a live process handle and `count` is writable.
    unsafe { GetProcessHandleCount(handle.raw(), &mut count) }.ok()?;
    Some(count)
}

/// Names of the running Win32 services hosted by `pid`. Empty on any failure.
fn services_of(pid: u32) -> Vec<String> {
    // SAFETY: plain FFI; the manager handle is closed below.
    let Ok(scm) = (unsafe { OpenSCManagerW(None, None, SC_MANAGER_ENUMERATE_SERVICE) }) else {
        return Vec::new();
    };
    let mut needed = 0u32;
    let mut returned = 0u32;
    // SAFETY: an empty buffer asks only for the size needed.
    let _ = unsafe {
        EnumServicesStatusExW(scm, SC_ENUM_PROCESS_INFO, SERVICE_WIN32, SERVICE_ACTIVE, None, &mut needed, &mut returned, None, None)
    };
    let mut out = Vec::new();
    if needed > 0 {
        // u64 storage keeps the records aligned; padded because services can
        // start between the two calls.
        let mut buf = vec![0u64; (needed as usize + 4096).div_ceil(8)];
        // SAFETY: the slice covers exactly the buffer's bytes.
        let bytes = unsafe { std::slice::from_raw_parts_mut(buf.as_mut_ptr().cast::<u8>(), buf.len() * 8) };
        // SAFETY: `bytes` is writable for its whole length.
        let ok = unsafe {
            EnumServicesStatusExW(scm, SC_ENUM_PROCESS_INFO, SERVICE_WIN32, SERVICE_ACTIVE, Some(bytes), &mut needed, &mut returned, None, None)
        };
        if ok.is_ok() {
            // SAFETY: on success the buffer starts with `returned` records,
            // whose name pointers point into the same buffer.
            let rows = unsafe { std::slice::from_raw_parts(buf.as_ptr().cast::<ENUM_SERVICE_STATUS_PROCESSW>(), returned as usize) };
            for row in rows.iter().filter(|r| r.ServiceStatusProcess.dwProcessId == pid).take(MAX_SERVICES) {
                // SAFETY: the SCM wrote a NUL-terminated name.
                if let Ok(name) = unsafe { row.lpServiceName.to_string() } {
                    out.push(name);
                }
            }
        }
    }
    // SAFETY: `scm` came from OpenSCManagerW and is closed once.
    let _ = unsafe { CloseServiceHandle(scm) };
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_process_has_threads_and_handles() {
        let d = detail(std::process::id()).unwrap();
        assert!(d.threads.is_some_and(|n| n >= 1));
        assert!(d.open_handles.is_some_and(|n| n >= 1));
        assert!(detail(u32::MAX - 7).is_none());
    }
}
