//! The Linux implementation of `ramwarden-platform`.
//!
//! Thin adapters over the rest of this crate. No new mechanism lives here: each
//! type answers one platform trait by calling the module that already measures
//! or actuates the thing, so Linux behaviour is exactly what it was before the
//! seam existed.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use ramwarden_platform as p;
use ramwarden_platform::{Outcome, Owner, ProcessSample};

use crate::cgroup::{Hierarchy, Scope};
use crate::procfd::PidFd;
use crate::psi::{self, Psi, Trigger};
use crate::{Error, Root, helper, madvise, meminfo, net, process, smaps, zram};

/// Resident size below which PSS is approximated rather than measured.
///
/// Reading `smaps_rollup` costs a syscall and a parse per process. For a 4 MB
/// helper the distinction between RSS and PSS cannot change any decision, so the
/// cheap figure from `stat` is used instead. v1 paid full price for every process.
const PSS_MEASURE_FLOOR: u64 = 24 * 1024 * 1024;

/// How long a stall must last within the window to wake the monitor.
///
/// 50 ms per second is roughly "5% pressure sustained" — comfortably below the
/// first rung, so the ladder is awake and sampling before it needs to act.
pub const STALL_BUDGET: Duration = Duration::from_millis(50);
pub const STALL_WINDOW: Duration = Duration::from_secs(1);

/// Build the Linux platform over `root` (normally [`Root::system`]).
pub fn platform(root: Root) -> p::Platform {
    let uid = current_uid();
    let factory_root = root.clone();
    p::Platform::new(
        p::Parts {
            name: "linux",
            memory: Arc::new(LinuxMemory { root: root.clone() }),
            pressure: Arc::new(LinuxPressure { root: root.clone() }),
            control: Arc::new(LinuxControl { root: root.clone() }),
            pager: Arc::new(LinuxPager { root: root.clone() }),
            // MADV_COLD would be the analogue, but it needs the same capability
            // as page-out; cgroup reclaim already covers the ground.
            background: Arc::new(p::NoBackground),
            groups: Arc::new(CgroupGroups { root, uid }),
        },
        move || Box::new(LinuxProcesses::new(factory_root.clone())),
        || Box::new(crate::desktop::DesktopProbe::new()),
    )
}

pub fn current_uid() -> u32 {
    // SAFETY: getuid cannot fail and touches no memory.
    unsafe { libc::getuid() }
}

// ── Errors and pressure ─────────────────────────────────────────────────────

impl From<Error> for p::Error {
    fn from(e: Error) -> Self {
        if e.is_missing() {
            return p::Error::Missing(e.to_string());
        }
        match e {
            Error::Denied { path, hint } => p::Error::denied(path.display(), hint),
            Error::Unsupported(what) => p::Error::Unsupported(what),
            other => p::Error::os(other),
        }
    }
}

impl From<Psi> for p::Pressure {
    fn from(psi: Psi) -> Self {
        p::Pressure {
            some_avg10: psi.some.avg10,
            some_avg60: psi.some.avg60,
            some_avg300: psi.some.avg300,
            full_avg10: psi.full.avg10,
            full_avg60: psi.full.avg60,
            full_avg300: psi.full.avg300,
            estimated: false,
        }
    }
}

pub struct LinuxPressure {
    root: Root,
}

impl p::PressureSource for LinuxPressure {
    fn read(&self) -> p::Result<p::Pressure> {
        Ok(psi::system_memory(&self.root)?.into())
    }

    fn waker(&self) -> p::Result<Box<dyn p::PressureWaker>> {
        Ok(Box::new(Trigger::memory(&self.root, STALL_BUDGET, STALL_WINDOW)?))
    }
}

impl p::PressureWaker for Trigger {
    fn wait(&mut self, timeout: Duration) -> p::Result<bool> {
        Ok(Trigger::wait(self, timeout)?)
    }

    fn describe(&self) -> String {
        format!(
            "PSI trigger: waking on {}ms of stall per {}s",
            STALL_BUDGET.as_millis(),
            STALL_WINDOW.as_secs()
        )
    }
}

// ── Memory ──────────────────────────────────────────────────────────────────

