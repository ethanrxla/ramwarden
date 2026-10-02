//! cgroup v2: exact per-application accounting, and the reclaim lever.
//!
//! # Why this is the heart of v2
//!
//! Every desktop application on a modern systemd session lives in its own scope
//! under `user@<uid>.service`. That gives RamWarden two things v1 could not get:
//!
//! **Exact accounting.** `memory.current` is the kernel's own charge for the
//! scope. It does not double-count shared pages, so it needs none of v1's
//! guesswork about which process names belong to the same application. On the
//! machine this was written for, v1's name-bucketed RSS sum reported Brave at
//! 15,094 MB; the two Brave scopes charge 6,347 MB, and summed PSS agrees.
//!
//! **A non-destructive lever.** Writing to `memory.reclaim` asks the kernel to
//! page a scope's cold memory out to zram. The application keeps running and
//! never learns it happened. `SIGSTOP` — v1's strongest action — freezes a GUI
//! app so thoroughly that users force-quit it believing it crashed; there are
//! comments throughout v1 apologising for exactly that. Reclaim is the action
//! that lever should always have been.
//!
//! Both are writable by the session user with **no privileges at all**:
//!
//! ```text
//! app-flatpak-com.brave.Browser-1681016714.scope/
//!     memory.reclaim   --w-------  ethanrisden
//!     memory.high      -rw-r--r--  ethanrisden
//! ```
//!
//! # The safety invariant
//!
//! A [`Scope`] can only be obtained from a [`Hierarchy`], and a `Hierarchy` is
//! rooted at one user's `user@<uid>.service` subtree. Docker, chimera, and every
//! system service live under `system.slice`, outside that subtree, so they are
//! unreachable by construction rather than by a check somebody might forget.

use std::fs;
use std::path::{Path, PathBuf};

use crate::psi::{self, Psi};
use crate::{Error, Result, Root, smaps};

/// The subset of `memory.stat` the ladder reasons about.
///
/// `inactive_*` is the kernel's own verdict on which pages are cold — strictly
/// better than inferring it from referenced bits, because it is the same figure
/// the kernel's reclaim path will act on. Sizing a reclaim request against
/// `inactive_anon + inactive_file` asks for memory that can actually be found.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stat {
    pub anon: u64,
    pub file: u64,
    pub inactive_anon: u64,
    pub active_anon: u64,
    pub inactive_file: u64,
    pub active_file: u64,
    pub unevictable: u64,
}

impl Stat {
    /// What the kernel is likely to hand back if asked to reclaim.
    pub fn reclaimable(&self) -> u64 {
        self.inactive_anon + self.inactive_file
    }
}

fn parse_stat(text: &str, path: &Path) -> Result<Stat> {
    let mut out = Stat::default();
    let mut seen = false;
    for line in text.lines() {
        let mut it = line.split_whitespace();
        let (Some(key), Some(value)) = (it.next(), it.next()) else {
            continue;
        };
        let slot = match key {
            "anon" => &mut out.anon,
            "file" => &mut out.file,
            "inactive_anon" => &mut out.inactive_anon,
            "active_anon" => &mut out.active_anon,
            "inactive_file" => &mut out.inactive_file,
            "active_file" => &mut out.active_file,
            "unevictable" => &mut out.unevictable,
            _ => continue,
        };
        *slot = value
            .parse()
            .map_err(|_| Error::parse(path, format!("{key} {value:?} is not an integer")))?;
        seen = true;
    }
    if !seen {
        return Err(Error::parse(path, "no recognised memory.stat keys"));
    }
    Ok(out)
}

/// Read a cgroup file holding a single integer, where `max` means unlimited.
fn read_limit(path: &Path) -> Result<Option<u64>> {
    let text = fs::read_to_string(path).map_err(|e| Error::io(path, e))?;
    let text = text.trim();
    if text == "max" {
        return Ok(None);
    }
    text.parse()
        .map(Some)
        .map_err(|_| Error::parse(path, format!("{text:?} is neither an integer nor `max`")))
}

