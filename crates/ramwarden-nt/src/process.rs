//! A handle on one specific process — the Windows counterpart of a pidfd.
//!
//! A process `HANDLE` names a kernel process *object*, not a number. If the
//! process exits and its PID is reused, this handle still names the dead
//! object: suspend and terminate fail on it rather than reaching whoever got
//! the number next. That is the guarantee the actuator's registry relies on.

use std::time::Duration;

use ramwarden_platform::{Error, ProcessHandle, Result};
use windows::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_INVALID_PARAMETER, HWND, LPARAM, WAIT_OBJECT_0, WAIT_TIMEOUT, WPARAM,
};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_ACCESS_RIGHTS, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_SET_INFORMATION, PROCESS_SET_QUOTA, PROCESS_SUSPEND_RESUME, PROCESS_SYNCHRONIZE,
    PROCESS_TERMINATE,
    TerminateProcess, WaitForSingleObject,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindow, GetWindowThreadProcessId, GW_OWNER, IsWindowVisible, PostMessageW,
    WM_CLOSE,
};
use windows::core::BOOL;

use crate::handle::Handle;
use crate::ntdll;

/// Everything the actuator may do to a process it opens.
const CONTROL_ACCESS: PROCESS_ACCESS_RIGHTS = PROCESS_ACCESS_RIGHTS(
    PROCESS_SUSPEND_RESUME.0
        | PROCESS_TERMINATE.0
        | PROCESS_QUERY_LIMITED_INFORMATION.0
        | PROCESS_SYNCHRONIZE.0,
);

/// What working-set trims and soft caps need, and no more: `EmptyWorkingSet`
/// and the working-set size calls accept the limited query right plus
/// `PROCESS_SET_QUOTA`. No `PROCESS_VM_READ` — RamWarden never reads another
/// process's memory, and anti-cheat rightly treats that right as suspicious.
pub const MEMORY_ACCESS: PROCESS_ACCESS_RIGHTS =
    PROCESS_ACCESS_RIGHTS(PROCESS_QUERY_LIMITED_INFORMATION.0 | PROCESS_SET_QUOTA.0);

/// Enough to read memory counters.
pub const QUERY_ACCESS: PROCESS_ACCESS_RIGHTS = PROCESS_QUERY_LIMITED_INFORMATION;

/// What memory priority and EcoQoS need.
pub const PRIORITY_ACCESS: PROCESS_ACCESS_RIGHTS =
    PROCESS_ACCESS_RIGHTS(PROCESS_SET_INFORMATION.0 | PROCESS_QUERY_LIMITED_INFORMATION.0);

/// Open `pid` with `access`, mapping the failure modes the ladder distinguishes.
pub fn open(pid: u32, access: PROCESS_ACCESS_RIGHTS) -> Result<Handle> {
    if pid <= 4 {
        return Err(Error::denied(format!("pid {pid}"), "the kernel's own pseudo-processes"));
    }
    // SAFETY: plain FFI; the returned handle is owned immediately.
    match unsafe { OpenProcess(access, false, pid) } {
        Ok(h) => Handle::new(h).ok_or_else(|| Error::missing(format!("pid {pid}"))),
        Err(e) if e.code() == ERROR_ACCESS_DENIED.to_hresult() => Err(Error::denied(
            format!("pid {pid}"),
            "owned by another user, or protected",
        )),
        // OpenProcess reports a PID that no longer exists as an invalid parameter.
        Err(e) if e.code() == ERROR_INVALID_PARAMETER.to_hresult() => {
            Err(Error::missing(format!("pid {pid}")))
        }
        Err(e) => Err(Error::os(format!("OpenProcess({pid}): {e}"))),
    }
}

/// A process pinned by handle.
#[derive(Debug)]
pub struct NtProcessHandle {
    handle: Handle,
    pid: u32,
}

impl NtProcessHandle {
    pub fn open(pid: u32) -> Result<Self> {
        Ok(NtProcessHandle {
            handle: open(pid, CONTROL_ACCESS)?,
            pid,
        })
    }

    fn nt(&self, what: &str, f: unsafe extern "system" fn(*mut std::ffi::c_void) -> i32) -> Result<()> {
        // SAFETY: the handle is open with PROCESS_SUSPEND_RESUME.
        let status = unsafe { f(self.handle.raw().0) };
        if ntdll::success(status) {
            return Ok(());
        }
        Err(match status {
            ntdll::STATUS_PROCESS_IS_TERMINATING => Error::missing(format!("pid {}", self.pid)),
            ntdll::STATUS_ACCESS_DENIED => Error::denied(format!("pid {}", self.pid), "access denied"),
            s => Error::os(format!("{what}(pid {}): {s:#x}", self.pid)),
        })
    }

