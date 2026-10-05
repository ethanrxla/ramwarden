//! A handle on one specific process, without pidfd.
//!
//! macOS has no pidfd, so PID reuse is guarded by identity instead: the handle
//! records the process start time when opened and re-checks it before every
//! signal. A recycled PID has a different start time, so the signal is refused
//! rather than delivered to a stranger. The check and the `kill` are not atomic,
//! which leaves a window of microseconds; it is documented rather than hidden.

use std::io;
use std::time::{Duration, Instant};

use nix::sys::signal::Signal;

use crate::{Error, Result, macos};

#[derive(Debug)]
pub struct PidFd {
    pid: i32,
    start: (u64, u64),
}

fn start_of(pid: i32) -> Option<(u64, u64)> {
    macos::bsd_info(pid).map(|b| (b.pbi_start_tvsec, b.pbi_start_tvusec))
}

impl PidFd {
    pub fn open(pid: i32) -> Result<Self> {
        match start_of(pid) {
            Some(start) => Ok(PidFd { pid, start }),
            None => Err(Error::io(
                format!("pid {pid}"),
                io::Error::from(io::ErrorKind::NotFound),
            )),
        }
    }

    /// Meaningless on macOS; kept so callers that log the descriptor compile.
    pub fn as_raw(&self) -> i32 {
        -1
    }

    pub fn pid(&self) -> i32 {
        self.pid
    }

    pub fn send_signal(&self, sig: Signal) -> Result<()> {
        if start_of(self.pid) != Some(self.start) {
            return Err(Error::io(
                format!("pid {} (already exited)", self.pid),
                io::Error::from(io::ErrorKind::NotFound),
            ));
        }
        // SAFETY: kill takes a pid and signal number; no memory is involved.
        let rc = unsafe { libc::kill(self.pid, sig as libc::c_int) };
        if rc == 0 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        Err(match err.raw_os_error() {
            Some(libc::ESRCH) => Error::io(
                format!("pid {} (already exited)", self.pid),
                io::Error::from(io::ErrorKind::NotFound),
            ),
            Some(libc::EPERM) => Error::Denied {
                path: format!("pid {}", self.pid).into(),
                hint: "no permission to signal this process",
            },
            _ => Error::io(format!("pid {} (kill)", self.pid), err),
        })
    }

    pub fn suspend(&self) -> Result<()> {
        self.send_signal(Signal::SIGSTOP)
    }

    pub fn resume(&self) -> Result<()> {
        self.send_signal(Signal::SIGCONT)
    }

    pub fn hangup(&self) -> Result<()> {
        self.send_signal(Signal::SIGHUP)
    }

    pub fn terminate(&self) -> Result<()> {
        self.send_signal(Signal::SIGTERM)
    }

    pub fn kill(&self) -> Result<()> {
        self.send_signal(Signal::SIGKILL)
    }

    pub fn is_alive(&self) -> bool {
        match macos::bsd_info(self.pid) {
            Some(b) => (b.pbi_start_tvsec, b.pbi_start_tvusec) == self.start && b.pbi_status != 5,
            None => false,
        }
    }

    pub fn wait_exit(&self, timeout: Duration) -> Result<bool> {
        let deadline = Instant::now() + timeout;
        loop {
            if !self.is_alive() {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracks_a_child_and_refuses_after_exit() {
        let mut child = std::process::Command::new("sleep").arg("30").spawn().unwrap();
        let h = PidFd::open(child.id() as i32).unwrap();
        assert!(h.is_alive());
        h.suspend().unwrap();
        h.resume().unwrap();
        h.kill().unwrap();
        child.wait().unwrap();
        assert!(!h.is_alive());
        assert!(h.send_signal(Signal::SIGCONT).is_err());
    }
}
