//! The Tidy rung: free housekeeping for quiet background apps, at any pressure.
//!
//! # Why a rung below reclaim
//!
//! The ladder waits for pressure, which is right for anything a user could
//! notice. But a desktop that is merely *full* — a browser, Discord and a
//! launcher idling at 31% of 32 GB — has memory sitting in apps nobody is
//! looking at, and there are levers that cost nothing to pull:
//!
//! * a **background hint** (Windows: low memory priority + EcoQoS) means that
//!   when memory is needed, these apps' pages go first and the foreground's
//!   stay;
//! * one **trim** per quiet period moves an app's private pages out of RAM to
//!   standby/compression. It faults back what it touches when used again.
//!
//! Neither stops, freezes or closes anything, so Tidy needs no watchlist and
//! may act at any pressure. It does need two things to be true first, and both
//! are deliberately conservative:
//!
//! 1. **Quiet for long enough** — no focus, audio, CPU or busy child for
//!    `after_minutes`. A process that was just used will be used again.
//! 2. **Not structurally protected.** Shell, games, anti-cheat, VMs, agents,
//!    builds and installers are never tidied. A process that only *serves* a
//!    socket may be: its clients do not care whether its idle pages are
//!    resident.
//!
//! Everything is undone the moment the process shows activity again, and each
//! quiet period gets at most one trim — trimming every tick would just churn.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use crate::actuator::{Actuator, Outcome};
use crate::config;
use crate::detector::Detector;
use crate::ladder::{COLD_RATIO, COLD_VMA_FLOOR};
use crate::rank::{DEFER_TRIM_ABOVE, ReuseRanker};
use crate::signals::{Protection, Signals};

/// Smaller than the ladder's floor, because tidying costs nothing: below this
/// a process holds too little for a trim to be worth its soft faults.
pub const TIDY_FLOOR_BYTES: u64 = 64 * 1024 * 1024;

/// One tidy action: (what, target, outcome), the same shape as ladder steps.
pub type Step = (&'static str, String, Outcome);

/// What Tidy currently believes about one process.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub pid: i32,
    pub name: String,
    pub quiet_minutes: f64,
    pub hinted: bool,
    pub trimmed: bool,
}

pub struct Tidy {
    cfg: config::Tidy,
    /// pid -> when it was first seen quiet in the current quiet period.
    quiet_since: HashMap<i32, Instant>,
    /// pid -> name, for processes RamWarden marked as background.
    hinted: HashMap<i32, String>,
    /// pids trimmed in the current quiet period.
    trimmed: HashSet<i32>,
}

impl Tidy {
    pub fn new(cfg: config::Tidy) -> Self {
        Tidy {
            cfg,
            quiet_since: HashMap::new(),
            hinted: HashMap::new(),
            trimmed: HashSet::new(),
        }
    }

    pub fn enabled(&self) -> bool {
        self.cfg.enabled
    }

    fn eligible(s: &Signals) -> bool {
        s.protection != Protection::Structural
            && s.is_quiet()
            && !s.is_stopped()
            && s.pss >= TIDY_FLOOR_BYTES
    }

    fn label(s: &Signals) -> String {
        format!("{} (pid {})", s.name, s.pid)
    }

    /// One pass over the detector's snapshot. Returns only what changed.
    pub fn step(
        &mut self,
        det: &Detector,
        act: &Actuator,
        ranker: Option<&dyn ReuseRanker>,
        now: Instant,
    ) -> Vec<Step> {
        if !self.cfg.enabled || !det.is_warm() {
            return Vec::new();
        }
        let after = Duration::from_secs_f64(self.cfg.after_minutes.max(0.0) * 60.0);
        let snapshot = det.snapshot();
        let mut steps = Vec::new();

        // Forget processes that have exited: there is nothing left to undo.
        self.quiet_since.retain(|pid, _| snapshot.contains_key(pid));
        self.hinted.retain(|pid, _| snapshot.contains_key(pid));
        self.trimmed.retain(|pid| snapshot.contains_key(pid));

        let mut pids: Vec<&i32> = snapshot.keys().collect();
        pids.sort_unstable();
        for pid in pids {
            let s = &snapshot[pid];
            if !Self::eligible(s) {
                // Activity, or protection: end the quiet period and undo.
                self.quiet_since.remove(pid);
                self.trimmed.remove(pid);
                if self.hinted.remove(pid).is_some() {
                    steps.push(("foreground", Self::label(s), act.set_background(*pid, false)));
                }
                continue;
            }

            let since = *self.quiet_since.entry(*pid).or_insert(now);
            if now.saturating_duration_since(since) < after {
                continue;
            }

            if self.cfg.background_hints && act.can_hint() && !self.hinted.contains_key(pid) {
                let outcome = act.set_background(*pid, true);
                if !outcome.did_nothing() {
                    self.hinted.insert(*pid, s.name.clone());
                }
                steps.push(("background", Self::label(s), outcome));
            }

            // A process the ranker expects back soon would fault its pages
            // straight back in; leave it, and ask again next tick.
            let expected_back = ranker
                .and_then(|r| r.reuse(*pid))
                .is_some_and(|p| p >= DEFER_TRIM_ABOVE);
            if self.cfg.trim && act.can_page_out() && !self.trimmed.contains(pid) && !expected_back {
                // Once per quiet period, whatever the outcome: a process that
                // had nothing to trim will not have more on the next tick.
                self.trimmed.insert(*pid);
                let outcome = act.page_out(*pid, COLD_RATIO, COLD_VMA_FLOOR.max(TIDY_FLOOR_BYTES / 4));
                if !outcome.did_nothing() {
                    steps.push(("tidy_trim", Self::label(s), outcome));
                }
            }
        }
        steps
    }