pub struct LinuxMemory {
    root: Root,
}

impl LinuxMemory {
    pub fn new(root: Root) -> Self {
        LinuxMemory { root }
    }
}

impl p::MemorySource for LinuxMemory {
    fn status(&self) -> p::Result<p::MemoryStatus> {
        let m = meminfo::read(&self.root)?;
        Ok(p::MemoryStatus {
            total: m.total,
            free: m.free,
            available: m.available,
            cached: m.buffers + m.cached,
            swap_total: m.swap_total,
            swap_free: m.swap_free,
            commit: 0,
            commit_limit: 0,
        })
    }

    fn compression(&self) -> p::Result<p::Compression> {
        let z = zram::total(&self.root)?;
        Ok(p::Compression {
            original: z.orig_data_size,
            resident: z.mem_used_total,
        })
    }
}

// ── Processes ───────────────────────────────────────────────────────────────

/// Samples `/proc`, measuring PSS where it can change a decision.
pub struct LinuxProcesses {
    root: Root,
    uid: u32,
    /// Set when `smaps_rollup` could not be read for a process large enough to
    /// matter, so PSS fell back to RSS.
    ///
    /// This must be visible. A silent fallback is a silent return to v1's
    /// accounting — the 2.3x browser overcount the whole rewrite exists to fix —
    /// and the only symptom is every row reporting `pss == rss`, which looks
    /// plausible. It was shipped that way once, caused by systemd mount-namespace
    /// sandboxing on the service unit.
    pss_unavailable: bool,
}

impl LinuxProcesses {
    pub fn new(root: Root) -> Self {
        LinuxProcesses {
            root,
            uid: current_uid(),
            pss_unavailable: false,
        }
    }

    /// pid -> cgroup scope name, for every process in the user's session.
    ///
    /// Processes outside it (system services, every Docker container) get no
    /// scope, which is also a reminder that the ladder cannot reach them.
    fn scope_map(&self) -> HashMap<i32, String> {
        let mut out = HashMap::new();
        let Ok(h) = Hierarchy::user_session(&self.root, self.uid) else {
            return out;
        };
        let Ok(scopes) = h.scopes() else { return out };
        for s in scopes {
            if let Ok(pids) = s.pids() {
                for pid in pids {
                    out.insert(pid, s.name().to_string());
                }
            }
        }
        out
    }

    fn owner(&self, pid: i32) -> Owner {
        match process::uid(&self.root, pid) {
            Ok(0) => Owner::System,
            Ok(u) if u == self.uid => Owner::CurrentUser,
            Ok(_) => Owner::OtherUser,
            Err(_) => Owner::Unknown,
        }
    }

    /// PSS for a process large enough to matter, RSS otherwise.
    fn proportional(&mut self, pid: i32, rss: u64) -> u64 {
        if rss < PSS_MEASURE_FLOOR {
            return rss;
        }
        match smaps::rollup(&self.root, pid) {
            Ok(r) => r.pss,
            Err(e) if e.is_missing() => rss, // the process just exited
            Err(e) => {
                // Not a vanished process: we are being denied. Say so loudly
                // and once, because the fallback silently restores v1's
                // overcount.
                if !self.pss_unavailable {
                    self.pss_unavailable = true;
                    tracing::error!(
                        "cannot read smaps_rollup ({e}) — accounting has fallen back \
                         to summed RSS, which over-reports any process that shares \
                         pages. If this daemon runs under systemd, check that the unit \
                         sets no mount-namespace sandboxing (PrivateTmp, \
                         ProtectKernelTunables, ProtectSystem)."
                    );
                }
                rss
            }
        }
    }
}

