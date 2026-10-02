//! pidfd: signal the process you meant, not whoever now holds that number.
//!
//! # The bug this fixes
//!
//! RamWarden v1 tracked suspended applications as a name and a list of PIDs,
//! then re-resolved them at resume time. `process_manager.py` carries a comment
//! admitting the hazard: after a frozen app is force-quit and relaunched, the
//! same name matches a different process tree, and a recycled PID can be
//! signalled by mistake. With `SIGCONT` that is harmless; the v2 ladder also
//! sends `SIGTERM` and `SIGKILL` autonomously, where signalling the wrong
//! process means killing something the user never agreed to lose.
//!
//! A pidfd is a handle on a *specific* process. Once that process exits the
//! handle refuses to signal anything, whatever happens to the number. PID reuse
//! stops being a race and becomes an impossibility.

use std::io;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::time::Duration;

use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::sys::signal::Signal;

use crate::{Error, Result};

/// A handle on one specific process.
#[derive(Debug)]
pub struct PidFd {
    fd: OwnedFd,
    pid: i32,
}

impl PidFd {
    /// Pin a running process.
    ///
    /// Fails with `ESRCH` if the process is already gone — which is the point:
    /// there is no window in which this silently latches onto a successor.
    pub fn open(pid: i32) -> Result<Self> {
        // SAFETY: pidfd_open takes a pid and a flag word and returns a new fd or
        // -1; it touches no memory we own.
        let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0) };
        if raw < 0 {
            let err = io::Error::last_os_error();
            return Err(match err.raw_os_error() {
                Some(libc::ENOSYS) => Error::Unsupported("pidfd_open (needs Linux 5.3+)"),
                Some(libc::EPERM) => Error::Denied {
                    path: format!("/proc/{pid}").into(),
                    hint: "process belongs to another user",
                },
                _ => Error::io(format!("/proc/{pid} (pidfd_open)"), err),
            });
        }
        // SAFETY: the syscall returned a fresh, owned fd.
        let fd = unsafe { OwnedFd::from_raw_fd(raw as i32) };
        Ok(PidFd { fd, pid })
    }

    /// The PID this handle was opened on. For logging only — never re-resolve a
    /// process from it, which is the mistake this type exists to prevent.
    pub fn pid(&self) -> i32 {
        self.pid
    }

    /// Send a signal to exactly this process.
    pub fn send_signal(&self, sig: Signal) -> Result<()> {
        // SAFETY: a null siginfo asks the kernel to synthesise one, as
        // documented for pidfd_send_signal.
        let rc = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                self.fd.as_raw_fd(),
                sig as libc::c_int,
                std::ptr::null_mut::<libc::siginfo_t>(),
                0,
            )
        };
        if rc < 0 {
            let err = io::Error::last_os_error();
            return Err(match err.raw_os_error() {
                Some(libc::ESRCH) => Error::io(
                    format!("pid {} (already exited)", self.pid),
                    io::Error::from(io::ErrorKind::NotFound),
                ),
                Some(libc::EPERM) => Error::Denied {
                    path: format!("/proc/{}", self.pid).into(),
                    hint: "no permission to signal this process",
                },
                _ => Error::io(format!("pid {} (pidfd_send_signal)", self.pid), err),
            });
        }
        Ok(())
    }

    /// Stop the process. Reversible with [`PidFd::resume`], but note that a
    /// stopped GUI application is indistinguishable from a crashed one — prefer
    /// cgroup reclaim wherever it will do.
    pub fn suspend(&self) -> Result<()> {
        self.send_signal(Signal::SIGSTOP)
    }

    pub fn resume(&self) -> Result<()> {
        self.send_signal(Signal::SIGCONT)
    }

    /// Hang up, as a closing terminal does.
    ///
    /// The signal to use on an interactive shell: it is a session leader with a
    /// controlling terminal, so it *discards* `SIGTERM` entirely and only
    /// `SIGHUP` gets its attention.
    pub fn hangup(&self) -> Result<()> {
        self.send_signal(Signal::SIGHUP)
    }

    /// Ask the process to exit, giving it the chance to save.
    pub fn terminate(&self) -> Result<()> {
        self.send_signal(Signal::SIGTERM)
    }

    pub fn kill(&self) -> Result<()> {
        self.send_signal(Signal::SIGKILL)
    }

    /// Whether the process is still running. A pidfd becomes readable exactly
    /// when its process exits, so this needs no `/proc` lookup and cannot be
    /// confused by a recycled PID.
    pub fn is_alive(&self) -> bool {
        !self.has_exited()
    }

    fn has_exited(&self) -> bool {
        let mut fds = [PollFd::new(self.fd.as_fd(), PollFlags::POLLIN)];
        matches!(poll(&mut fds, PollTimeout::ZERO), Ok(n) if n > 0)
    }

    /// Block until the process exits, or `timeout` elapses.
    ///
    /// Returns `true` if it exited. Used by the ladder's terminate step to give
    /// an application a real chance to shut down before escalating to `SIGKILL`,
    /// rather than v1's fixed three-second sleep.
    pub fn wait_exit(&self, timeout: Duration) -> Result<bool> {
        let mut fds = [PollFd::new(self.fd.as_fd(), PollFlags::POLLIN)];
        let ms: u16 = timeout.as_millis().try_into().unwrap_or(u16::MAX);
        loop {
            match poll(&mut fds, PollTimeout::from(ms)) {
                Ok(0) => return Ok(false),
                Ok(_) => return Ok(true),
                Err(nix::errno::Errno::EINTR) => continue,
                Err(e) => {
                    return Err(Error::io(
                        format!("pid {} (poll)", self.pid),
                        io::Error::from(e),
                    ));
                }
            }
        }
    }

    pub(crate) fn as_raw(&self) -> i32 {
        self.fd.as_raw_fd()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Child, Command};

    fn spawn_sleeper() -> Child {
        Command::new("sleep").arg("60").spawn().expect("spawn sleep")
    }

    fn state_of(pid: i32) -> char {
        // /proc/<pid>/stat: the state field follows the parenthesised comm, and
        // comm may itself contain spaces or brackets — so split on the last ')'.
        let s = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
        let tail = &s[s.rfind(')').unwrap() + 1..];
        tail.trim_start().chars().next().unwrap()
    }

    #[test]
    fn opens_and_reports_a_live_process() {
        let mut child = spawn_sleeper();
        let fd = PidFd::open(child.id() as i32).unwrap();
        assert_eq!(fd.pid(), child.id() as i32);
        assert!(fd.is_alive());
        child.kill().ok();
        child.wait().ok();
    }

    #[test]
    fn suspends_and_resumes_a_real_process() {
        let mut child = spawn_sleeper();
        let pid = child.id() as i32;
        let fd = PidFd::open(pid).unwrap();

        fd.suspend().unwrap();
        // The state change is synchronous for a sleeping task, but give the
        // scheduler a moment on a loaded machine.
        for _ in 0..50 {
            if state_of(pid) == 'T' {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(state_of(pid), 'T', "SIGSTOP should leave it stopped");

        fd.resume().unwrap();
        for _ in 0..50 {
            if state_of(pid) != 'T' {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_ne!(state_of(pid), 'T', "SIGCONT should wake it");

        child.kill().ok();
        child.wait().ok();
    }

    #[test]
    fn wait_exit_observes_a_kill() {
        let mut child = spawn_sleeper();
        let fd = PidFd::open(child.id() as i32).unwrap();
        assert!(!fd.wait_exit(Duration::from_millis(50)).unwrap(), "still alive");

        fd.kill().unwrap();
        assert!(fd.wait_exit(Duration::from_secs(5)).unwrap(), "should have exited");
        child.wait().ok();
    }

    /// The whole reason this module exists. Once the process is gone and reaped,
    /// its PID is free for the kernel to reissue — and this handle must refuse
    /// to signal, rather than hitting whoever inherited the number.
    #[test]
    fn a_handle_to_an_exited_process_refuses_to_signal() {
        let mut child = spawn_sleeper();
        let pid = child.id() as i32;
        let fd = PidFd::open(pid).unwrap();

        fd.kill().unwrap();
        child.wait().unwrap(); // reap, releasing the PID for reuse
        assert!(fd.wait_exit(Duration::from_secs(5)).unwrap());

        let err = fd.send_signal(Signal::SIGCONT).unwrap_err();
        assert!(err.is_missing(), "expected ESRCH, got {err:?}");
        assert!(!fd.is_alive());
    }

    /// Wait until `pid` has installed an ignore disposition for signal number
    /// `signo`, so the test does not race the shell's own startup.
    fn await_ignored(pid: i32, signo: u32) -> bool {
        let mask = 1u64 << (signo - 1);
        for _ in 0..200 {
            if let Ok(status) = std::fs::read_to_string(format!("/proc/{pid}/status"))
                && let Some(line) = status.lines().find(|l| l.starts_with("SigIgn:"))
                && let Some(hex) = line.split_whitespace().nth(1)
                && let Ok(bits) = u64::from_str_radix(hex, 16)
                && bits & mask != 0
            {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    /// An interactive shell ignores SIGTERM but acts on SIGHUP. This is the
    /// distinction v1 got wrong, reporting shells closed that were still alive.
    #[test]
    fn a_shell_ignoring_sigterm_still_responds_to_hangup() {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg("trap '' TERM; sleep 60")
            .spawn()
            .unwrap();
        let pid = child.id() as i32;
        let fd = PidFd::open(pid).unwrap();

        // Signalling before the shell has run `trap` would kill it with the
        // default disposition and test nothing.
        assert!(await_ignored(pid, 15), "shell never installed the SIGTERM trap");

        fd.terminate().unwrap();
        assert!(
            !fd.wait_exit(Duration::from_millis(300)).unwrap(),
            "SIGTERM is trapped, so it must survive"
        );

        fd.hangup().unwrap();
        assert!(fd.wait_exit(Duration::from_secs(5)).unwrap(), "SIGHUP should land");
        child.wait().ok();
    }

    #[test]
    fn opening_a_process_that_does_not_exist_fails_immediately() {
        // Above the kernel's pid_max on any realistic configuration.
        let err = PidFd::open(0x7FFF_FFFE).unwrap_err();
        assert!(!matches!(err, Error::Unsupported(_)), "{err:?}");
    }
}
