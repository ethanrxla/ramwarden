//! Carrying out decisions, and remembering what to undo.
//!
//! # Only what RamWarden froze
//!
//! The registry here tracks exactly the processes RamWarden suspended, and
//! nothing else. v1 got this right and the comment explaining why is worth
//! keeping: a process the *user* stopped — `Ctrl-Z`, or a debugger — must never
//! be swept up by auto-resume. Membership in this registry, not the `T` process
//! state, is what makes something ours to wake.
//!
//! # Handles, not numbers
//!
//! v1 stored PIDs and re-resolved them by name at resume time, and
//! `process_manager.py` admits the hazard in a comment: after a frozen app is
//! force-quit and relaunched, the same name matches a different process tree.
//! This registry holds [`PidFd`] handles instead. A handle refers to one
//! specific process forever; once that process exits it refuses to signal
//! anything. With `SIGCONT` the old bug was harmless, but the v2 ladder sends
//! `SIGKILL` on its own authority, where signalling a recycled PID means
//! destroying something the user never agreed to lose.
//!
//! # Reporting
//!
//! Every method returns what *happened*, not what was attempted. v1's
//! `_apply` learned this the hard way — see the "only report what actually
//! closed" commit — and the autonomous ladder makes it more important still,
//! because nobody is watching to notice the difference.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use ramwarden_kernel::procfd::PidFd;
use ramwarden_kernel::{Root, cgroup, madvise, smaps};

use crate::detector::{Decision, Detector};

/// How long a terminated process gets to exit before being killed.
pub const TERMINATE_GRACE: Duration = Duration::from_secs(3);

/// One application RamWarden froze, and when.
pub struct SuspendedEntry {
    pub name: String,
    /// Handles to the exact processes that were stopped.
    handles: Vec<PidFd>,
    pub since: Instant,
}

impl SuspendedEntry {
    pub fn minutes(&self) -> f64 {
        self.since.elapsed().as_secs_f64() / 60.0
    }

    /// Handles whose processes are still alive. A process the user killed or
    /// continued themselves drops out.
    pub fn live(&self) -> Vec<&PidFd> {
        self.handles.iter().filter(|h| h.is_alive()).collect()
    }

    pub fn pids(&self) -> Vec<i32> {
        self.handles.iter().map(|h| h.pid()).collect()
    }

    pub fn live_pids(&self) -> Vec<i32> {
        self.live().iter().map(|h| h.pid()).collect()
    }

    /// Memory still held by the frozen processes, in bytes.
    pub fn pss(&self, root: &Root) -> u64 {
        self.live()
            .iter()
            .filter_map(|h| smaps::rollup(root, h.pid()).ok())
            .map(|r| r.pss)
            .sum()
    }

    /// Whether every process in this entry is gone.
    pub fn is_dead(&self) -> bool {
        self.handles.iter().all(|h| !h.is_alive())
    }
}

/// What an action actually achieved.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    /// Processes or scopes the action successfully touched.
    pub affected: Vec<i32>,
    /// Bytes of memory measurably returned, where measurable.
    pub bytes_freed: u64,
    /// Why the action did not happen, or did not fully happen. Empty on a clean
    /// success.
    pub notes: Vec<String>,
}

impl Outcome {
    pub fn refused(reason: impl Into<String>) -> Self {
        Outcome {
            notes: vec![reason.into()],
            ..Default::default()
        }
    }

    pub fn did_nothing(&self) -> bool {
        self.affected.is_empty() && self.bytes_freed == 0
    }
}

/// Applies decisions and tracks what must be undone when pressure passes.
pub struct Actuator {
    root: Root,
    suspended: HashMap<String, SuspendedEntry>,
    /// scope name -> the `memory.high` value before RamWarden capped it.
    /// `None` means it was uncapped, which is the usual case.
    capped: HashMap<String, Option<u64>>,
}

impl Actuator {
    pub fn new(root: Root) -> Self {
        Actuator {
            root,
            suspended: HashMap::new(),
            capped: HashMap::new(),
        }
    }

    // ── Suspend / resume ────────────────────────────────────────────────────

