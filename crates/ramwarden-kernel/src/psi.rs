//! Pressure Stall Information — how long tasks actually stalled on memory.
//!
//! # Why this replaces the percent threshold
//!
//! RamWarden v1 triggered on `used / total >= 65%`. On the machine it was built
//! for, that fires constantly while nothing is wrong: 23 GiB of 30 GiB "used"
//! included 4.64 GB of anonymous memory that zram had already compressed into
//! 1.02 GB. Percent-used cannot tell a full-but-healthy machine from a stalling
//! one, so v1 either nagged or slept through the real event.
//!
//! PSI reports the thing that actually matters — wall time lost waiting for
//! memory. `some` is the share of time at least one task stalled; `full` is the
//! share where *every* runnable task stalled, which is the signal that the
//! machine is thrashing rather than merely busy.
//!
//! # Triggers are frequently unavailable, and that is not an error
//!
//! The kernel can in principle wake a process on a stall threshold: write a
//! stall budget to an `O_RDWR` handle on the pressure file and `poll()` it.
//! [`Trigger`] does that, and callers should expect it to fail.
//!
//! Measured on the machine this was written for (Linux 7.1.5, Pop!_OS): the file
//! opens `O_RDWR` and reads fine, but **every** trigger write is rejected with
//! `EINVAL`, at every window and threshold the kernel documents, and the cgroup
//! `memory.pressure` files are not writable at all. Trigger creation is
//! privileged on this configuration; plain reading is not.
//!
//! So an open succeeding proves nothing — only an accepted write does, which is
//! why [`Trigger::memory`] performs the write up front rather than lazily. When
//! it fails, callers fall back to polling [`system_memory`], which always works.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::fd::{AsFd, AsRawFd};
use std::path::Path;
use std::time::Duration;
use std::{fs, io};

use nix::poll::{PollFd, PollFlags, PollTimeout, poll};

use crate::{Error, Result, Root};

/// One `some` or `full` line: stall share over three decaying windows, plus a
/// monotonic total.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Window {
    /// Percent of wall time stalled, averaged over the last 10 seconds. This is
    /// the figure the remediation ladder escalates on — 60s and 300s are too
    /// slow to catch a browser opening 40 tabs.
    pub avg10: f64,
    pub avg60: f64,
    pub avg300: f64,
    /// Total microseconds stalled since boot. Monotonic, so differencing it
    /// across two reads gives an exact stall figure for that interval with none
    /// of the averaging lag.
    pub total_us: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Psi {
    /// At least one task stalled on memory.
    pub some: Window,
    /// Every runnable task stalled — the machine got no useful work done.
    pub full: Window,
}

impl Psi {
    /// True when nothing has stalled at all. The common case on a healthy box,
    /// and worth short-circuiting before doing any further work.
    pub fn is_quiet(&self) -> bool {
        self.some.avg10 == 0.0 && self.full.avg10 == 0.0
    }
}

/// Parse the two-line body of any `memory.pressure` or `/proc/pressure/memory`.
pub fn parse(text: &str, path: &Path) -> Result<Psi> {
    let mut out = Psi::default();
    let mut saw_some = false;

    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let kind = match fields.next() {
            Some(k) => k,
            None => continue,
        };
        let target = match kind {
            "some" => {
                saw_some = true;
                &mut out.some
            }
            "full" => &mut out.full,
            // cpu.pressure has no `full` line on some kernels, and future
            // kernels may add resources. Ignore rather than reject.
            _ => continue,
        };

        for field in fields {
            let Some((key, value)) = field.split_once('=') else {
                continue;
            };
            match key {
                "avg10" | "avg60" | "avg300" => {
                    let v: f64 = value
                        .parse()
                        .map_err(|_| Error::parse(path, format!("{key}={value:?} is not a float")))?;
                    match key {
                        "avg10" => target.avg10 = v,
                        "avg60" => target.avg60 = v,
                        _ => target.avg300 = v,
                    }
                }
                "total" => {
                    target.total_us = value
                        .parse()
                        .map_err(|_| Error::parse(path, format!("total={value:?} is not an integer")))?;
                }
                _ => continue,
            }
        }
    }

    // Without a `some` line this is not a pressure file. Defaulting to zero
    // would tell the ladder the machine is idle, which is the worst possible
    // wrong answer here.
    if !saw_some {
        return Err(Error::parse(path, "no `some` line"));
    }
    Ok(out)
}