fn read_u64(path: &Path) -> Result<u64> {
    let text = fs::read_to_string(path).map_err(|e| Error::io(path, e))?;
    let text = text.trim();
    text.parse()
        .map_err(|_| Error::parse(path, format!("{text:?} is not an integer")))
}

/// How much charge a reclaim actually dropped.
///
/// Saturating, and deliberately so: an application can allocate faster than the
/// kernel reclaims, leaving `after` above `before`. That is a real outcome —
/// "reclaim achieved nothing" — and must report zero rather than underflow.
pub(crate) fn measured_delta(before: u64, after: u64) -> u64 {
    before.saturating_sub(after)
}

/// One application's cgroup.
///
/// Obtainable only through a [`Hierarchy`], which is what guarantees it lies
/// inside the calling user's own subtree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scope {
    name: String,
    path: PathBuf,
}

impl Scope {
    fn new(name: String, path: PathBuf) -> Self {
        Scope { name, path }
    }

    /// The cgroup directory name, e.g. `app-flatpak-com.brave.Browser-1053156663.scope`.
    /// Useful as a stable key, but a poor label — see [`Scope::label`].
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The kernel's exact memory charge for this scope, in bytes.
    pub fn current(&self) -> Result<u64> {
        read_u64(&self.path.join("memory.current"))
    }

    /// High-water mark since the scope was created.
    pub fn peak(&self) -> Result<u64> {
        read_u64(&self.path.join("memory.peak"))
    }

    pub fn stat(&self) -> Result<Stat> {
        let path = self.path.join("memory.stat");
        let text = fs::read_to_string(&path).map_err(|e| Error::io(&path, e))?;
        parse_stat(&text, &path)
    }

    /// Per-application memory pressure. Lets the ladder tell an app that is
    /// stalling from one that merely happens to be large.
    pub fn pressure(&self) -> Result<Psi> {
        psi::read(self.path.join("memory.pressure"))
    }

    pub fn pids(&self) -> Result<Vec<i32>> {
        let path = self.path.join("cgroup.procs");
        let text = fs::read_to_string(&path).map_err(|e| Error::io(&path, e))?;
        Ok(text
            .lines()
            .filter_map(|l| l.trim().parse::<i32>().ok())
            .collect())
    }

    /// The current `memory.high` soft cap, or `None` when unlimited.
    pub fn high(&self) -> Result<Option<u64>> {
        read_limit(&self.path.join("memory.high"))
    }

    /// Ask the kernel to reclaim `bytes` of this scope's memory.
    ///
    /// Returns the charge actually dropped, measured rather than requested: the
    /// kernel reclaims what it can find and reports `EAGAIN` when it falls
    /// short, and v1's habit of reporting intentions as outcomes is a large part
    /// of why this rewrite exists. A short reclaim is a normal result, not an
    /// error — the caller escalates instead.
    pub fn reclaim(&self, bytes: u64) -> Result<u64> {
        let path = self.path.join("memory.reclaim");
        let before = self.current()?;

        match fs::write(&path, bytes.to_string()) {
            Ok(()) => {}
            // The kernel could not find the full amount. It still reclaimed
            // whatever it did find, so fall through to measuring.
            Err(e) if e.raw_os_error() == Some(nix::errno::Errno::EAGAIN as i32) => {
                tracing::debug!(scope = %self.name, requested = bytes, "reclaim came up short");
            }
            Err(e) => return Err(Error::io(&path, e)),
        }

        let after = self.current()?;
        Ok(measured_delta(before, after))
    }

    /// Set a soft cap. Above it the kernel throttles the application and
    /// reclaims from it continuously, rather than killing anything — unlike
    /// `memory.max`, which triggers the OOM killer. RamWarden never writes
    /// `memory.max` for that reason.
    pub fn set_high(&self, bytes: u64) -> Result<()> {
        let path = self.path.join("memory.high");
        fs::write(&path, bytes.to_string()).map_err(|e| Error::io(&path, e))
    }