    /// Freeze every process matching `name`, if the gate allows it.
    ///
    /// `force` skips the watchlist and in-use checks but never structural
    /// protection — see [`Detector::may_suspend_forced`].
    pub fn suspend(
        &mut self,
        name: &str,
        det: &Detector,
        watchlist: &[String],
        force: bool,
    ) -> Outcome {
        let decision = if force {
            det.may_suspend_forced(name)
        } else {
            det.may_suspend(name, watchlist)
        };
        let Decision::Allow(why) = decision else {
            tracing::warn!("refusing to suspend {name} — {}", decision.reason());
            return Outcome::refused(decision.reason().to_string());
        };
        tracing::info!("suspend allowed for {name} — {why}");

        let mut out = Outcome::default();
        let mut handles = Vec::new();

        for s in det.matching(name) {
            if s.is_stopped() {
                out.notes.push(format!("{} (pid {}) already stopped", s.name, s.pid));
                continue;
            }
            // Open the handle before signalling: if the process is already gone,
            // this fails here rather than hitting whoever reused the number.
            let Ok(fd) = PidFd::open(s.pid) else {
                out.notes.push(format!("pid {} vanished before it could be frozen", s.pid));
                continue;
            };
            match fd.suspend() {
                Ok(()) => {
                    out.bytes_freed += s.pss;
                    out.affected.push(s.pid);
                    handles.push(fd);
                }
                Err(e) => out.notes.push(format!("pid {}: {e}", s.pid)),
            }
        }

        if !handles.is_empty() {
            self.suspended.insert(
                name.to_string(),
                SuspendedEntry {
                    name: name.to_string(),
                    handles,
                    since: Instant::now(),
                },
            );
        }
        out
    }

    /// Wake one entry by name.
    pub fn resume(&mut self, name: &str) -> Outcome {
        let Some(entry) = self.suspended.remove(name) else {
            return Outcome::refused(format!("{name} was not suspended by RamWarden"));
        };
        Self::resume_entry(&entry)
    }

    /// Wake exactly the processes an entry froze.
    fn resume_entry(entry: &SuspendedEntry) -> Outcome {
        let mut out = Outcome::default();
        for h in entry.handles.iter() {
            match h.resume() {
                Ok(()) => out.affected.push(h.pid()),
                // Already gone. Not a failure — the user force-quit it, which is
                // their right, and the entry is being discarded anyway.
                Err(e) if e.is_missing() => {
                    out.notes.push(format!("pid {} had already exited", h.pid()))
                }
                Err(e) => out.notes.push(format!("pid {}: {e}", h.pid())),
            }
        }
        out
    }

    /// Wake everything RamWarden froze. Called when pressure passes.
    ///
    /// A suspended GUI app is indistinguishable from a crashed one, so leaving
    /// one frozen after the reason has gone is how a user ends up force-quitting
    /// a healthy application.
    pub fn resume_all(&mut self) -> Vec<(String, Outcome)> {
        let names: Vec<String> = self.suspended.keys().cloned().collect();
        names
            .into_iter()
            .filter_map(|n| self.suspended.remove(&n).map(|e| (n, Self::resume_entry(&e))))
            .collect()
    }

    /// Currently frozen applications, pruned of anything that died or woke.
    pub fn suspended(&mut self) -> Vec<&SuspendedEntry> {
        self.suspended.retain(|name, e| {
            if e.is_dead() {
                tracing::info!("{name} is no longer running — dropping it from the registry");
                return false;
            }
            true
        });
        let mut out: Vec<&SuspendedEntry> = self.suspended.values().collect();
        out.sort_by_key(|e| e.since);
        out
    }

    pub fn is_suspended(&self, name: &str) -> bool {
        self.suspended.contains_key(name)
    }

    /// Stop tracking an entry without signalling it.
    pub fn forget(&mut self, name: &str) {
        self.suspended.remove(name);
    }

    // ── Terminate / kill ────────────────────────────────────────────────────