    /// Top-level windows this process owns that a user could close.
    fn closable_windows(&self) -> Vec<HWND> {
        struct Search {
            pid: u32,
            found: Vec<HWND>,
        }
        unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> BOOL {
            // SAFETY: lparam is the &mut Search passed below, alive for the call.
            let s = unsafe { &mut *(lparam.0 as *mut Search) };
            let mut owner = 0u32;
            // SAFETY: plain FFI on a window handle the system gave us.
            unsafe {
                GetWindowThreadProcessId(hwnd, Some(&mut owner));
                let unowned = GetWindow(hwnd, GW_OWNER).map(|h| h.0.is_null()).unwrap_or(true);
                if owner == s.pid && IsWindowVisible(hwnd).as_bool() && unowned {
                    s.found.push(hwnd);
                }
            }
            BOOL(1)
        }
        let mut search = Search {
            pid: self.pid,
            found: Vec::new(),
        };
        // SAFETY: `search` outlives EnumWindows, which calls back synchronously.
        unsafe {
            let _ = EnumWindows(Some(visit), LPARAM(&mut search as *mut Search as isize));
        }
        search.found
    }
}

impl ProcessHandle for NtProcessHandle {
    fn pid(&self) -> i32 {
        self.pid as i32
    }

    fn is_alive(&self) -> bool {
        // SAFETY: the handle is open with SYNCHRONIZE.
        unsafe { WaitForSingleObject(self.handle.raw(), 0) == WAIT_TIMEOUT }
    }

    fn suspend(&self) -> Result<()> {
        self.nt("NtSuspendProcess", ntdll::NtSuspendProcess)
    }

    fn resume(&self) -> Result<()> {
        self.nt("NtResumeProcess", ntdll::NtResumeProcess)
    }

    /// Ask politely: `WM_CLOSE` to each of the process's top-level windows,
    /// which is what clicking the close button sends. A windowless process has
    /// nothing to ask, so this succeeds without effect and the caller's grace
    /// period ends in [`ProcessHandle::kill`] — the same path as a Linux
    /// process ignoring `SIGTERM`.
    fn terminate(&self) -> Result<()> {
        if !self.is_alive() {
            return Err(Error::missing(format!("pid {}", self.pid)));
        }
        for hwnd in self.closable_windows() {
            // SAFETY: posting a message to a window handle is always sound.
            unsafe {
                let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
        Ok(())
    }

    fn kill(&self) -> Result<()> {
        // SAFETY: the handle is open with PROCESS_TERMINATE.
        unsafe { TerminateProcess(self.handle.raw(), 1) }.map_err(|e| {
            if !self.is_alive() {
                Error::missing(format!("pid {}", self.pid))
            } else {
                Error::os(format!("TerminateProcess(pid {}): {e}", self.pid))
            }
        })
    }

    fn wait_exit(&self, timeout: Duration) -> Result<bool> {
        let ms = timeout.as_millis().min(u32::MAX as u128 - 1) as u32;
        // SAFETY: the handle is open with SYNCHRONIZE.
        let r = unsafe { WaitForSingleObject(self.handle.raw(), ms) };
        Ok(r == WAIT_OBJECT_0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn spawn() -> std::process::Child {
        // `ping -n` is the portable Windows "sleep" that needs no console input.
        Command::new("ping")
            .args(["-n", "60", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .spawn()
            .expect("spawn ping")
    }

    fn suspended(pid: u32) -> bool {
        crate::snapshot::processes()
            .unwrap()
            .into_iter()
            .find(|p| p.pid == pid)
            .is_some_and(|p| p.suspended)
    }

    #[test]
    fn suspends_and_resumes_a_real_process() {
        let mut child = spawn();
        let h = NtProcessHandle::open(child.id()).unwrap();
        h.suspend().unwrap();
        assert!(suspended(child.id()), "every thread should be suspended");
        h.resume().unwrap();
        assert!(!suspended(child.id()));
        child.kill().ok();
        child.wait().ok();
    }

    /// The reason this type exists: once the process is gone, the handle
    /// refuses to act rather than reaching a successor with the same PID.
    #[test]
    fn a_handle_outlives_its_process_without_acting_on_anything() {
        let mut child = spawn();
        let h = NtProcessHandle::open(child.id()).unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(!h.is_alive());
        assert!(h.suspend().is_err());
        assert!(h.wait_exit(Duration::from_millis(10)).unwrap());
    }

    #[test]
    fn kill_ends_the_process() {
        let mut child = spawn();
        let h = NtProcessHandle::open(child.id()).unwrap();
        h.kill().unwrap();
        assert!(h.wait_exit(Duration::from_secs(5)).unwrap());
        child.wait().ok();
    }

    #[test]
    fn the_kernel_is_refused_before_any_call() {
        assert!(NtProcessHandle::open(4).unwrap_err().is_denied());
        assert!(NtProcessHandle::open(0).unwrap_err().is_denied());
    }
}
