//! The process table as `ramwarden-platform` samples, read from `/proc`.
//!
//! # Cheap every tick, expensive only when it can have changed
//!
//! Every tick reads `/proc/<pid>/stat` for every process: it carries the CPU
//! time and resident size the detector needs fresh, and it is one short read.
//! Everything else is cached per process instance (PID plus `starttime`, see
//! [`ramwarden_platform::cache`]) and re-read only when it can have changed:
//!
//! | Fact | Re-read when |
//! |---|---|
//! | command line, owner | first two sightings, after an `exec` (comm changed), and every [`DETAIL_TTL`] |
//! | PSS (`smaps_rollup`) | resident size moved by more than 1/[`PSS_DRIFT`], or every [`PSS_TTL`] |
//! | sockets | every [`TABLES_EVERY`], or [`TABLES_SOONEST`] after a new process appears |
//! | cgroup scopes | as sockets |
//!
//! The command line is read on the second sighting too because Chromium's
//! zygote forks renderers and rewrites their `argv` just after the fork; a read
//! in between would keep the zygote's command line for a whole TTL.
//!
//! Measured with `cargo run --release -p ramwarden-core --example tick_cost`
//! over a synthetic 400-process `/proc` (60 browser-sized, 30 descriptors
//! each), detector included: a tick went from 15.4 ms, 81,000 allocations and
//! 2,524 read syscalls to 1.2 ms, 2,100 allocations and 802 reads (the two
//! reads of each `stat`). The socket table is most of the difference: tracing
//! owners through 12,000 descriptors costs 12.5 ms and 66,000 allocations
//! there, and now runs once per [`TABLES_EVERY`] instead of once per tick.
//! Real procfs reads cost more than synthetic ones, so the saving on a real
//! machine is larger.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ramwarden_platform as p;
use ramwarden_platform::{Cadence, Instances, Owner, ProcessSample};

use crate::cgroup::Hierarchy;
use crate::platform::current_uid;
use crate::{Root, net, process, smaps};

/// Resident size below which PSS is approximated rather than measured.
///
/// Reading `smaps_rollup` costs a syscall and a parse per process. For a 4 MB
/// helper the distinction between RSS and PSS cannot change any decision, so the
/// cheap figure from `stat` is used instead. v1 paid full price for every process.
const PSS_MEASURE_FLOOR: u64 = 24 * 1024 * 1024;

/// A cached PSS is re-measured once resident size has moved by more than
/// 1/16th (about 6%) of what it was when PSS was last read.
pub const PSS_DRIFT: u64 = 16;

/// A cached PSS is re-measured at least this often, because sharing changes
/// without resident size changing: when one of two renderers sharing pages
/// exits, the survivor's PSS rises while its RSS stays put.
pub const PSS_TTL: Duration = Duration::from_secs(20);

/// Command line and owner are re-read at least this often. Both are fixed for
/// the life of an executable image, so this only bounds the damage of a change
/// nothing else detects (a process rewriting its own `argv`, a `setuid`).
pub const DETAIL_TTL: Duration = Duration::from_secs(60);

/// The socket table and cgroup scopes are re-read at least this often.
///
/// Listening ports and scope membership change far less often than memory
/// pressure. The socket table is the most expensive read in a tick: finding
/// which process owns a socket means a `readlink` on every file descriptor of
/// every process.
pub const TABLES_EVERY: Duration = Duration::from_secs(10);

/// ...and no sooner than this after the last read, when a new process appeared.
/// A dev server just started is the case that matters; it cannot be an idle
/// reclaim candidate for its first minutes anyway (see `FRESH_MINUTES`).
pub const TABLES_SOONEST: Duration = Duration::from_secs(3);