impl p::ProcessSource for LinuxProcesses {
    fn sample(&mut self) -> p::Result<Vec<ProcessSample>> {
        let now_epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let boot = process::boot_time(&self.root).unwrap_or(0);
        let sockets = net::sockets(&self.root).unwrap_or_default();
        let scopes = self.scope_map();
        let table = process::table(&self.root)?;

        let mut out = Vec::with_capacity(table.len());
        let mut measured_pss = false;
        for e in table {
            let rss = e.rss_bytes();
            let pss = self.proportional(e.pid, rss);
            measured_pss |= pss != rss;
            out.push(ProcessSample {
                pid: e.pid,
                ppid: e.ppid,
                cmdline: process::cmdline(&self.root, e.pid).unwrap_or_default(),
                owner: self.owner(e.pid),
                cpu_seconds: e.cpu_seconds(),
                resident: rss,
                proportional: pss,
                state: e.state,
                has_tty: e.has_tty(),
                age_seconds: e.age_seconds(boot, now_epoch),
                listening_ports: sockets.ports_for(e.pid).to_vec(),
                established: sockets.established_for(e.pid),
                group: scopes.get(&e.pid).cloned(),
                name: e.comm,
            });
        }

        // Recoverable: if the unit is fixed and reloaded, a later tick reads
        // rollups again and the warning should be able to fire once more.
        if self.pss_unavailable && measured_pss {
            self.pss_unavailable = false;
            tracing::info!("smaps_rollup is readable again — PSS accounting restored");
        }
        Ok(out)
    }

    fn accounting_degraded(&self) -> bool {
        self.pss_unavailable
    }
}

// ── Process control ─────────────────────────────────────────────────────────

pub struct LinuxControl {
    root: Root,
}

impl p::ProcessControl for LinuxControl {
    fn open(&self, pid: i32) -> p::Result<Box<dyn p::ProcessHandle>> {
        Ok(Box::new(PidFd::open(pid)?))
    }

    fn proportional(&self, pid: i32) -> Option<u64> {
        smaps::rollup(&self.root, pid).ok().map(|r| r.pss)
    }
}

impl p::ProcessHandle for PidFd {
    fn pid(&self) -> i32 {
        PidFd::pid(self)
    }

    fn is_alive(&self) -> bool {
        PidFd::is_alive(self)
    }

    fn suspend(&self) -> p::Result<()> {
        Ok(PidFd::suspend(self)?)
    }

    fn resume(&self) -> p::Result<()> {
        Ok(PidFd::resume(self)?)
    }

    fn terminate(&self) -> p::Result<()> {
        Ok(PidFd::terminate(self)?)
    }

    fn kill(&self) -> p::Result<()> {
        Ok(PidFd::kill(self)?)
    }

    fn wait_exit(&self, timeout: Duration) -> p::Result<bool> {
        Ok(PidFd::wait_exit(self, timeout)?)
    }
}

// ── Page-out ────────────────────────────────────────────────────────────────

/// Targeted page-out of cold mappings: directly when the daemon holds
/// `CAP_SYS_NICE`, otherwise through the setcap helper.
pub struct LinuxPager {
    root: Root,
}

impl p::Pager for LinuxPager {
    fn available(&self) -> bool {
        madvise::available() || helper::present()
    }

    fn route(&self) -> &'static str {
        if madvise::available() {
            "direct (daemon holds CAP_SYS_NICE)"
        } else if helper::present() {
            "ramwarden-helper"
        } else {
            "unavailable — cgroup reclaim only"
        }
    }

    /// Finer than cgroup reclaim, which acts on a whole application: this can
    /// take a browser's cold heap while leaving the tab the user is reading
    /// fully resident. Without `CAP_SYS_NICE` or the helper every call is
    /// refused, and the caller falls back to the cgroup path.
    fn page_out(&self, pid: i32, referenced_ratio: f64, min_bytes: u64) -> Outcome {
        let vmas = match smaps::cold_vmas(&self.root, pid, referenced_ratio, min_bytes) {
            Ok(v) => v,
            Err(e) => return Outcome::refused(format!("pid {pid}: {e}")),
        };
        if vmas.is_empty() {
            return Outcome::refused(format!("pid {pid} has no cold regions worth paging out"));
        }

        // Directly first: that works without privilege when the target is this
        // process, and on an install where the daemon holds CAP_SYS_NICE.
        if madvise::available()
            && let Ok(fd) = PidFd::open(pid)
            && let Ok(bytes) = madvise::page_out(&fd, &vmas)
            && bytes > 0
        {
            return Outcome::freed(vec![pid], bytes);
        }

        // Otherwise the privileged helper, which exists precisely so the
        // daemon does not have to hold the capability.
        helper::page_out(pid, &vmas)
    }
}