pub fn read(path: impl AsRef<Path>) -> Result<Psi> {
    let path = path.as_ref();
    let text = fs::read_to_string(path).map_err(|e| Error::io(path, e))?;
    parse(&text, path)
}

/// System-wide memory pressure.
pub fn system_memory(root: &Root) -> Result<Psi> {
    read(root.join("proc/pressure/memory"))
}

/// A kernel-side stall threshold that wakes us through `poll()`.
///
/// The kernel enforces a 500 ms – 10 s window and requires the stall budget to
/// fit inside it; both are validated here so a bad config surfaces as a clear
/// error rather than `EINVAL` from a write.
#[derive(Debug)]
pub struct Trigger {
    file: File,
    spec: String,
}

impl Trigger {
    /// Ask to be woken when tasks stall for more than `stall` out of every
    /// `window`. For example 150 ms per 1 s is roughly "15% pressure sustained".
    pub fn memory(root: &Root, stall: Duration, window: Duration) -> Result<Self> {
        let path = root.join("proc/pressure/memory");

        if !(Duration::from_millis(500)..=Duration::from_secs(10)).contains(&window) {
            return Err(Error::parse(
                &path,
                format!("window {window:?} outside the kernel's 500ms..=10s range"),
            ));
        }
        if stall > window {
            return Err(Error::parse(
                &path,
                format!("stall {stall:?} cannot exceed window {window:?}"),
            ));
        }

        let spec = format!("some {} {}", stall.as_micros(), window.as_micros());

        // O_RDWR is what makes this a trigger rather than a plain read. It is
        // also the capability probe: a kernel built without CONFIG_PSI, or one
        // with psi=0 on the command line, refuses the open.
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|e| match e.kind() {
                io::ErrorKind::PermissionDenied | io::ErrorKind::InvalidInput => Error::Denied {
                    path: path.clone(),
                    hint: "kernel built without CONFIG_PSI, or booted with psi=0",
                },
                _ => Error::io(&path, e),
            })?;

        // Write immediately rather than on first use. The kernel validates the
        // trigger here, and this is the only way to learn whether triggers are
        // actually permitted — a successful open tells you nothing.
        file.write_all(spec.as_bytes()).map_err(|e| {
            if e.raw_os_error() == Some(nix::errno::Errno::EINVAL as i32) {
                // Not a bad argument from us: this kernel restricts trigger
                // creation to privileged callers. Reading still works.
                Error::Unsupported(
                    "PSI trigger creation (this kernel permits it only to privileged callers; \
                     poll the pressure file instead)",
                )
            } else {
                Error::io(&path, e)
            }
        })?;