/// What is kept between ticks for one process instance.
struct Known {
    comm: String,
    cmdline: Arc<str>,
    owner: Owner,
    details_at: Instant,
    /// How many times command line and owner have been read.
    detail_reads: u8,
    /// PSS as last measured, with the resident size and time it was measured
    /// at. `None` until it has been.
    pss: Option<(u64, u64, Instant)>,
}

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
    known: Instances<Known>,
    sockets: net::Sockets,
    scopes: HashMap<i32, String>,
    tables: Cadence,
}

impl LinuxProcesses {
    pub fn new(root: Root) -> Self {
        LinuxProcesses {
            root,
            uid: current_uid(),
            pss_unavailable: false,
            known: Instances::new(),
            sockets: net::Sockets::default(),
            scopes: HashMap::new(),
            tables: Cadence::new(TABLES_EVERY, TABLES_SOONEST),
        }
    }

    /// pid -> cgroup scope name, for every process in the user's session.
    ///
    /// Processes outside it (system services, every Docker container) get no
    /// scope, which is also a reminder that the ladder cannot reach them.
    fn read_scopes(&mut self) {
        self.scopes.clear();
        let Ok(h) = Hierarchy::user_session(&self.root, self.uid) else {
            return;
        };
        let Ok(scopes) = h.scopes() else { return };
        for s in scopes {
            if let Ok(pids) = s.pids() {
                for pid in pids {
                    self.scopes.insert(pid, s.name().to_string());
                }
            }
        }
    }

    fn owner(root: &Root, me: u32, pid: i32) -> Owner {
        match process::uid(root, pid) {
            Ok(0) => Owner::System,
            Ok(u) if u == me => Owner::CurrentUser,
            Ok(_) => Owner::OtherUser,
            Err(_) => Owner::Unknown,
        }
    }

    /// Refresh one instance's command line and owner if they may be stale.
    fn refresh_details(root: &Root, me: u32, pid: i32, comm: &str, k: &mut Known, now: Instant) {
        let execed = k.comm != comm;
        if !execed && k.detail_reads >= 2 && now.saturating_duration_since(k.details_at) < DETAIL_TTL {
            return;
        }
        let cmdline = process::cmdline(root, pid).unwrap_or_default();
        if *k.cmdline != *cmdline {
            k.cmdline = Arc::from(cmdline);
        }
        k.owner = Self::owner(root, me, pid);
        if execed {
            k.comm.clear();
            k.comm.push_str(comm);
            // A new image maps new memory; its old PSS says nothing.
            k.pss = None;
        }
        k.details_at = now;
        k.detail_reads = k.detail_reads.saturating_add(1);
    }

    /// PSS for a process large enough to matter, RSS otherwise. Returns the
    /// figure and whether `smaps_rollup` was actually read for it.
    fn proportional(
        root: &Root,
        unavailable: &mut bool,
        pid: i32,
        rss: u64,
        cached: &mut Option<(u64, u64, Instant)>,
        now: Instant,
    ) -> (u64, bool) {
        if rss < PSS_MEASURE_FLOOR {
            return (rss, false);
        }
        if let Some((pss, at_rss, at)) = *cached
            && rss.abs_diff(at_rss) <= at_rss / PSS_DRIFT
            && now.saturating_duration_since(at) < PSS_TTL
        {
            return (pss, false);
        }
        match smaps::rollup(root, pid) {
            Ok(r) => {
                *cached = Some((r.pss, rss, now));
                (r.pss, true)
            }
            Err(e) if e.is_missing() => (rss, false), // the process just exited
            Err(e) => {
                // Not a vanished process: we are being denied. Say so loudly
                // and once, because the fallback silently restores v1's
                // overcount.
                if !*unavailable {
                    *unavailable = true;
                    tracing::error!(
                        "cannot read smaps_rollup ({e}) — accounting has fallen back \
                         to summed RSS, which over-reports any process that shares \
                         pages. If this daemon runs under systemd, check that the unit \
                         sets no mount-namespace sandboxing (PrivateTmp, \
                         ProtectKernelTunables, ProtectSystem)."
                    );
                }
                *cached = None;
                (rss, false)
            }
        }
    }
}