// ── cgroup groups ───────────────────────────────────────────────────────────

/// The user's delegated cgroup subtree, resolved fresh on every call: scopes
/// appear and vanish as applications start and stop.
pub struct CgroupGroups {
    root: Root,
    uid: u32,
}

impl CgroupGroups {
    pub fn new(root: Root, uid: u32) -> Self {
        CgroupGroups { root, uid }
    }

    fn hierarchy(&self) -> p::Result<Hierarchy> {
        Ok(Hierarchy::user_session(&self.root, self.uid)?)
    }

    fn wrap(&self, scope: Scope) -> Box<dyn p::AppGroup> {
        Box::new(CgroupGroup {
            root: self.root.clone(),
            scope,
        })
    }
}

impl p::GroupSource for CgroupGroups {
    fn ready(&self) -> p::Result<()> {
        self.hierarchy().map(|_| ())
    }

    fn groups(&self) -> p::Result<Vec<Box<dyn p::AppGroup>>> {
        Ok(self
            .hierarchy()?
            .scopes()?
            .into_iter()
            .map(|s| self.wrap(s))
            .collect())
    }

    fn group(&self, name: &str) -> p::Result<Box<dyn p::AppGroup>> {
        Ok(self.wrap(self.hierarchy()?.scope(name)?))
    }
}

/// One cgroup scope, with the root needed to label it.
pub struct CgroupGroup {
    root: Root,
    scope: Scope,
}

impl p::AppGroup for CgroupGroup {
    fn name(&self) -> &str {
        self.scope.name()
    }

    fn label(&self) -> String {
        self.scope
            .label(&self.root)
            .unwrap_or_else(|_| self.scope.name().to_string())
    }

    fn pids(&self) -> p::Result<Vec<i32>> {
        Ok(self.scope.pids()?)
    }

    fn current(&self) -> p::Result<u64> {
        Ok(self.scope.current()?)
    }

    fn reclaimable(&self) -> p::Result<u64> {
        Ok(self.scope.stat()?.reclaimable())
    }

    fn reclaim(&self, bytes: u64) -> p::Result<u64> {
        Ok(self.scope.reclaim(bytes)?)
    }

    fn soft_cap(&self) -> p::Result<Option<u64>> {
        Ok(self.scope.high()?)
    }

    /// `memory.high` throttles and reclaims continuously above the cap; it
    /// never invokes the OOM killer, which is why RamWarden uses it and never
    /// writes `memory.max`.
    fn set_soft_cap(&self, bytes: u64) -> p::Result<()> {
        Ok(self.scope.set_high(bytes)?)
    }

    fn clear_soft_cap(&self) -> p::Result<()> {
        Ok(self.scope.clear_high()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ramwarden_platform::PressureSource;

    /// The trigger is optional. If this machine grants one, good; if not, the
    /// monitor must still be able to start — so a refusal has to be a
    /// capability error it can recognise, not an arbitrary failure.
    #[test]
    fn a_missing_trigger_is_a_capability_error() {
        let pressure = LinuxPressure { root: Root::system() };
        if let Err(e) = pressure.waker() {
            assert!(
                matches!(e, p::Error::Unsupported(_) | p::Error::Denied { .. }),
                "a trigger refusal must be a capability error, not {e:?}"
            );
        }
    }

    #[test]
    fn psi_converts_without_losing_a_window() {
        let mut psi = Psi::default();
        psi.some.avg10 = 3.5;
        psi.full.avg300 = 1.25;
        let p: p::Pressure = psi.into();
        assert_eq!(p.some_avg10, 3.5);
        assert_eq!(p.full_avg300, 1.25);
        assert!(!p.estimated, "Linux pressure is measured, not estimated");
    }

    #[test]
    fn a_missing_file_stays_missing_across_the_seam() {
        let e = Error::Io {
            path: "/proc/1/stat".into(),
            source: std::io::Error::from(std::io::ErrorKind::NotFound),
        };
        assert!(p::Error::from(e).is_missing());
    }
}