        Ok(Trigger { file, spec })
    }

    /// Block until the threshold trips, or `timeout` elapses.
    ///
    /// Returns `true` if pressure tripped the trigger, `false` on timeout — the
    /// caller uses a timeout tick to re-sample and de-escalate, which is how
    /// suspended apps get resumed once pressure has passed.
    pub fn wait(&mut self, timeout: Duration) -> Result<bool> {
        let fd = self.file.as_fd();
        // The kernel signals a PSI trigger with POLLPRI, not POLLIN.
        let mut fds = [PollFd::new(fd, PollFlags::POLLPRI)];
        let ms: u16 = timeout.as_millis().try_into().unwrap_or(u16::MAX);

        loop {
            match poll(&mut fds, PollTimeout::from(ms)) {
                Ok(0) => return Ok(false),
                Ok(_) => return Ok(true),
                // A signal arriving mid-wait is not an event; go back to waiting.
                Err(nix::errno::Errno::EINTR) => continue,
                Err(e) => {
                    return Err(Error::io(
                        format!("/proc/pressure/memory (poll {})", self.spec),
                        io::Error::from(e),
                    ));
                }
            }
        }
    }

    pub fn as_raw_fd(&self) -> i32 {
        self.file.as_raw_fd()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn p() -> PathBuf {
        PathBuf::from("/proc/pressure/memory")
    }

    /// Verbatim from the machine this was written for.
    const QUIET: &str = "\
some avg10=0.00 avg60=0.00 avg300=0.01 total=3073683
full avg10=0.00 avg60=0.00 avg300=0.01 total=3065846
";

    const STALLING: &str = "\
some avg10=27.43 avg60=12.10 avg300=3.55 total=99001234
full avg10=11.02 avg60=4.87 avg300=1.20 total=44001234
";

    #[test]
    fn parses_a_quiet_machine() {
        let psi = parse(QUIET, &p()).unwrap();
        assert_eq!(psi.some.avg10, 0.0);
        assert_eq!(psi.some.avg300, 0.01);
        assert_eq!(psi.some.total_us, 3_073_683);
        assert_eq!(psi.full.total_us, 3_065_846);
        assert!(psi.is_quiet());
    }

    #[test]
    fn parses_a_stalling_machine() {
        let psi = parse(STALLING, &p()).unwrap();
        assert_eq!(psi.some.avg10, 27.43);
        assert_eq!(psi.full.avg10, 11.02);
        assert!(!psi.is_quiet());
    }

    #[test]
    fn tolerates_a_missing_full_line() {
        // cpu.pressure has no `full` on older kernels; parsing must not reject it.
        let psi = parse("some avg10=1.00 avg60=0.50 avg300=0.10 total=42\n", &p()).unwrap();
        assert_eq!(psi.some.avg10, 1.0);
        assert_eq!(psi.full, Window::default());
    }

    #[test]
    fn a_file_with_no_some_line_is_an_error_not_a_quiet_reading() {
        let err = parse("full avg10=1.0 total=1\n", &p()).unwrap_err();
        assert!(matches!(err, Error::Parse { .. }), "{err:?}");
    }

    #[test]
    fn a_truncated_read_is_an_error() {
        assert!(parse("", &p()).is_err());
        assert!(parse("some avg10=notafloat total=1\n", &p()).is_err());
    }

    #[test]
    fn unknown_future_fields_are_ignored() {
        let psi = parse(
            "some avg10=2.00 avg60=1.00 avg300=0.50 avg900=0.10 total=7 newfield=x\n",
            &p(),
        )
        .unwrap();
        assert_eq!(psi.some.avg10, 2.0);
        assert_eq!(psi.some.total_us, 7);
    }

    #[test]
    fn reads_through_a_synthetic_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = Root::at(dir.path());
        std::fs::create_dir_all(root.join("proc/pressure")).unwrap();
        std::fs::write(root.join("proc/pressure/memory"), QUIET).unwrap();
        assert!(system_memory(&root).unwrap().is_quiet());
    }

    /// On a kernel that restricts trigger creation, this must come back as
    /// `Unsupported` so the caller falls back to polling rather than treating it
    /// as a failure worth stopping for.
    #[test]
    fn an_unavailable_trigger_is_reported_as_unsupported_not_as_an_io_error() {
        let root = Root::system();
        match Trigger::memory(&root, Duration::from_millis(50), Duration::from_secs(1)) {
            // Permitted here: fine, the real thing works.
            Ok(_) => {}
            Err(Error::Unsupported(msg)) => {
                assert!(msg.contains("PSI trigger"), "{msg}");
            }
            Err(Error::Denied { .. }) => {}
            // Anything else means we are misreporting a capability limit.
            Err(e) => panic!("expected Unsupported or Denied, got {e:?}"),
        }
    }

    #[test]
    fn trigger_rejects_a_window_the_kernel_would_refuse() {
        let root = Root::at("/nonexistent-root");
        let too_long = Trigger::memory(&root, Duration::from_secs(1), Duration::from_secs(60));
        assert!(matches!(too_long, Err(Error::Parse { .. })));

        let too_short = Trigger::memory(&root, Duration::from_millis(1), Duration::from_millis(100));
        assert!(matches!(too_short, Err(Error::Parse { .. })));
    }

    #[test]
    fn trigger_rejects_a_stall_larger_than_its_window() {
        let root = Root::at("/nonexistent-root");
        let bad = Trigger::memory(&root, Duration::from_secs(5), Duration::from_secs(1));
        assert!(matches!(bad, Err(Error::Parse { .. })), "{bad:?}");
    }
}