impl p::ProcessSource for LinuxProcesses {
    fn sample(&mut self) -> p::Result<Vec<ProcessSample>> {
        let now = Instant::now();
        let now_epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let boot = process::boot_time(&self.root).unwrap_or(0);
        let table = process::table(&self.root)?;

        self.known.begin();
        let mut out = Vec::with_capacity(table.len());
        let mut measured_pss = false;
        let mut appeared = 0usize;
        // Two passes: identities first, so a new process can bring the socket
        // and scope tables forward before any row is built from them.
        let mut details: Vec<(Arc<str>, Owner, u64)> = Vec::with_capacity(table.len());
        for e in &table {
            let (root, me) = (&self.root, self.uid);
            let (k, new) = self.known.entry(e.pid, e.start_ticks, || Known {
                comm: e.comm.clone(),
                cmdline: Arc::from(""),
                owner: Owner::Unknown,
                details_at: now,
                detail_reads: 0,
                pss: None,
            });
            appeared += new as usize;
            Self::refresh_details(root, me, e.pid, &e.comm, k, now);
            let (pss, read) =
                Self::proportional(root, &mut self.pss_unavailable, e.pid, e.rss_bytes(), &mut k.pss, now);
            measured_pss |= read;
            let (cmdline, owner) = (Arc::clone(&k.cmdline), k.owner);
            details.push((cmdline, owner, pss));
        }
        self.known.sweep();

        if self.tables.due(now, appeared > 0) {
            self.tables.mark(now);
            self.sockets = net::sockets(&self.root).unwrap_or_default();
            self.read_scopes();
        }

        for (e, (cmdline, owner, pss)) in table.into_iter().zip(details) {
            let rss = e.rss_bytes();
            out.push(ProcessSample {
                pid: e.pid,
                ppid: e.ppid,
                started: e.start_ticks,
                cmdline,
                owner,
                cpu_seconds: e.cpu_seconds(),
                resident: rss,
                proportional: pss,
                state: e.state,
                has_tty: e.has_tty(),
                age_seconds: e.age_seconds(boot, now_epoch),
                listening_ports: self.sockets.ports_for(e.pid).to_vec(),
                established: self.sockets.established_for(e.pid),
                group: self.scopes.get(&e.pid).cloned(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use ramwarden_platform::ProcessSource;
    use std::fs;

    struct Proc(tempfile::TempDir);

    impl Proc {
        fn new() -> Self {
            let d = tempfile::tempdir().unwrap();
            fs::create_dir_all(d.path().join("proc")).unwrap();
            fs::write(d.path().join("proc/stat"), "btime 1700000000\n").unwrap();
            Proc(d)
        }

        fn root(&self) -> Root {
            Root::at(self.0.path())
        }

        /// One process. `rss_pages` large enough puts it over the PSS floor.
        fn put(&self, pid: i32, comm: &str, start: u64, rss_pages: u64, cmdline: &str, pss_kb: u64) {
            let d = self.0.path().join(format!("proc/{pid}"));
            fs::create_dir_all(&d).unwrap();
            fs::write(
                d.join("stat"),
                format!("{pid} ({comm}) S 1 {pid} {pid} 0 -1 0 0 0 0 0 5 5 0 0 20 0 1 0 {start} 1000 {rss_pages} 0\n"),
            )
            .unwrap();
            fs::write(d.join("cmdline"), cmdline.replace(' ', "\0")).unwrap();
            fs::write(d.join("status"), "Uid:\t1000\t1000\t1000\t1000\n").unwrap();
            fs::write(d.join("smaps_rollup"), format!("[rollup]\nRss: 1 kB\nPss: {pss_kb} kB\n")).unwrap();
        }

        fn cmdline(&self, pid: i32, cmdline: &str) {
            fs::write(self.0.path().join(format!("proc/{pid}/cmdline")), cmdline.replace(' ', "\0")).unwrap();
        }

        fn pss(&self, pid: i32, pss_kb: u64) {
            let path = self.0.path().join(format!("proc/{pid}/smaps_rollup"));
            fs::write(path, format!("[rollup]\nRss: 1 kB\nPss: {pss_kb} kB\n")).unwrap();
        }
    }

    fn one(src: &mut LinuxProcesses, pid: i32) -> ProcessSample {
        src.sample().unwrap().into_iter().find(|s| s.pid == pid).unwrap()
    }

    /// 100,000 pages is ~400 MB: over the PSS floor.
    const BIG: u64 = 100_000;

    #[test]
    fn a_command_line_is_cached_per_instance_and_reread_after_exec() {
        let p = Proc::new();
        p.put(10, "zygote", 500, 10, "chrome --type=zygote", 0);
        let mut src = LinuxProcesses::new(p.root());
        assert_eq!(&*one(&mut src, 10).cmdline, "chrome --type=zygote");

        // Second sighting re-reads: the zygote rewrote argv after forking.
        p.cmdline(10, "chrome --type=renderer");
        assert_eq!(&*one(&mut src, 10).cmdline, "chrome --type=renderer");

        // From then on it is cached: a change nothing signals is not seen...
        p.cmdline(10, "chrome --type=changed");
        let s = one(&mut src, 10);
        assert_eq!(&*s.cmdline, "chrome --type=renderer");
        assert_eq!(s.started, 500);

        // ...until an exec changes comm.
        p.put(10, "python3", 500, 10, "python3 server.py", 0);
        assert_eq!(&*one(&mut src, 10).cmdline, "python3 server.py");
    }

    /// The reason the cache is keyed by start time: a recycled PID must not
    /// inherit the dead process's command line, and with it its role.
    #[test]
    fn a_recycled_pid_is_read_afresh() {
        let p = Proc::new();
        p.put(20, "qemu-system-x86", 100, 10, "qemu-system-x86_64 -m 4G", 0);
        let mut src = LinuxProcesses::new(p.root());
        one(&mut src, 20);
        one(&mut src, 20);
        // Same pid and comm, later start: a different process.
        p.put(20, "qemu-system-x86", 900, 10, "qemu-img convert", 0);
        let s = one(&mut src, 20);
        assert_eq!(&*s.cmdline, "qemu-img convert");
        assert_eq!(s.started, 900);
    }

    #[test]
    fn pss_is_remeasured_only_when_resident_size_moves() {
        let p = Proc::new();
        p.put(30, "brave", 1, BIG, "brave", 300_000);
        let mut src = LinuxProcesses::new(p.root());
        assert_eq!(one(&mut src, 30).proportional, 300_000 * 1024);

        // Sharing changed but RSS did not: the cached figure stands (until PSS_TTL).
        p.pss(30, 250_000);
        assert_eq!(one(&mut src, 30).proportional, 300_000 * 1024);

        // RSS grew by more than 1/16: measured again.
        p.put(30, "brave", 1, BIG + BIG / 8, "brave", 340_000);
        assert_eq!(one(&mut src, 30).proportional, 340_000 * 1024);

        // Small processes are never measured: PSS is RSS.
        p.put(31, "helper", 1, 100, "helper", 999);
        let s = one(&mut src, 31);
        assert_eq!(s.proportional, s.resident);
        assert!(!src.accounting_degraded());
    }

    #[test]
    fn gone_processes_leave_the_cache() {
        let p = Proc::new();
        p.put(40, "a", 1, 10, "a", 0);
        p.put(41, "b", 1, 10, "b", 0);
        let mut src = LinuxProcesses::new(p.root());
        src.sample().unwrap();
        assert_eq!(src.known.len(), 2);
        fs::remove_dir_all(p.0.path().join("proc/41")).unwrap();
        src.sample().unwrap();
        assert_eq!(src.known.len(), 1);
    }
}