    /// Lift the soft cap. Called on de-escalation; leaving a cap in place after
    /// pressure has passed would throttle an application indefinitely for a
    /// reason that no longer exists.
    pub fn clear_high(&self) -> Result<()> {
        let path = self.path.join("memory.high");
        fs::write(&path, "max").map_err(|e| Error::io(&path, e))
    }

    /// A human label for this scope, taken from the process inside it holding
    /// the most memory.
    ///
    /// The directory name cannot be trusted for this. On the machine this was
    /// written for, `app-cosmic-com.system76.CosmicAppList-88887.scope` holds
    /// 3,032 MB that is entirely **ChatGPT** — the app inherited the scope of
    /// the launcher that started it. Labelling by directory name would have put
    /// "CosmicAppList" in front of the user and invited them to reclaim their
    /// desktop shell.
    pub fn label(&self, root: &Root) -> Result<String> {
        let pids = self.pids()?;
        let mut best: Option<(u64, String)> = None;

        for pid in pids {
            let pss = match smaps::rollup(root, pid) {
                Ok(r) => r.pss,
                // Routine: the process exited between listing and reading.
                Err(e) if e.is_missing() => continue,
                Err(Error::Denied { .. }) => continue,
                Err(e) => return Err(e),
            };
            let comm_path = root.proc_pid(pid, "comm");
            let Ok(comm) = fs::read_to_string(&comm_path) else {
                continue;
            };
            let comm = comm.trim().to_string();
            if best.as_ref().is_none_or(|(b, _)| pss > *b) {
                best = Some((pss, comm));
            }
        }

        Ok(best.map(|(_, c)| c).unwrap_or_else(|| self.name.clone()))
    }
}

/// One user's delegated cgroup subtree — the whole of what RamWarden may touch.
#[derive(Clone, Debug)]
pub struct Hierarchy {
    base: PathBuf,
}

impl Hierarchy {
    /// The calling user's `user@<uid>.service` subtree.
    ///
    /// Everything reachable from here is the user's own session: their browsers,
    /// editors, chat apps. Everything *not* reachable — `system.slice`, and so
    /// every Docker container and system daemon — is outside RamWarden's reach
    /// permanently, which is a safety property worth having by construction.
    pub fn user_session(root: &Root, uid: u32) -> Result<Self> {
        let base = root
            .cgroup_fs()
            .join("user.slice")
            .join(format!("user-{uid}.slice"))
            .join(format!("user@{uid}.service"));

        if !base.is_dir() {
            return Err(Error::Io {
                path: base,
                source: std::io::Error::from(std::io::ErrorKind::NotFound),
            });
        }

        let h = Hierarchy { base };
        h.require_memory_controller()?;
        Ok(h)
    }

    /// For tests and for reading a captured snapshot.
    pub fn at(base: impl Into<PathBuf>) -> Self {
        Hierarchy { base: base.into() }
    }

    /// systemd delegates controllers explicitly. Without `memory` in the
    /// delegated set there is no `memory.reclaim` to write, and the ladder must
    /// fall back to signals — so surface it as unsupported at construction
    /// rather than as a confusing ENOENT on the first reclaim.
    fn require_memory_controller(&self) -> Result<()> {
        let path = self.base.join("cgroup.controllers");
        let text = fs::read_to_string(&path).map_err(|e| Error::io(&path, e))?;
        if !text.split_whitespace().any(|c| c == "memory") {
            return Err(Error::Unsupported(
                "the memory controller is not delegated to this user's cgroup subtree",
            ));
        }
        Ok(())
    }

    pub fn base(&self) -> &Path {
        &self.base
    }

    /// Whether a path lies inside this subtree. The reclaim and cap paths assert
    /// on this, so the invariant is checked as well as structural.
    pub fn contains(&self, path: &Path) -> bool {
        path.starts_with(&self.base)
    }

    /// Total charge for the whole session.
    pub fn current(&self) -> Result<u64> {
        read_u64(&self.base.join("memory.current"))
    }

    pub fn pressure(&self) -> Result<Psi> {
        psi::read(self.base.join("memory.pressure"))
    }