    /// Restore every process RamWarden marked as background. Called on
    /// shutdown, so no hint outlives the daemon that set it.
    pub fn release_all(&mut self, act: &Actuator) -> Vec<Step> {
        let hinted: Vec<(i32, String)> = self.hinted.drain().collect();
        self.quiet_since.clear();
        self.trimmed.clear();
        hinted
            .into_iter()
            .map(|(pid, name)| ("foreground", format!("{name} (pid {pid})"), act.set_background(pid, false)))
            .collect()
    }

    /// What Tidy is tracking, for `/tidy` and the window.
    pub fn entries(&self, det: &Detector, now: Instant) -> Vec<Entry> {
        let mut out: Vec<Entry> = self
            .quiet_since
            .iter()
            .filter_map(|(pid, since)| {
                let s = det.get(*pid)?;
                Some(Entry {
                    pid: *pid,
                    name: s.name.clone(),
                    quiet_minutes: now.saturating_duration_since(*since).as_secs_f64() / 60.0,
                    hinted: self.hinted.contains_key(pid),
                    trimmed: self.trimmed.contains(pid),
                })
            })
            .collect();
        out.sort_by(|a, b| b.quiet_minutes.total_cmp(&a.quiet_minutes).then(a.pid.cmp(&b.pid)));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::roles::classify;
    use ramwarden_platform::fake::{self, Shared};
    use std::sync::{Arc, Mutex};

    const MB: u64 = 1024 * 1024;

    fn sig(pid: i32, name: &str, mb: u64) -> Signals {
        let mut s = Signals::new(pid, name, classify(name, "", None));
        s.pss = mb * MB;
        s.age_minutes = 600.0;
        s
    }

    fn setup(sigs: &[Signals]) -> (Shared, Actuator) {
        let mut w = fake::World::default();
        for s in sigs {
            w.processes.push(fake::process(s.pid, &s.name, s.pss));
        }
        let shared = Arc::new(Mutex::new(w));
        let act = Actuator::for_platform(&fake::platform(shared.clone()));
        (shared, act)
    }

    fn tidy() -> Tidy {
        Tidy::new(config::Tidy::default())
    }

    #[test]
    fn a_quiet_app_is_hinted_and_trimmed_once_after_the_quiet_period() {
        let sigs = vec![sig(20, "Discord", 500)];
        let (shared, act) = setup(&sigs);
        let det = Detector::preloaded(true, sigs);
        let mut t = tidy();
        let t0 = Instant::now();

        assert!(t.step(&det, &act, None, t0).is_empty(), "not quiet long enough yet");
        let steps = t.step(&det, &act, None, t0 + Duration::from_secs(601));
        let actions: Vec<&str> = steps.iter().map(|(a, _, _)| *a).collect();
        assert_eq!(actions, vec!["background", "tidy_trim"]);

        // The next tick in the same quiet period does nothing more.
        assert!(t.step(&det, &act, None, t0 + Duration::from_secs(700)).is_empty());
        let w = shared.lock().unwrap();
        assert_eq!(fake::count(&w, "pageout 20"), 1);
        assert_eq!(w.background, vec![20]);
    }

    #[test]
    fn activity_undoes_the_hint_and_starts_a_new_quiet_period() {
        let quiet = vec![sig(20, "Discord", 500)];
        let (shared, act) = setup(&quiet);
        let mut t = tidy();
        let t0 = Instant::now();
        let det = Detector::preloaded(true, quiet.clone());
        t.step(&det, &act, None, t0);
        t.step(&det, &act, None, t0 + Duration::from_secs(601));

        let mut focused = quiet[0].clone();
        focused.is_focused = true;
        let det = Detector::preloaded(true, vec![focused]);
        let steps = t.step(&det, &act, None, t0 + Duration::from_secs(620));
        assert_eq!(steps.iter().map(|(a, _, _)| *a).collect::<Vec<_>>(), vec!["foreground"]);
        assert!(shared.lock().unwrap().background.is_empty());

        // Quiet again: a fresh period, so a fresh trim once it has elapsed.
        let det = Detector::preloaded(true, quiet);
        assert!(t.step(&det, &act, None, t0 + Duration::from_secs(630)).is_empty());
        let steps = t.step(&det, &act, None, t0 + Duration::from_secs(1300));
        assert!(steps.iter().any(|(a, _, _)| *a == "tidy_trim"));
    }

    /// A game, the shell, an agent: structural, never tidied, at any age.
    #[test]
    fn structural_processes_are_never_tidied() {
        let sigs = vec![
            sig(30, "FortniteClient-Win64-Shipping", 3000),
            sig(40, "explorer", 300),
            sig(50, "claude", 400),
        ];
        let (shared, act) = setup(&sigs);
        let det = Detector::preloaded(true, sigs);
        let mut t = tidy();
        let t0 = Instant::now();
        t.step(&det, &act, None, t0);
        assert!(t.step(&det, &act, None, t0 + Duration::from_secs(3600)).is_empty());
        assert!(shared.lock().unwrap().log.is_empty());
    }

    /// Serving a socket protects against suspend and kill, not against a
    /// trim: Discord's RPC clients do not need its idle pages resident.
    #[test]
    fn a_quiet_server_may_be_tidied() {
        let mut s = sig(20, "Discord", 500);
        s.listening_ports = vec![6463];
        let (_, act) = setup(std::slice::from_ref(&s));
        let det = Detector::preloaded(true, vec![s]);
        assert_eq!(det.get(20).unwrap().protection, Protection::Serving);
        let mut t = tidy();
        let t0 = Instant::now();
        t.step(&det, &act, None, t0);
        assert!(!t.step(&det, &act, None, t0 + Duration::from_secs(601)).is_empty());
    }

    #[test]
    fn nothing_happens_while_the_detector_is_cold_or_tidy_is_disabled() {
        let sigs = vec![sig(20, "Discord", 500)];
        let (_, act) = setup(&sigs);
        let t0 = Instant::now();

        let cold = Detector::preloaded(false, sigs.clone());
        let mut t = tidy();
        t.step(&cold, &act, None, t0);
        assert!(t.step(&cold, &act, None, t0 + Duration::from_secs(3600)).is_empty());

        let warm = Detector::preloaded(true, sigs);
        let mut off = Tidy::new(config::Tidy { enabled: false, ..Default::default() });
        off.step(&warm, &act, None, t0);
        assert!(off.step(&warm, &act, None, t0 + Duration::from_secs(3600)).is_empty());
    }

    #[test]
    fn small_processes_are_not_worth_tidying() {
        let sigs = vec![sig(20, "tiny-helper", 30)];
        let (_, act) = setup(&sigs);
        let det = Detector::preloaded(true, sigs);
        let mut t = tidy();
        let t0 = Instant::now();
        t.step(&det, &act, None, t0);
        assert!(t.step(&det, &act, None, t0 + Duration::from_secs(3600)).is_empty());
    }

    struct Says(f64);
    impl ReuseRanker for Says {
        fn reuse(&self, _: i32) -> Option<f64> {
            Some(self.0)
        }
    }

    /// The ranker may defer a trim, never cause one.
    #[test]
    fn a_process_expected_back_soon_is_hinted_but_not_trimmed() {
        let sigs = vec![sig(20, "Discord", 500)];
        let (shared, act) = setup(&sigs);
        let det = Detector::preloaded(true, sigs);
        let mut t = tidy();
        let t0 = Instant::now();
        t.step(&det, &act, Some(&Says(0.9)), t0);
        let steps = t.step(&det, &act, Some(&Says(0.9)), t0 + Duration::from_secs(601));
        assert_eq!(steps.iter().map(|(a, _, _)| *a).collect::<Vec<_>>(), vec!["background"]);
        assert_eq!(fake::count(&shared.lock().unwrap(), "pageout"), 0);

        // Once the ranker no longer expects it, the deferred trim happens.
        let steps = t.step(&det, &act, Some(&Says(0.1)), t0 + Duration::from_secs(700));
        assert!(steps.iter().any(|(a, _, _)| *a == "tidy_trim"));
    }

    #[test]
    fn release_all_restores_every_hint() {
        let sigs = vec![sig(20, "Discord", 500), sig(21, "EpicGamesLauncher", 300)];
        let (shared, act) = setup(&sigs);
        let det = Detector::preloaded(true, sigs);
        let mut t = tidy();
        let t0 = Instant::now();
        t.step(&det, &act, None, t0);
        t.step(&det, &act, None, t0 + Duration::from_secs(601));
        assert_eq!(shared.lock().unwrap().background.len(), 2);

        let steps = t.release_all(&act);
        assert_eq!(steps.len(), 2);
        assert!(shared.lock().unwrap().background.is_empty());
    }

    #[test]
    fn entries_report_what_is_being_tracked() {
        let sigs = vec![sig(20, "Discord", 500)];
        let (_, act) = setup(&sigs);
        let det = Detector::preloaded(true, sigs);
        let mut t = tidy();
        let t0 = Instant::now();
        t.step(&det, &act, None, t0);
        t.step(&det, &act, None, t0 + Duration::from_secs(601));
        let e = t.entries(&det, t0 + Duration::from_secs(601));
        assert_eq!(e.len(), 1);
        assert!(e[0].hinted && e[0].trimmed);
        assert!(e[0].quiet_minutes >= 10.0);
    }
}