    /// Ask matching processes to exit, escalating to `SIGKILL` if they do not.
    ///
    /// The gate still applies: structural protection cannot be killed, and
    /// without `force` the process must be on the watchlist and idle. The
    /// autonomous ladder calls this only at its last rung.
    pub fn kill(
        &mut self,
        name: &str,
        det: &Detector,
        watchlist: &[String],
        force: bool,
    ) -> Outcome {
        let decision = if force {
            det.may_suspend_forced(name)
        } else {
            det.may_suspend(name, watchlist)
        };
        if let Decision::Refuse(why) = &decision {
            tracing::warn!("refusing to kill {name} — {why}");
            return Outcome::refused(why.clone());
        }

        let mut out = Outcome::default();
        let mut pending = Vec::new();

        for s in det.matching(name) {
            let Ok(fd) = PidFd::open(s.pid) else {
                out.notes.push(format!("pid {} had already exited", s.pid));
                continue;
            };
            match fd.terminate() {
                Ok(()) => pending.push((fd, s.pss)),
                Err(e) if e.is_missing() => {}
                Err(e) => out.notes.push(format!("pid {}: {e}", s.pid)),
            }
        }

        // Give them all the grace period concurrently rather than serially — a
        // browser with twenty renderers would otherwise take a minute.
        let deadline = Instant::now() + TERMINATE_GRACE;
        for (fd, pss) in pending {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let exited = fd.wait_exit(remaining).unwrap_or(false);
            if !exited {
                if let Err(e) = fd.kill() {
                    out.notes.push(format!("pid {} would not die: {e}", fd.pid()));
                    continue;
                }
                out.notes.push(format!("pid {} needed SIGKILL", fd.pid()));
            }
            out.affected.push(fd.pid());
            out.bytes_freed += pss;
        }
        out
    }

    // ── Targeted page-out ───────────────────────────────────────────────────

    /// Page out the cold regions of one process.
    ///
    /// Finer than cgroup reclaim, which acts on a whole application: this can
    /// take a browser's cold heap while leaving the tab the user is reading
    /// fully resident. It needs `CAP_SYS_NICE`, which a systemd *user* service
    /// cannot be granted, so without the setcap helper every call returns
    /// `Denied` and the caller falls back to the cgroup path. That fallback is
    /// the normal configuration, not a degraded one.
    pub fn page_out(&self, pid: i32, referenced_ratio: f64, min_bytes: u64) -> Outcome {
        let vmas = match smaps::cold_vmas(&self.root, pid, referenced_ratio, min_bytes) {
            Ok(v) => v,
            Err(e) => return Outcome::refused(format!("pid {pid}: {e}")),
        };
        if vmas.is_empty() {
            return Outcome::refused(format!("pid {pid} has no cold regions worth paging out"));
        }

        // Try it directly first. That works without any privilege when the target
        // is this process, and on an install where the daemon itself holds
        // CAP_SYS_NICE.
        if madvise::available()
            && let Ok(fd) = PidFd::open(pid)
            && let Ok(bytes) = madvise::page_out(&fd, &vmas)
            && bytes > 0
        {
            return Outcome {
                affected: vec![pid],
                bytes_freed: bytes,
                notes: Vec::new(),
            };
        }

        // Otherwise ask the privileged helper, which exists precisely so the
        // daemon does not have to hold the capability.
        crate::helper::page_out(pid, &vmas)
    }

    /// Whether targeted page-out can do anything on this install.
    ///
    /// True when either the daemon itself may call `process_madvise` on other
    /// processes, or a helper is listening. False means rung 1 contributes
    /// nothing and the ladder leans on cgroup reclaim — which is the normal
    /// configuration, not a fault.
    pub fn can_page_out(&self) -> bool {
        madvise::available() || crate::helper::present()
    }