    /// Every cgroup in the subtree that actually holds processes.
    ///
    /// Parent slices (`app.slice`, `session.slice`) carry no processes of their
    /// own under systemd, so walking for non-empty `cgroup.procs` naturally
    /// yields the per-application leaves without hardcoding the slice layout —
    /// which differs between systemd versions and between desktops.
    pub fn scopes(&self) -> Result<Vec<Scope>> {
        let mut found = Vec::new();
        self.walk(&self.base, &mut found)?;
        found.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(found)
    }

    fn walk(&self, dir: &Path, out: &mut Vec<Scope>) -> Result<()> {
        let entries = match fs::read_dir(dir) {
            Ok(e) => e,
            // Scopes appear and vanish as applications start and stop; one
            // disappearing mid-walk is routine.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(Error::io(dir, e)),
        };

        let procs = dir.join("cgroup.procs");
        if let Ok(text) = fs::read_to_string(&procs)
            && text.split_whitespace().next().is_some()
            && dir != self.base
        {
            let name = dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            out.push(Scope::new(name, dir.to_path_buf()));
        }

        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                self.walk(&path, out)?;
            }
        }
        Ok(())
    }

    /// Look up one scope by directory name.
    pub fn scope(&self, name: &str) -> Result<Scope> {
        self.scopes()?
            .into_iter()
            .find(|s| s.name == name)
            .ok_or_else(|| Error::Io {
                path: self.base.join(name),
                source: std::io::Error::from(std::io::ErrorKind::NotFound),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a synthetic session mirroring the real layout measured on the
    /// target machine, including the two separate Brave scopes and the
    /// misleadingly-named scope that actually contains ChatGPT.
    fn fixture() -> (tempfile::TempDir, Root, Hierarchy) {
        let dir = tempfile::tempdir().unwrap();
        let root = Root::at(dir.path());
        let base = root
            .cgroup_fs()
            .join("user.slice/user-1000.slice/user@1000.service");

        fs::create_dir_all(&base).unwrap();
        fs::write(base.join("cgroup.controllers"), "cpu memory pids\n").unwrap();
        fs::write(base.join("cgroup.procs"), "").unwrap();
        fs::write(base.join("memory.current"), "8000000000\n").unwrap();
        fs::write(
            base.join("memory.pressure"),
            "some avg10=0.00 avg60=0.00 avg300=0.00 total=384795\n\
             full avg10=0.00 avg60=0.00 avg300=0.00 total=381712\n",
        )
        .unwrap();

        // app.slice is a parent: it has an (empty) cgroup.procs and must not be
        // returned as an application.
        let app = base.join("app.slice");
        fs::create_dir_all(&app).unwrap();
        fs::write(app.join("cgroup.procs"), "").unwrap();

        let mk = |scope_name: &str, current: u64, pids: &[(i32, &str, u64)]| {
            let d = app.join(scope_name);
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join("memory.current"), format!("{current}\n")).unwrap();
            fs::write(d.join("memory.peak"), format!("{}\n", current * 2)).unwrap();
            fs::write(d.join("memory.high"), "max\n").unwrap();
            fs::write(d.join("memory.reclaim"), "").unwrap();
            fs::write(
                d.join("memory.stat"),
                format!(
                    "anon {}\nfile {}\ninactive_anon {}\nactive_anon {}\ninactive_file {}\nactive_file {}\nunevictable 0\n",
                    current / 2, current / 4, current / 3, current / 6, current / 8, current / 8
                ),
            )
            .unwrap();
            fs::write(
                d.join("memory.pressure"),
                "some avg10=0.00 avg60=0.00 avg300=0.00 total=0\nfull avg10=0.00 avg60=0.00 avg300=0.00 total=0\n",
            )
            .unwrap();
            let procs: String = pids.iter().map(|(p, _, _)| format!("{p}\n")).collect();
            fs::write(d.join("cgroup.procs"), procs).unwrap();

            for (pid, comm, pss) in pids {
                let pd = root.join(format!("proc/{pid}"));
                fs::create_dir_all(&pd).unwrap();
                fs::write(pd.join("comm"), format!("{comm}\n")).unwrap();
                fs::write(
                    pd.join("smaps_rollup"),
                    format!(
                        "0-1 ---p 0 00:00 0 [rollup]\nRss: {} kB\nPss: {} kB\nReferenced: 0 kB\nLocked: 0 kB\n",
                        pss / 1024 * 2,
                        pss / 1024
                    ),
                )
                .unwrap();
            }
        };

        mk(
            "app-flatpak-com.brave.Browser-1681016714.scope",
            5_201_741_414,
            &[(6669, "brave", 4_000_000_000), (2070834, "brave", 900_000_000)],
        );
        mk(
            "app-flatpak-com.brave.Browser-1053156663.scope",
            1_453_326_336,
            &[(1680002, "brave", 1_300_000_000)],
        );
        // The real trap: named for the launcher, but it is ChatGPT inside.
        mk(
            "app-cosmic-com.system76.CosmicAppList-88887.scope",
            3_179_282_432,
            &[
                (88887, "cosmic-app-list", 60_000_000),
                (89406, "ChatGPT", 800_000_000),
                (89405, "ChatGPT", 780_000_000),
            ],
        );

        // Outside the hierarchy entirely — a Docker container. Must never appear.
        let sys = root.cgroup_fs().join("system.slice/docker-chimera-core.scope");
        fs::create_dir_all(&sys).unwrap();
        fs::write(sys.join("cgroup.procs"), "3026\n").unwrap();
        fs::write(sys.join("memory.current"), "43109120\n").unwrap();

        let h = Hierarchy::user_session(&root, 1000).unwrap();
        (dir, root, h)
    }

    #[test]
    fn finds_application_scopes_but_not_parent_slices() {
        let (_d, _root, h) = fixture();
        let names: Vec<_> = h.scopes().unwrap().iter().map(|s| s.name().to_string()).collect();
        assert_eq!(names.len(), 3, "{names:?}");
        assert!(!names.iter().any(|n| n == "app.slice"), "{names:?}");
        assert!(!names.iter().any(|n| n.contains("user@")), "{names:?}");
    }

    /// The safety invariant that keeps chimera and every other container out of
    /// reach. If this ever fails, RamWarden can touch system services.
    #[test]
    fn never_reaches_outside_the_user_session() {
        let (_d, root, h) = fixture();
        for s in h.scopes().unwrap() {
            assert!(h.contains(s.path()), "{:?} escaped the hierarchy", s.path());
            assert!(!s.path().to_string_lossy().contains("system.slice"));
        }
        // The Docker scope exists in the fixture and is still not reachable.
        assert!(root.cgroup_fs().join("system.slice/docker-chimera-core.scope").is_dir());
        assert!(h.scope("docker-chimera-core.scope").is_err());
    }

    #[test]
    fn accounting_matches_the_kernel_not_a_name_bucketed_rss_sum() {
        let (_d, _root, h) = fixture();
        let brave: u64 = h
            .scopes()
            .unwrap()
            .iter()
            .filter(|s| s.name().contains("brave"))
            .map(|s| s.current().unwrap())
            .sum();
        // ~6.35 GB across the two scopes, as measured, not v1's ~15 GB.
        assert_eq!(brave, 5_201_741_414 + 1_453_326_336);
        assert!(brave < 7_000_000_000, "{brave}");
    }

    /// Regression guard for the CosmicAppList trap.
    #[test]
    fn labels_a_scope_by_its_largest_process_not_its_directory_name() {
        let (_d, root, h) = fixture();
        let scope = h.scope("app-cosmic-com.system76.CosmicAppList-88887.scope").unwrap();
        assert_eq!(scope.label(&root).unwrap(), "ChatGPT");
        assert!(scope.name().contains("CosmicAppList"));
    }

    #[test]
    fn labels_fall_back_to_the_scope_name_when_no_process_is_readable() {
        let (_d, root, h) = fixture();
        let scope = h.scope("app-flatpak-com.brave.Browser-1053156663.scope").unwrap();
        // Remove the only process's proc entry, as if it exited mid-read.
        fs::remove_dir_all(root.join("proc/1680002")).unwrap();
        assert_eq!(
            scope.label(&root).unwrap(),
            "app-flatpak-com.brave.Browser-1053156663.scope"
        );
    }

    #[test]
    fn reads_memory_stat_and_reports_what_is_reclaimable() {
        let (_d, _root, h) = fixture();
        let s = h.scope("app-flatpak-com.brave.Browser-1053156663.scope").unwrap();
        let stat = s.stat().unwrap();
        let c = 1_453_326_336u64;
        assert_eq!(stat.inactive_anon, c / 3);
        assert_eq!(stat.inactive_file, c / 8);
        assert_eq!(stat.reclaimable(), c / 3 + c / 8);
    }

    #[test]
    fn reclaim_asks_the_kernel_for_exactly_the_requested_byte_count() {
        let (_d, _root, h) = fixture();
        let s = h.scope("app-flatpak-com.brave.Browser-1053156663.scope").unwrap();
        s.reclaim(500_000_000).unwrap();
        assert_eq!(
            fs::read_to_string(s.path().join("memory.reclaim")).unwrap(),
            "500000000",
            "the kernel takes a plain byte count"
        );
    }

    /// The reported figure is the measured drop in charge, never the amount
    /// requested. v1 routinely reported intentions as outcomes; this is the
    /// arithmetic that stops v2 doing the same.
    #[test]
    fn measured_delta_reports_the_drop_not_the_request() {
        assert_eq!(measured_delta(1_453_326_336, 1_253_326_336), 200_000_000);
    }

    #[test]
    fn measured_delta_saturates_when_the_app_outran_the_reclaim() {
        // The application allocated faster than the kernel could reclaim.
        assert_eq!(measured_delta(1_000_000_000, 1_400_000_000), 0);
    }

    #[test]
    fn reclaim_of_an_unbudging_scope_reports_zero_rather_than_failing() {
        let (_d, _root, h) = fixture();
        let s = h.scope("app-flatpak-com.brave.Browser-1681016714.scope").unwrap();
        assert_eq!(s.reclaim(1_000_000_000).unwrap(), 0);
    }

    #[test]
    fn soft_cap_round_trips_and_clears_to_max() {
        let (_d, _root, h) = fixture();
        let s = h.scope("app-flatpak-com.brave.Browser-1053156663.scope").unwrap();
        assert_eq!(s.high().unwrap(), None, "starts uncapped");
        s.set_high(1_000_000_000).unwrap();
        assert_eq!(s.high().unwrap(), Some(1_000_000_000));
        s.clear_high().unwrap();
        assert_eq!(s.high().unwrap(), None, "de-escalation must lift the cap");
    }

    #[test]
    fn a_session_without_the_memory_controller_is_unsupported() {
        let dir = tempfile::tempdir().unwrap();
        let root = Root::at(dir.path());
        let base = root.cgroup_fs().join("user.slice/user-1000.slice/user@1000.service");
        fs::create_dir_all(&base).unwrap();
        // systemd delegated cpu and pids, but not memory.
        fs::write(base.join("cgroup.controllers"), "cpu pids\n").unwrap();

        let err = Hierarchy::user_session(&root, 1000).unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)), "{err:?}");
    }

    #[test]
    fn a_missing_session_is_a_clear_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let root = Root::at(dir.path());
        let err = Hierarchy::user_session(&root, 1000).unwrap_err();
        assert!(err.is_missing(), "{err:?}");
    }

    #[test]
    fn session_pressure_parses() {
        let (_d, _root, h) = fixture();
        assert!(h.pressure().unwrap().is_quiet());
        assert_eq!(h.current().unwrap(), 8_000_000_000);
    }

    #[test]
    fn stat_with_no_known_keys_is_an_error() {
        assert!(parse_stat("something_else 5\n", Path::new("memory.stat")).is_err());
    }
}