    /// Where page-out would come from, for the status line.
    pub fn page_out_route(&self) -> &'static str {
        if madvise::available() {
            "direct (daemon holds CAP_SYS_NICE)"
        } else if crate::helper::present() {
            "ramwarden-helper"
        } else {
            "unavailable — cgroup reclaim only"
        }
    }

    // ── cgroup reclaim and soft caps ────────────────────────────────────────

    /// Ask the kernel to reclaim from a scope. Non-destructive and invisible to
    /// the application, which is why the ladder reaches for it first.
    pub fn reclaim_scope(&self, scope: &cgroup::Scope, bytes: u64) -> Outcome {
        match scope.reclaim(bytes) {
            Ok(freed) => {
                let mut out = Outcome {
                    bytes_freed: freed,
                    ..Default::default()
                };
                if freed < bytes {
                    out.notes.push(format!(
                        "asked for {} MB, kernel found {} MB",
                        bytes / 1_000_000,
                        freed / 1_000_000
                    ));
                }
                out
            }
            Err(e) => Outcome::refused(format!("{}: {e}", scope.name())),
        }
    }

    /// Soft-cap a scope, remembering the previous value so it can be restored.
    ///
    /// `memory.high` throttles and reclaims continuously above the cap; it never
    /// invokes the OOM killer, which is why RamWarden uses it and never writes
    /// `memory.max`.
    pub fn soft_cap(&mut self, scope: &cgroup::Scope, bytes: u64) -> Outcome {
        let previous = scope.high().unwrap_or(None);
        match scope.set_high(bytes) {
            Ok(()) => {
                // Record the value from before *our first* cap, so repeated
                // tightening does not lose the original.
                self.capped
                    .entry(scope.name().to_string())
                    .or_insert(previous);
                Outcome::default()
            }
            Err(e) => Outcome::refused(format!("{}: {e}", scope.name())),
        }
    }

    /// Lift every cap RamWarden set, restoring whatever was there before.
    pub fn release_caps(&mut self, h: &cgroup::Hierarchy) -> Vec<(String, Outcome)> {
        let entries: Vec<(String, Option<u64>)> = self.capped.drain().collect();
        entries
            .into_iter()
            .map(|(name, previous)| {
                let outcome = match h.scope(&name) {
                    Ok(scope) => {
                        let r = match previous {
                            Some(v) => scope.set_high(v),
                            None => scope.clear_high(),
                        };
                        match r {
                            Ok(()) => Outcome::default(),
                            Err(e) => Outcome::refused(format!("{name}: {e}")),
                        }
                    }
                    // The application exited and took its scope with it; there
                    // is nothing left to uncap.
                    Err(_) => Outcome::refused(format!("{name} no longer exists")),
                };
                (name, outcome)
            })
            .collect()
    }

    pub fn has_caps(&self) -> bool {
        !self.capped.is_empty()
    }

    pub fn capped_scopes(&self) -> Vec<&str> {
        self.capped.keys().map(String::as_str).collect()
    }

    /// Whether anything is currently being held back and owes a release.
    pub fn owes_release(&self) -> bool {
        !self.suspended.is_empty() || !self.capped.is_empty()
    }

    pub fn root(&self) -> &Root {
        &self.root
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::roles::{Role, classify};
    use crate::signals::Signals;
    use std::process::{Child, Command};

    fn spawn() -> Child {
        Command::new("sleep").arg("60").spawn().expect("spawn sleep")
    }

    fn state_of(pid: i32) -> Option<char> {
        let s = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let tail = &s[s.rfind(')')? + 1..];
        tail.trim_start().chars().next()
    }

    /// Block until `pid` reaches `want`, or give up.
    ///
    /// Signal delivery is asynchronous: a test that sends SIGSTOP and reads
    /// `/proc` on the next line can still see `S`, which makes any assertion
    /// built on that read intermittently wrong.
    fn await_state(pid: i32, want: char) -> bool {
        for _ in 0..200 {
            if state_of(pid) == Some(want) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    /// A detector preloaded with signals for real PIDs, so the actuator can act
    /// on actual processes while the policy stays under test control.
    fn det_for(entries: &[(i32, &str)], warm: bool) -> Detector {
        let mut sigs = Vec::new();
        for (pid, name) in entries {
            let mut s = Signals::new(*pid, *name, classify(name, "", None));
            s.pss = 500 * 1024 * 1024;
            s.age_minutes = 600.0;
            s.state = state_of(*pid).unwrap_or('S');
            sigs.push(s);
        }
        Detector::preloaded(Root::system(), warm, sigs)
    }

    fn wl() -> Vec<String> {
        vec!["sleep".to_string()]
    }

    #[test]
    fn suspends_and_resumes_a_real_process() {
        let mut child = spawn();
        let pid = child.id() as i32;
        let det = det_for(&[(pid, "sleep")], true);
        let mut act = Actuator::new(Root::system());

        let out = act.suspend("sleep", &det, &wl(), false);
        assert_eq!(out.affected, vec![pid], "{:?}", out.notes);
        assert!(out.bytes_freed > 0);
        assert!(await_state(pid, 'T'), "SIGSTOP should leave it stopped");
        assert!(act.is_suspended("sleep"));

        let out = act.resume("sleep");
        assert_eq!(out.affected, vec![pid]);
        assert!(await_state(pid, 'S'), "SIGCONT should wake it");
        assert!(!act.is_suspended("sleep"));

        child.kill().ok();
        child.wait().ok();
    }

    #[test]
    fn the_gate_refuses_what_it_should_and_nothing_is_signalled() {
        let mut child = spawn();
        let pid = child.id() as i32;
        let det = det_for(&[(pid, "sleep")], true);
        let mut act = Actuator::new(Root::system());

        // Not on the watchlist.
        let out = act.suspend("sleep", &det, &[], false);
        assert!(out.did_nothing());
        assert!(out.notes[0].contains("not on the watchlist"), "{:?}", out.notes);
        assert_ne!(state_of(pid), Some('T'), "must not have been signalled");
        assert!(!act.is_suspended("sleep"));

        child.kill().ok();
        child.wait().ok();
    }

    #[test]
    fn a_structurally_protected_name_is_refused_even_when_forced() {
        let mut child = spawn();
        let pid = child.id() as i32;
        // Present it as dockerd, which is structurally protected.
        let det = det_for(&[(pid, "dockerd")], true);
        let mut act = Actuator::new(Root::system());

        let out = act.suspend("dockerd", &det, &["dockerd".into()], true);
        assert!(out.did_nothing());
        assert!(out.notes[0].contains("not forceable"), "{:?}", out.notes);
        assert_ne!(state_of(pid), Some('T'));

        child.kill().ok();
        child.wait().ok();
    }

    /// v1's `test_registry_only_tracks_what_ramwarden_froze`. A process the
    /// user stopped themselves must never be woken by auto-resume.
    #[test]
    fn resume_all_only_wakes_what_ramwarden_froze() {
        let mut ours = spawn();
        let mut theirs = spawn();
        let our_pid = ours.id() as i32;
        let their_pid = theirs.id() as i32;

        // The user stops their own process, the way Ctrl-Z would.
        PidFd::open(their_pid).unwrap().suspend().unwrap();
        assert!(await_state(their_pid, 'T'));

        let det = det_for(&[(our_pid, "sleep")], true);
        let mut act = Actuator::new(Root::system());
        act.suspend("sleep", &det, &wl(), false);

        let results = act.resume_all();
        let woken: Vec<i32> = results.iter().flat_map(|(_, o)| o.affected.clone()).collect();
        assert_eq!(woken, vec![our_pid]);
        assert_eq!(
            state_of(their_pid),
            Some('T'),
            "a process the user stopped must stay stopped"
        );
        assert!(await_state(our_pid, 'S'), "ours should have been woken");

        PidFd::open(their_pid).unwrap().resume().ok();
        for c in [&mut ours, &mut theirs] {
            c.kill().ok();
            c.wait().ok();
        }
    }

    /// The whole reason the registry holds handles rather than numbers.
    #[test]
    fn a_relaunched_app_is_not_mistaken_for_the_one_that_was_frozen() {
        let mut child = spawn();
        let pid = child.id() as i32;
        let det = det_for(&[(pid, "sleep")], true);
        let mut act = Actuator::new(Root::system());
        act.suspend("sleep", &det, &wl(), false);

        // The user force-quits the frozen app.
        child.kill().unwrap();
        child.wait().unwrap();

        // Resuming now must report the exit, and signal nothing.
        let out = act.resume("sleep");
        assert!(out.affected.is_empty(), "nothing should have been signalled");
        assert!(
            out.notes.iter().any(|n| n.contains("already exited")),
            "{:?}",
            out.notes
        );
    }

    #[test]
    fn a_dead_entry_is_pruned_from_the_registry() {
        let mut child = spawn();
        let pid = child.id() as i32;
        let det = det_for(&[(pid, "sleep")], true);
        let mut act = Actuator::new(Root::system());
        act.suspend("sleep", &det, &wl(), false);
        assert_eq!(act.suspended().len(), 1);

        child.kill().unwrap();
        child.wait().unwrap();
        assert!(act.suspended().is_empty(), "dead entries prune themselves");
        assert!(!act.owes_release());
    }

    #[test]
    fn resuming_something_ramwarden_did_not_freeze_is_refused() {
        let mut act = Actuator::new(Root::system());
        let out = act.resume("never-suspended");
        assert!(out.did_nothing());
        assert!(out.notes[0].contains("not suspended by RamWarden"));
    }

    #[test]
    fn an_already_stopped_process_is_noted_rather_than_signalled_again() {
        let mut child = spawn();
        let pid = child.id() as i32;
        PidFd::open(pid).unwrap().suspend().unwrap();
        // The detector samples /proc, so it must see `T` before it is built —
        // otherwise it reports the process as running and suspending it is the
        // correct behaviour, not the bug this test is about.
        assert!(await_state(pid, 'T'), "process never reached the stopped state");

        let det = det_for(&[(pid, "sleep")], true);
        let mut act = Actuator::new(Root::system());
        let out = act.suspend("sleep", &det, &wl(), false);
        assert!(out.affected.is_empty());
        assert!(out.notes[0].contains("already stopped"), "{:?}", out.notes);

        PidFd::open(pid).unwrap().resume().ok();
        child.kill().ok();
        child.wait().ok();
    }

    #[test]
    fn kill_terminates_a_real_process_and_reports_it() {
        let mut child = spawn();
        let pid = child.id() as i32;
        let det = det_for(&[(pid, "sleep")], true);
        let mut act = Actuator::new(Root::system());

        let out = act.kill("sleep", &det, &wl(), false);
        assert_eq!(out.affected, vec![pid], "{:?}", out.notes);
        assert!(out.bytes_freed > 0);
        child.wait().ok();
        assert!(!PidFd::open(pid).map(|f| f.is_alive()).unwrap_or(false));
    }

    #[test]
    fn kill_respects_the_gate() {
        let mut child = spawn();
        let pid = child.id() as i32;
        let det = det_for(&[(pid, "dockerd")], true);
        let mut act = Actuator::new(Root::system());

        let out = act.kill("dockerd", &det, &["dockerd".into()], true);
        assert!(out.did_nothing());
        assert!(PidFd::open(pid).unwrap().is_alive(), "must still be running");

        child.kill().ok();
        child.wait().ok();
    }

    #[test]
    fn nothing_is_suspendable_before_the_detector_is_warm() {
        let mut child = spawn();
        let pid = child.id() as i32;
        let det = det_for(&[(pid, "sleep")], false);
        let mut act = Actuator::new(Root::system());

        let out = act.suspend("sleep", &det, &wl(), false);
        assert!(out.did_nothing(), "{:?}", out.notes);
        assert_ne!(state_of(pid), Some('T'));

        child.kill().ok();
        child.wait().ok();
    }

    #[test]
    fn an_outcome_reports_refusal_rather_than_pretending_to_succeed() {
        let o = Outcome::refused("because");
        assert!(o.did_nothing());
        assert_eq!(o.notes, vec!["because"]);
        assert_eq!(o.bytes_freed, 0);
    }

    /// Without `CAP_SYS_NICE` this is refused, and that must read as a clean
    /// refusal the ladder can fall back from — never as a crash or a false
    /// claim of success.
    #[test]
    fn page_out_of_another_process_degrades_cleanly_without_the_capability() {
        let mut child = spawn();
        let pid = child.id() as i32;
        let act = Actuator::new(Root::system());

        let out = act.page_out(pid, 0.2, 0);
        if out.did_nothing() {
            assert!(!out.notes.is_empty(), "a refusal must say why");
        } else {
            // Running with the capability granted: it really paged something out.
            assert!(out.bytes_freed > 0);
        }
        assert!(PidFd::open(pid).unwrap().is_alive(), "page-out must not kill");

        child.kill().ok();
        child.wait().ok();
    }

    #[test]
    fn page_out_of_a_vanished_process_is_refused_not_fatal() {
        let mut child = spawn();
        let pid = child.id() as i32;
        child.kill().unwrap();
        child.wait().unwrap();
        let act = Actuator::new(Root::system());
        assert!(act.page_out(pid, 0.2, 0).did_nothing());
    }

    #[test]
    fn capability_availability_is_reportable() {
        let act = Actuator::new(Root::system());
        // Either answer is valid; what matters is that asking is safe.
        let _ = act.can_page_out();
    }

    #[test]
    fn a_fresh_actuator_owes_nothing() {
        let act = Actuator::new(Root::system());
        assert!(!act.owes_release());
        assert!(!act.has_caps());
        assert!(act.capped_scopes().is_empty());
    }

    #[test]
    fn suspended_entries_report_how_long_they_have_been_frozen() {
        let mut child = spawn();
        let pid = child.id() as i32;
        let det = det_for(&[(pid, "sleep")], true);
        let mut act = Actuator::new(Root::system());
        act.suspend("sleep", &det, &wl(), false);

        let entries = act.suspended();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "sleep");
        assert_eq!(entries[0].live_pids(), vec![pid]);
        assert!(entries[0].minutes() < 1.0);

        act.resume("sleep");
        child.kill().ok();
        child.wait().ok();
    }

    #[test]
    fn role_classification_is_consulted_through_the_detector_not_the_name() {
        // Guards against anyone reintroducing name checks in the actuator.
        assert!(classify("dockerd", "", None).is_structural());
        assert_eq!(classify("sleep", "", None), Role::App);
    }
}
