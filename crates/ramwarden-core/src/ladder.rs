//! The remediation ladder: what RamWarden does, on its own, as pressure rises.
//!
//! # Reclaim first, signals last
//!
//! The rungs are ordered by what they cost the user if the decision is wrong:
//!
//! | Rung | Action | Cost of being wrong |
//! |---|---|---|
//! | `Reclaim` | kernel pages cold memory to zram | a few page faults |
//! | `PageOut` | targeted page-out, soft cap | a few page faults |
//! | `Tabs` | unload idle browser tabs in bounded batches | reload on activation |
//! | `Suspend` | `SIGSTOP` idle watchlisted apps | app looks frozen until resumed |
//! | `Kill` | `SIGTERM` then `SIGKILL` | **unsaved work is gone** |
//!
//! v1 had only the bottom two of those and asked permission for both. v2 runs
//! autonomously, which is only defensible because the top rungs exist: on this
//! machine `Reclaim` returned 137 MB from a single Brave scope without the
//! browser noticing, and there are four Brave scopes. Most pressure never needs
//! a signal at all.
//!
//! # Escalation is cumulative
//!
//! Reaching `Suspend` does not mean skipping `Reclaim` — it means doing reclaim
//! *and* page-out *and* tabs *and* suspend on this tick. Pressure high enough to
//! warrant freezing an application is also high enough to want the free wins.
//!
//! # Hysteresis
//!
//! Between `release_below` and `reclaim_at` the ladder holds: it neither acts nor
//! undoes. Without that band a machine hovering at the threshold would suspend
//! and resume the same application every tick.

use std::time::{Duration, Instant};

use ramwarden_kernel::psi::Psi;
use ramwarden_kernel::{cgroup, smaps};

use crate::actuator::{Actuator, Outcome};
use crate::config;
use crate::detector::Detector;
use crate::history::{History, NewAction};
use crate::judge::{Candidate, Intent, Judge};
use crate::signals::{Signals, Verdict};
use crate::structures::{CircularList, LinkedQueue, LinkedStack};

/// How much of a scope's reclaimable memory to ask for at the first rung.
///
/// Not all of it: asking for everything cold makes the application fault its
/// working set back in immediately, which costs more than it saves. Two thirds
/// leaves a margin.
const RECLAIM_FRACTION: f64 = 0.66;

/// Don't bother with a scope holding less than this.
const SCOPE_FLOOR: u64 = 64 * 1024 * 1024;

/// Cap a soft-capped scope at this fraction of its current charge.
const SOFT_CAP_FRACTION: f64 = 0.8;

/// How much recent touching still counts as cold, for targeted page-out.
const COLD_RATIO: f64 = 0.2;

/// Ignore mappings smaller than this — not worth an iovec entry.
const COLD_VMA_FLOOR: u64 = 2 * 1024 * 1024;

/// At most this many processes get targeted page-out per tick, largest first.
/// Walking full `smaps` is expensive, and the cgroup rung has already taken the
/// cheap bulk of the memory.
const PAGEOUT_LIMIT: usize = 3;

/// How many candidates from the LRU tail to put in front of the judge per tick.
///
/// The coldest few are where the decision is; asking about everything would
/// make the request larger without changing what gets closed first.
const JUDGE_LIMIT: usize = 8;

/// Which rung the current pressure calls for.
///
/// Ordered, so `>=` comparisons read naturally and the cumulative escalation in
/// [`Ladder::step`] is a simple range.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rung {
    /// Pressure has passed. Undo everything reversible.
    Release,
    /// In the hysteresis band: do nothing, undo nothing.
    Hold,
    Reclaim,
    PageOut,
    Tabs,
    Suspend,
    Kill,
}

impl Rung {
    pub fn index(self) -> i32 {
        match self {
            Rung::Release => -2,
            Rung::Hold => -1,
            Rung::Reclaim => 0,
            Rung::PageOut => 1,
            Rung::Tabs => 2,
            Rung::Suspend => 3,
            Rung::Kill => 4,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Rung::Release => "release",
            Rung::Hold => "hold",
            Rung::Reclaim => "reclaim",
            Rung::PageOut => "pageout",
            Rung::Tabs => "tabs",
            Rung::Suspend => "suspend",
            Rung::Kill => "kill",
        }
    }
}

/// Decide the rung from pressure alone.
///
/// Pure, so the whole escalation policy can be driven by a synthetic PSI series
/// rather than by putting a real machine into swap.
pub fn rung_for(cfg: &config::Ladder, psi: &Psi, available_bytes: u64) -> Rung {
    let some = psi.some.avg10;
    let full = psi.full.avg10;
    let available_mb = available_bytes / 1_000_000;

    // `full` means nothing is getting useful work done, and low available memory
    // means the OOM killer is close. Either justifies the last rung on its own —
    // by the time thrashing shows up in a ten-second average it is already late.
    if full >= cfg.kill_at_full || available_mb < cfg.kill_below_available_mb {
        return Rung::Kill;
    }
    if some >= cfg.suspend_at {
        return Rung::Suspend;
    }
    if some >= cfg.tabs_at {
        return Rung::Tabs;
    }
    if some >= cfg.pageout_at {
        return Rung::PageOut;
    }
    if some >= cfg.reclaim_at {
        return Rung::Reclaim;
    }
    if some <= cfg.release_below {
        return Rung::Release;
    }
    Rung::Hold
}

/// Closing browser tabs needs the daemon's WebSocket connections, which the
/// ladder has no business knowing about.
pub trait TabCloser {
    /// Close stale tabs and report what actually closed.
    fn close_stale(&mut self, goal: &str) -> Outcome;
}

/// Everything the ladder needs to look at, assembled by the caller.
pub struct World<'a> {
    pub det: &'a Detector,
    /// `None` when the memory controller is not delegated — the kernel rungs are
    /// then unavailable and the ladder skips straight to signals.
    pub hierarchy: Option<&'a cgroup::Hierarchy>,
    pub psi: Psi,
    pub available_bytes: u64,
    pub watchlist: &'a [String],
    pub goal: String,
    pub tabs: Option<&'a mut dyn TabCloser>,
    /// Asked before suspending or killing. `None` means the gate alone decides.
    pub judge: Option<&'a mut dyn Judge>,
}

/// A kill the ladder intends to carry out, once its grace period expires.
///
/// The kill proceeds by default — this is a window to intervene, not a request
/// for permission. With `kill_grace_seconds = 0` there is no window at all.
#[derive(Clone, Debug)]
pub struct PendingKill {
    pub targets: Vec<String>,
    pub armed_at: Instant,
    pub grace: Duration,
}

impl PendingKill {
    pub fn due(&self) -> bool {
        self.armed_at.elapsed() >= self.grace
    }

    pub fn remaining(&self) -> Duration {
        self.grace.saturating_sub(self.armed_at.elapsed())
    }
}

/// What the ladder *would* do, without doing it.
///
/// The UI shows this so the user can see the policy before it fires, and it is
/// the only honest way to inspect an autonomous ladder on a live machine without
/// provoking it.
#[derive(Clone, Debug, Default)]
pub struct Plan {
    pub rung: Option<Rung>,
    /// (label, bytes that would be requested) per scope.
    pub reclaim: Vec<(String, u64)>,
    /// The scope that would be soft-capped, and to what charge.
    pub soft_cap: Option<(String, u64)>,
    pub suspend: Vec<String>,
    pub kill: Vec<String>,
    /// Why nothing would happen, when that is the answer.
    pub blocked: Option<String>,
}

/// One tick's worth of what the ladder did.
#[derive(Debug, Default)]
pub struct Report {
    pub rung: Option<Rung>,
    /// (action, target, outcome) in the order they were attempted.
    pub steps: Vec<(&'static str, String, Outcome)>,
    pub total_freed: u64,
    /// Set when a kill is armed and waiting out its grace period.
    pub kill_pending: Option<PendingKill>,
}

impl Report {
    pub fn did_something(&self) -> bool {
        self.total_freed > 0 || self.steps.iter().any(|(_, _, o)| !o.did_nothing())
    }
}

pub struct Ladder {
    cfg: config::Ladder,
    act: Actuator,
    history: Option<History>,
    pending_kill: Option<PendingKill>,
    /// Kills whose grace period is over, coldest first. One leaves per tick, so
    /// pressure is re-read between kills and the rest are spared the moment it
    /// drops below the kill rung.
    kill_queue: LinkedQueue<(String, Instant)>,
    /// What the ladder suspended, newest on top, so "undo" resumes the most
    /// recent first.
    undo: LinkedStack<String>,
    /// Idle processes taking turns at targeted page-out. Always picking the
    /// largest three meant paging out the same already-cold processes every
    /// tick; going round the circle reaches every idle process in turn.
    pageout_turns: CircularList<i32>,
    last_tabs: Option<Instant>,
}

impl Ladder {
    pub fn new(cfg: config::Ladder, act: Actuator) -> Self {
        Ladder {
            cfg,
            act,
            history: None,
            pending_kill: None,
            kill_queue: LinkedQueue::new(),
            undo: LinkedStack::new(),
            pageout_turns: CircularList::new(),
            last_tabs: None,
        }
    }

    /// Record every action to the history database.
    ///
    /// Not optional in production: when nobody is watching the ladder escalate,
    /// this record is the only way to answer "why is my editor frozen?"
    pub fn with_history(mut self, h: History) -> Self {
        self.history = Some(h);
        self
    }

    pub fn actuator(&mut self) -> &mut Actuator {
        &mut self.act
    }

    /// Whether any soft cap is currently set. Read-only, for assertions and
    /// for the UI's status line.
    pub fn actuator_has_caps(&self) -> bool {
        self.act.has_caps()
    }

    /// The action log, if one was attached. The UI reads this to show what the
    /// ladder has been doing unattended.
    pub fn history(&self) -> Option<&History> {
        self.history.as_ref()
    }

    pub fn pending_kill(&self) -> Option<&PendingKill> {
        self.pending_kill.as_ref()
    }

    /// Kills past their grace period that have not happened yet, next first.
    pub fn queued_kills(&self) -> Vec<String> {
        self.kill_queue.iter().map(|(name, _)| name.clone()).collect()
    }

    /// Call off an armed kill, and any still queued. The UI's cancel button.
    pub fn cancel_kill(&mut self) -> bool {
        let queued = !self.kill_queue.is_empty();
        self.kill_queue.clear();
        self.pending_kill.take().is_some() || queued
    }

    /// Applications the ladder suspended, most recent first.
    pub fn undo_stack(&self) -> Vec<String> {
        self.undo.iter().cloned().collect()
    }

    /// Resume the most recently suspended application that is still suspended.
    ///
    /// Entries already resumed by other means (the release rung, the UI's
    /// resume button) are discarded on the way down.
    pub fn undo_last(&mut self) -> Option<(String, Outcome)> {
        while let Some(name) = self.undo.pop() {
            if self.act.is_suspended(&name) {
                let outcome = self.act.resume(&name);
                return Some((name, outcome));
            }
        }
        None
    }

    /// Work out what this tick would do, changing nothing.
    pub fn plan(&self, w: &World<'_>) -> Plan {
        let rung = rung_for(&self.cfg, &w.psi, w.available_bytes);
        let mut plan = Plan {
            rung: Some(rung),
            ..Default::default()
        };

        if !w.det.is_warm() && rung != Rung::Release {
            plan.blocked = Some("activity detector is still warming up".into());
            return plan;
        }
        if matches!(rung, Rung::Release | Rung::Hold) {
            return plan;
        }

        if let Some(h) = w.hierarchy {
            let targets = self.reclaim_targets(w, h);
            if rung >= Rung::PageOut
                && let Some((scope, _)) = targets.first()
                && let Ok(current) = scope.current()
            {
                let cap = (current as f64 * SOFT_CAP_FRACTION) as u64;
                if cap >= SCOPE_FLOOR {
                    let label = scope
                        .label(self.act.root())
                        .unwrap_or_else(|_| scope.name().to_string());
                    plan.soft_cap = Some((label, cap));
                }
            }
            plan.reclaim = targets
                .into_iter()
                .take(4)
                .map(|(scope, ask)| {
                    let label = scope
                        .label(self.act.root())
                        .unwrap_or_else(|_| scope.name().to_string());
                    (label, ask)
                })
                .collect();
        } else {
            plan.blocked = Some("no delegated cgroup memory controller".into());
        }

        if rung >= Rung::Suspend {
            plan.suspend = self.eligible_names(w);
        }
        if rung >= Rung::Kill {
            plan.kill = self.eligible_names(w);
        }
        plan
    }

    /// Distinct process names that are both idle and on the watchlist, least
    /// recently used first.
    ///
    /// `reclaimable` has already applied the gate and the watchlist ceiling, so
    /// this only deduplicates and orders. The order comes from the detector's
    /// LRU: walking it from the tail means the app the user has gone longest
    /// without touching is the first considered, rather than simply the largest.
    fn eligible_names(&self, w: &World<'_>) -> Vec<String> {
        let mut seen: Vec<String> = Vec::new();
        for s in w.det.reclaimable(w.watchlist) {
            if !seen.contains(&s.name) {
                seen.push(s.name.clone());
            }
        }
        let mut ordered: Vec<String> = w
            .det
            .coldest()
            .filter(|(name, _)| seen.iter().any(|s| s == name))
            .map(|(name, _)| name.to_string())
            .collect();
        // Anything the LRU has not seen yet keeps its largest-first place.
        for name in seen {
            if !ordered.contains(&name) {
                ordered.push(name);
            }
        }
        ordered
    }

    /// The eligible names a judge agrees to act on, in LRU order, plus the
    /// steps to report for each one it vetoed.
    ///
    /// `Err` when a judge is attached but could not be reached.
    fn judged_names(
        &self,
        w: &mut World<'_>,
        intent: Intent,
    ) -> (Vec<String>, Vec<(String, Outcome)>, Option<String>) {
        let names = self.eligible_names(w);
        let Some(judge) = w.judge.as_mut() else {
            return (names, Vec::new(), None);
        };
        if names.is_empty() {
            return (names, Vec::new(), None);
        }

        let candidates: Vec<Candidate> = names
            .iter()
            .take(JUDGE_LIMIT)
            .enumerate()
            .map(|(rank, name)| {
                let procs: Vec<&Signals> = w
                    .det
                    .reclaimable(w.watchlist)
                    .into_iter()
                    .filter(|s| &s.name == name)
                    .collect();
                let idle = w
                    .det
                    .recency(name)
                    .map(|r| r.idle().as_secs_f64() / 60.0)
                    .unwrap_or(0.0);
                Candidate::from_signals(name, rank, idle, &procs)
            })
            .collect();

        match judge.judge(intent, &candidates) {
            Ok(verdicts) => {
                let mut keep = Vec::new();
                let mut vetoed = Vec::new();
                for c in &candidates {
                    match verdicts.iter().find(|j| j.name == c.name) {
                        Some(j) if j.close => keep.push(c.name.clone()),
                        Some(j) => vetoed.push((c.name.clone(), Outcome::refused(j.reason.clone()))),
                        None => vetoed.push((
                            c.name.clone(),
                            Outcome::refused("the judge gave no verdict"),
                        )),
                    }
                }
                (keep, vetoed, None)
            }
            Err(e) => (names, Vec::new(), Some(e)),
        }
    }

    /// Run one tick.
    pub fn step(&mut self, w: &mut World<'_>) -> Report {
        let rung = rung_for(&self.cfg, &w.psi, w.available_bytes);
        let mut report = Report {
            rung: Some(rung),
            ..Default::default()
        };

        // A detector with no CPU delta yet cannot honestly call anything idle,
        // so there is nothing safe to act on. Release is still allowed: undoing
        // is safe regardless of what we can measure.
        if !w.det.is_warm() && rung != Rung::Release {
            report.steps.push((
                "hold",
                String::new(),
                Outcome::refused("activity detector is still warming up"),
            ));
            return report;
        }

        match rung {
            Rung::Release => {
                self.pending_kill = None;
                self.kill_queue.clear();
                self.release(w, &mut report);
                return report;
            }
            Rung::Hold => {
                self.pending_kill = None;
                self.kill_queue.clear();
                return report;
            }
            _ => {}
        }

        // Cumulative: every rung at or below the target runs.
        if rung >= Rung::Reclaim {
            self.do_reclaim(w, &mut report);
        }
        if rung >= Rung::PageOut {
            self.do_page_out(w, &mut report);
            self.do_soft_cap(w, &mut report);
        }
        if rung >= Rung::Tabs {
            self.do_tabs(w, &mut report);
        }
        if rung >= Rung::Suspend {
            self.do_suspend(w, &mut report);
        }
        if rung >= Rung::Kill {
            self.do_kill(w, &mut report);
        } else {
            // Dropped back below the kill threshold: stand the kill down.
            self.pending_kill = None;
            self.kill_queue.clear();
        }

        report.kill_pending = self.pending_kill.clone();
        self.log(&report, w, rung);
        report
    }

    // ── Rung 0: kernel reclaim ──────────────────────────────────────────────

    /// Scopes worth reclaiming from, largest reclaimable share first.
    ///
    /// A scope is skipped entirely if *any* process in it is structurally
    /// protected. Reclaim is non-destructive, but asking the kernel to page out
    /// the compositor's cold memory still buys a stutter in exchange for very
    /// little, and the compositor shares a session with nothing else worth
    /// reclaiming anyway.
    fn reclaim_targets(
        &self,
        w: &World<'_>,
        h: &cgroup::Hierarchy,
    ) -> Vec<(cgroup::Scope, u64)> {
        let Ok(scopes) = h.scopes() else {
            return Vec::new();
        };
        let mut out = Vec::new();

        for scope in scopes {
            let Ok(pids) = scope.pids() else { continue };
            if pids.is_empty() {
                continue;
            }

            let mut protected = false;
            let mut any_idle = false;
            for pid in &pids {
                match w.det.get(*pid).map(|s| s.verdict) {
                    Some(Verdict::Protected) => {
                        protected = true;
                        break;
                    }
                    Some(Verdict::Idle) => any_idle = true,
                    _ => {}
                }
            }
            if protected || !any_idle {
                continue;
            }

            let Ok(stat) = scope.stat() else { continue };
            let reclaimable = stat.reclaimable();
            if reclaimable < SCOPE_FLOOR {
                continue;
            }
            out.push((scope, (reclaimable as f64 * RECLAIM_FRACTION) as u64));
        }

        out.sort_by_key(|(_, ask)| std::cmp::Reverse(*ask));
        out
    }

    fn do_reclaim(&mut self, w: &World<'_>, report: &mut Report) {
        let Some(h) = w.hierarchy else {
            report.steps.push((
                "reclaim",
                String::new(),
                Outcome::refused("no delegated cgroup memory controller"),
            ));
            return;
        };

        for (scope, ask) in self.reclaim_targets(w, h).into_iter().take(4) {
            let label = scope.label(self.act.root()).unwrap_or_else(|_| scope.name().to_string());
            let outcome = self.act.reclaim_scope(&scope, ask);
            report.total_freed += outcome.bytes_freed;
            report.steps.push(("reclaim", label, outcome));
        }
    }

    // ── Rung 1: targeted page-out, then a soft cap ──────────────────────────

    /// Page out the cold regions of idle processes, a few per tick, taking turns.
    ///
    /// Runs after cgroup reclaim has taken the bulk, and only reaches a few per
    /// tick — walking full `smaps` costs a syscall per mapping, and a browser has
    /// thousands. Which few comes from a circular list, so successive ticks work
    /// round every idle process instead of re-paging the same largest three. Without `CAP_SYS_NICE` every call is refused and
    /// the rung contributes nothing, which is the expected configuration.
    fn do_page_out(&mut self, w: &World<'_>, report: &mut Report) {
        if !self.act.can_page_out() {
            report.steps.push((
                "pageout",
                String::new(),
                Outcome::refused("needs CAP_SYS_NICE (ramwarden-helper not installed)"),
            ));
            return;
        }

        let mut idle: Vec<(i32, String, u64)> = w
            .det
            .snapshot()
            .values()
            .filter(|s| s.verdict == Verdict::Idle && s.pss >= SCOPE_FLOOR)
            .map(|s| (s.pid, s.name.clone(), s.pss))
            .collect();
        idle.sort_by_key(|(_, _, pss)| std::cmp::Reverse(*pss));

        // Keep the circle in step with what is idle now; newcomers join at the
        // back, largest first.
        self.pageout_turns.retain(|pid| idle.iter().any(|(p, _, _)| p == pid));
        for (pid, _, _) in &idle {
            if !self.pageout_turns.iter().any(|p| p == pid) {
                self.pageout_turns.insert_last(*pid);
            }
        }
        let turns = PAGEOUT_LIMIT.min(self.pageout_turns.len());
        let picked: Vec<i32> = (0..turns).filter_map(|_| self.pageout_turns.turn().copied()).collect();

        for pid in picked {
            let Some((_, name, _)) = idle.iter().find(|(p, _, _)| *p == pid).cloned() else {
                continue;
            };
            let outcome = self.act.page_out(pid, COLD_RATIO, COLD_VMA_FLOOR);
            report.total_freed += outcome.bytes_freed;
            report.steps.push(("pageout", format!("{name} (pid {pid})"), outcome));
        }
    }

    fn do_soft_cap(&mut self, w: &World<'_>, report: &mut Report) {
        let Some(h) = w.hierarchy else { return };
        let targets = self.reclaim_targets(w, h);
        let Some((scope, _)) = targets.into_iter().next() else {
            return;
        };
        let Ok(current) = scope.current() else { return };
        let cap = (current as f64 * SOFT_CAP_FRACTION) as u64;
        if cap < SCOPE_FLOOR {
            return;
        }
        let label = scope.label(self.act.root()).unwrap_or_else(|_| scope.name().to_string());
        let outcome = self.act.soft_cap(&scope, cap);
        report.steps.push(("soft_cap", label, outcome));
    }

    // ── Rung 2: tabs ────────────────────────────────────────────────────────

    fn do_tabs(&mut self, w: &mut World<'_>, report: &mut Report) {
        // Give the browser and pressure signal time to settle between batches.
        if self.last_tabs.is_some_and(|at| at.elapsed() < Duration::from_secs(30)) { return; }
        self.last_tabs = Some(Instant::now());
        let goal = w.goal.clone();
        let Some(closer) = w.tabs.as_mut() else {
            report.steps.push((
                "discard_tab",
                String::new(),
                Outcome::refused("no browser connected"),
            ));
            return;
        };
        let outcome = closer.close_stale(&goal);
        report.total_freed += outcome.bytes_freed;
        report.steps.push(("discard_tab", "idle tabs".into(), outcome));
    }

    // ── Rung 3: suspend ─────────────────────────────────────────────────────

    /// Suspending is reversible, so a judge that cannot be reached does not
    /// block it: the gate alone decides, as it did before judges existed.
    fn do_suspend(&mut self, w: &mut World<'_>, report: &mut Report) {
        let (names, vetoed, unreachable) = self.judged_names(w, Intent::Suspend);
        if let Some(e) = unreachable {
            tracing::warn!("judge unavailable for suspend ({e}) — using the gate alone");
        }
        for (name, outcome) in vetoed {
            report.steps.push(("suspend", name, outcome));
        }
        for name in names {
            if self.act.is_suspended(&name) {
                continue;
            }
            let outcome = self.act.suspend(&name, w.det, w.watchlist, false);
            if !outcome.did_nothing() {
                self.undo.push(name.clone());
            }
            report.total_freed += outcome.bytes_freed;
            report.steps.push(("suspend", name, outcome));
        }
    }

    // ── Rung 4: kill ────────────────────────────────────────────────────────

    /// Kill whatever is next in the queue, if it is still eligible.
    ///
    /// Re-checked at the last moment against the LRU: an app the user came back
    /// to after it was armed must not be killed for having been idle a minute
    /// ago. (Not against the gate: by now the suspend rung has usually stopped
    /// it, and a stopped process no longer counts as reclaimable.)
    fn kill_next(&mut self, w: &World<'_>, report: &mut Report) {
        let Some((name, armed_at)) = self.kill_queue.dequeue() else {
            return;
        };
        if w.det.recency(&name).is_some_and(|r| r.touched && r.since > armed_at) {
            report.steps.push((
                "kill",
                name,
                Outcome::refused("used again since it was armed — spared"),
            ));
            return;
        }
        let outcome = self.act.kill(&name, w.det, w.watchlist, false);
        report.total_freed += outcome.bytes_freed;
        report.steps.push(("kill", name, outcome));
    }

    /// Killing is not reversible, so a judge that cannot be reached blocks it.
    fn do_kill(&mut self, w: &mut World<'_>, report: &mut Report) {
        // Already past the grace period: work through the queue, one per tick.
        if !self.kill_queue.is_empty() {
            self.kill_next(w, report);
            return;
        }

        let armed = self.pending_kill.is_some();
        let (targets, vetoed, unreachable) = if armed {
            (self.eligible_names(w), Vec::new(), None)
        } else {
            self.judged_names(w, Intent::Kill)
        };
        if let Some(e) = unreachable {
            self.pending_kill = None;
            report.steps.push((
                "kill",
                targets.join(", "),
                Outcome::refused(format!("judge unavailable ({e}) — not killing without it")),
            ));
            return;
        }
        for (name, outcome) in vetoed {
            report.steps.push(("kill", name, outcome));
        }

        if targets.is_empty() {
            self.pending_kill = None;
            report.steps.push((
                "kill",
                String::new(),
                Outcome::refused("nothing is both idle and on the watchlist"),
            ));
            return;
        }

        let grace = Duration::from_secs(self.cfg.kill_grace_seconds);

        match &self.pending_kill {
            // Already armed and the window has closed: proceed.
            Some(p) if p.due() => {
                let at = p.armed_at;
                self.kill_queue = p.targets.iter().map(|t| (t.clone(), at)).collect();
                self.pending_kill = None;
                self.kill_next(w, report);
            }
            // Armed, still inside the window.
            Some(p) => {
                report.steps.push((
                    "kill",
                    p.targets.join(", "),
                    Outcome::refused(format!("killing in {:.0}s", p.remaining().as_secs_f64())),
                ));
            }
            None => {
                let pending = PendingKill {
                    targets: targets.clone(),
                    armed_at: Instant::now(),
                    grace,
                };
                if pending.due() {
                    // grace of zero: no window, act now.
                    self.kill_queue = targets.into_iter().map(|t| (t, pending.armed_at)).collect();
                    self.kill_next(w, report);
                } else {
                    tracing::warn!(
                        "arming kill of {} in {}s — cancel to stop it",
                        targets.join(", "),
                        self.cfg.kill_grace_seconds
                    );
                    report.steps.push((
                        "kill",
                        targets.join(", "),
                        Outcome::refused(format!(
                            "armed, killing in {}s unless cancelled",
                            self.cfg.kill_grace_seconds
                        )),
                    ));
                    self.pending_kill = Some(pending);
                }
            }
        }
    }

    // ── De-escalation ───────────────────────────────────────────────────────

    /// Undo everything reversible, now that pressure has passed.
    ///
    /// A suspended GUI app is indistinguishable from a crashed one, and a soft
    /// cap left in place throttles an application for a reason that no longer
    /// exists. Neither should outlive the pressure that caused it.
    fn release(&mut self, w: &World<'_>, report: &mut Report) {
        for (name, outcome) in self.act.resume_all() {
            tracing::info!("resumed {name} — pressure back to {:.1}%", w.psi.some.avg10);
            report.steps.push(("resume", name, outcome));
        }
        // Everything on it was just resumed.
        self.undo.clear();
        if let Some(h) = w.hierarchy {
            for (name, outcome) in self.act.release_caps(h) {
                report.steps.push(("release", name, outcome));
            }
        }
    }

    fn log(&self, report: &Report, w: &World<'_>, rung: Rung) {
        let Some(h) = &self.history else { return };
        for (action, target, outcome) in &report.steps {
            let _ = h.log(&NewAction {
                rung: rung.index(),
                trigger: "psi".into(),
                psi_some: w.psi.some.avg10,
                psi_full: w.psi.full.avg10,
                action: (*action).to_string(),
                target: target.clone(),
                bytes_freed: outcome.bytes_freed as i64,
                succeeded: !outcome.did_nothing(),
                notes: outcome.notes.join("; "),
            });
        }
    }
}

/// Available memory in bytes, from `/proc/meminfo`.
///
/// `MemAvailable` rather than `MemFree`: the kernel's own estimate of what can
/// be allocated without swapping, which is the figure the last rung cares about.
pub fn available_bytes(root: &ramwarden_kernel::Root) -> u64 {
    let Ok(text) = std::fs::read_to_string(root.join("proc/meminfo")) else {
        return u64::MAX; // unknown: do not let a bad read trigger a kill
    };
    text.lines()
        .find_map(|l| {
            let rest = l.strip_prefix("MemAvailable:")?;
            rest.split_whitespace().next()?.parse::<u64>().ok()
        })
        .map(|kb| kb * 1024)
        .unwrap_or(u64::MAX)
}

/// Total PSS of every process the detector considers idle.
pub fn idle_bytes(det: &Detector) -> u64 {
    det.snapshot()
        .values()
        .filter(|s| s.verdict == Verdict::Idle)
        .map(|s| s.pss)
        .sum()
}

/// Bytes of cold memory in a process, for the targeted page-out rung.
pub fn cold_bytes(root: &ramwarden_kernel::Root, pid: i32) -> u64 {
    smaps::rollup(root, pid).map(|r| r.cold_bytes()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::roles::classify;
    use crate::signals::Signals;
    use ramwarden_kernel::Root;
    use ramwarden_kernel::psi::Window;
    use std::fs;

    fn cfg() -> config::Ladder {
        config::Ladder::default()
    }

    /// PSI with the given `some` and `full` ten-second averages.
    fn psi(some: f64, full: f64) -> Psi {
        Psi {
            some: Window {
                avg10: some,
                ..Default::default()
            },
            full: Window {
                avg10: full,
                ..Default::default()
            },
        }
    }

    const PLENTY: u64 = 8_000_000_000;

    // ── Rung selection, driven by a synthetic pressure series ───────────────

    #[test]
    fn the_pressure_series_climbs_the_rungs_in_order() {
        let c = cfg();
        let series = [
            (0.0, Rung::Release),
            (0.5, Rung::Release),
            (1.0, Rung::Release),  // release_below is inclusive
            (1.5, Rung::Hold),     // the hysteresis band
            (2.0, Rung::Reclaim),
            (4.9, Rung::Reclaim),
            (5.0, Rung::PageOut),
            (9.9, Rung::PageOut),
            (10.0, Rung::Tabs),
            (14.9, Rung::Tabs),
            (15.0, Rung::Suspend),
            (99.0, Rung::Suspend), // `some` alone never reaches Kill
        ];
        for (pressure, expected) in series {
            assert_eq!(
                rung_for(&c, &psi(pressure, 0.0), PLENTY),
                expected,
                "some avg10 = {pressure}"
            );
        }
    }

    /// The band between releasing and acting exists so a machine hovering at the
    /// threshold does not suspend and resume the same app every tick.
    #[test]
    fn the_hysteresis_band_neither_acts_nor_undoes() {
        let c = cfg();
        for p in [1.01, 1.5, 1.99] {
            assert_eq!(rung_for(&c, &psi(p, 0.0), PLENTY), Rung::Hold, "{p}");
        }
    }

    /// `full` means nothing is getting work done — thrashing, not merely busy.
    #[test]
    fn sustained_full_stall_reaches_the_kill_rung_on_its_own() {
        let c = cfg();
        assert_eq!(rung_for(&c, &psi(1.0, 25.0), PLENTY), Rung::Kill);
        assert_eq!(rung_for(&c, &psi(0.0, 99.0), PLENTY), Rung::Kill);
    }

    /// By the time thrashing registers in a ten-second average it is late, so
    /// running out of available memory is a trigger by itself.
    #[test]
    fn exhausted_available_memory_reaches_the_kill_rung_regardless_of_pressure() {
        let c = cfg();
        assert_eq!(rung_for(&c, &psi(0.0, 0.0), 400_000_000), Rung::Kill);
        assert_eq!(rung_for(&c, &psi(0.0, 0.0), 499_000_000), Rung::Kill);
        assert_eq!(rung_for(&c, &psi(0.0, 0.0), 501_000_000), Rung::Release);
    }

    #[test]
    fn a_retuned_ladder_shifts_the_thresholds() {
        let mut c = cfg();
        c.reclaim_at = 20.0;
        c.pageout_at = 30.0;
        c.tabs_at = 40.0;
        c.suspend_at = 50.0;
        assert!(c.validate().is_ok());
        assert_eq!(rung_for(&c, &psi(10.0, 0.0), PLENTY), Rung::Hold);
        assert_eq!(rung_for(&c, &psi(25.0, 0.0), PLENTY), Rung::Reclaim);
        assert_eq!(rung_for(&c, &psi(55.0, 0.0), PLENTY), Rung::Suspend);
    }

    #[test]
    fn rungs_are_ordered_so_escalation_is_a_simple_comparison() {
        assert!(Rung::Release < Rung::Hold);
        assert!(Rung::Hold < Rung::Reclaim);
        assert!(Rung::Reclaim < Rung::PageOut);
        assert!(Rung::PageOut < Rung::Tabs);
        assert!(Rung::Tabs < Rung::Suspend);
        assert!(Rung::Suspend < Rung::Kill);
    }

    // ── A synthetic session to act on ───────────────────────────────────────

    struct Fixture {
        _dir: tempfile::TempDir,
        root: Root,
    }

    impl Fixture {
        /// A session with three scopes: an idle browser, a scope containing the
        /// compositor, and a Docker container outside the session entirely.
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let root = Root::at(dir.path());
            let base = root
                .cgroup_fs()
                .join("user.slice/user-1000.slice/user@1000.service");
            fs::create_dir_all(&base).unwrap();
            fs::write(base.join("cgroup.controllers"), "cpu memory pids\n").unwrap();
            fs::write(base.join("cgroup.procs"), "").unwrap();
            fs::write(base.join("memory.current"), "8000000000\n").unwrap();

            let app = base.join("app.slice");
            fs::create_dir_all(&app).unwrap();
            fs::write(app.join("cgroup.procs"), "").unwrap();

            let mk = |name: &str, current: u64, pids: &[i32]| {
                let d = app.join(name);
                fs::create_dir_all(&d).unwrap();
                fs::write(d.join("memory.current"), format!("{current}\n")).unwrap();
                fs::write(d.join("memory.high"), "max\n").unwrap();
                fs::write(d.join("memory.reclaim"), "").unwrap();
                fs::write(
                    d.join("memory.stat"),
                    format!(
                        "anon {}\nfile {}\ninactive_anon {}\nactive_anon {}\n\
                         inactive_file {}\nactive_file {}\nunevictable 0\n",
                        current / 2,
                        current / 4,
                        current / 2,
                        current / 8,
                        current / 8,
                        current / 8
                    ),
                )
                .unwrap();
                fs::write(
                    d.join("cgroup.procs"),
                    pids.iter().map(|p| format!("{p}\n")).collect::<String>(),
                )
                .unwrap();
                for pid in pids {
                    let pd = root.join(format!("proc/{pid}"));
                    fs::create_dir_all(&pd).unwrap();
                    fs::write(pd.join("comm"), "x\n").unwrap();
                    fs::write(
                        pd.join("smaps_rollup"),
                        "0-1 ---p 0 00:00 0 [rollup]\nRss: 100 kB\nPss: 100 kB\n",
                    )
                    .unwrap();
                }
            };

            mk("app-brave.scope", 4_000_000_000, &[10, 11]);
            mk("app-compositor.scope", 1_500_000_000, &[20]);
            mk("app-tiny.scope", 1_000_000, &[30]);

            // Outside the user session: a container. Must never be reachable.
            let sys = root.cgroup_fs().join("system.slice/docker-chimera.scope");
            fs::create_dir_all(&sys).unwrap();
            fs::write(sys.join("cgroup.procs"), "99\n").unwrap();
            fs::write(sys.join("memory.current"), "43109120\n").unwrap();
            fs::write(sys.join("memory.reclaim"), "").unwrap();
            fs::write(
                sys.join("memory.stat"),
                "anon 1\nfile 1\ninactive_anon 999999999\ninactive_file 1\n\
                 active_anon 1\nactive_file 1\nunevictable 0\n",
            )
            .unwrap();

            fs::write(
                root.join("proc/meminfo"),
                "MemTotal:       32000000 kB\nMemAvailable:    7000000 kB\n",
            )
            .unwrap();

            Fixture { _dir: dir, root }
        }

        fn hierarchy(&self) -> cgroup::Hierarchy {
            cgroup::Hierarchy::user_session(&self.root, 1000).unwrap()
        }

        /// Browser pids idle, compositor pid protected, container pid idle.
        fn detector(&self, warm: bool) -> Detector {
            let mut sigs = Vec::new();
            for (pid, name) in [(10, "brave"), (11, "brave"), (30, "Discord")] {
                let mut s = Signals::new(pid, name, classify(name, "", None));
                s.pss = 500 * 1024 * 1024;
                s.age_minutes = 600.0;
                sigs.push(s);
            }
            // The compositor: structurally protected.
            let mut comp = Signals::new(20, "cosmic-comp", classify("cosmic-comp", "", None));
            comp.pss = 1_500 * 1024 * 1024;
            comp.age_minutes = 600.0;
            sigs.push(comp);
            // A container process, idle, but outside the session.
            let mut c = Signals::new(99, "Discord", classify("Discord", "", None));
            c.pss = 500 * 1024 * 1024;
            c.age_minutes = 600.0;
            sigs.push(c);

            Detector::preloaded(self.root.clone(), warm, sigs)
        }

        /// What was written to each scope's `memory.reclaim`.
        fn reclaim_requests(&self) -> Vec<(String, String)> {
            let app = self
                .root
                .cgroup_fs()
                .join("user.slice/user-1000.slice/user@1000.service/app.slice");
            let mut out = Vec::new();
            for e in fs::read_dir(&app).unwrap().flatten() {
                let f = e.path().join("memory.reclaim");
                if let Ok(body) = fs::read_to_string(&f)
                    && !body.is_empty()
                {
                    out.push((e.file_name().to_string_lossy().into_owned(), body));
                }
            }
            // Also check the container, which must never have been written to.
            let sys = self
                .root
                .cgroup_fs()
                .join("system.slice/docker-chimera.scope/memory.reclaim");
            if let Ok(body) = fs::read_to_string(&sys)
                && !body.is_empty()
            {
                out.push(("docker-chimera.scope".into(), body));
            }
            out.sort();
            out
        }
    }

    fn ladder(cfg: config::Ladder, root: &Root) -> Ladder {
        Ladder::new(cfg, Actuator::new(root.clone()))
            .with_history(History::in_memory().unwrap())
    }

    struct FakeTabs {
        closed: u32,
    }

    impl TabCloser for FakeTabs {
        fn close_stale(&mut self, _goal: &str) -> Outcome {
            self.closed += 1;
            Outcome {
                affected: vec![1, 2],
                bytes_freed: 50_000_000,
                notes: vec![],
            }
        }
    }

    fn world<'a>(
        f: &'a Fixture,
        h: &'a cgroup::Hierarchy,
        det: &'a Detector,
        pressure: f64,
        full: f64,
        watchlist: &'a [String],
    ) -> World<'a> {
        World {
            det,
            hierarchy: Some(h),
            psi: psi(pressure, full),
            available_bytes: available_bytes(&f.root),
            watchlist,
            goal: String::new(),
            tabs: None,
            judge: None,
        }
    }

    // ── Reclaim targeting and the safety invariants ─────────────────────────

    /// The invariant the whole design rests on: nothing outside the user's own
    /// session is ever a target, so Docker and chimera are unreachable.
    #[test]
    fn nothing_outside_the_user_session_is_ever_touched() {
        let f = Fixture::new();
        let h = f.hierarchy();
        let det = f.detector(true);
        let wl = vec!["Discord".to_string()];
        let mut l = ladder(cfg(), &f.root);

        // Run every acting rung.
        for pressure in [2.0, 5.0, 10.0, 15.0] {
            let mut w = world(&f, &h, &det, pressure, 0.0, &wl);
            l.step(&mut w);
        }

        let touched = f.reclaim_requests();
        assert!(
            !touched.iter().any(|(name, _)| name.contains("docker")),
            "a container scope was written to: {touched:?}"
        );
    }

    /// Reclaim is cheap, but asking the kernel to page out the compositor's cold
    /// memory buys a stutter for almost nothing.
    #[test]
    fn a_scope_containing_a_protected_process_is_never_reclaimed_from() {
        let f = Fixture::new();
        let h = f.hierarchy();
        let det = f.detector(true);
        let wl: Vec<String> = vec![];
        let mut l = ladder(cfg(), &f.root);

        let mut w = world(&f, &h, &det, 3.0, 0.0, &wl);
        l.step(&mut w);

        let touched = f.reclaim_requests();
        assert!(
            !touched.iter().any(|(n, _)| n.contains("compositor")),
            "{touched:?}"
        );
        assert!(
            touched.iter().any(|(n, _)| n.contains("brave")),
            "the idle browser should have been reclaimed from: {touched:?}"
        );
    }

    #[test]
    fn a_scope_too_small_to_matter_is_skipped() {
        let f = Fixture::new();
        let h = f.hierarchy();
        let det = f.detector(true);
        let mut l = ladder(cfg(), &f.root);
        let mut w = world(&f, &h, &det, 3.0, 0.0, &[]);
        l.step(&mut w);
        assert!(
            !f.reclaim_requests().iter().any(|(n, _)| n.contains("tiny")),
            "a 1 MB scope is not worth a syscall"
        );
    }

    #[test]
    fn reclaim_asks_for_a_fraction_rather_than_everything_cold() {
        let f = Fixture::new();
        let h = f.hierarchy();
        let det = f.detector(true);
        let mut l = ladder(cfg(), &f.root);
        let mut w = world(&f, &h, &det, 3.0, 0.0, &[]);
        l.step(&mut w);

        let reqs = f.reclaim_requests();
        let (_, asked) = reqs.iter().find(|(n, _)| n.contains("brave")).unwrap();
        let asked: u64 = asked.parse().unwrap();
        // inactive_anon + inactive_file = 4e9/2 + 4e9/8 = 2.5e9; two thirds of that.
        let reclaimable = 4_000_000_000u64 / 2 + 4_000_000_000u64 / 8;
        assert_eq!(asked, (reclaimable as f64 * RECLAIM_FRACTION) as u64);
        assert!(asked < reclaimable, "must not ask for all of it");
    }

    #[test]
    fn browser_batches_wait_for_pressure_to_settle() {
        let f=Fixture::new();let h=f.hierarchy();let det=f.detector(true);
        let mut tabs=FakeTabs { closed: 0 };let mut l=ladder(cfg(),&f.root);
        let mut w=World { tabs:Some(&mut tabs),..world(&f,&h,&det,11.0,0.0,&[]) };
        let first=l.step(&mut w);let second=l.step(&mut w);
        assert!(first.steps.iter().any(|(a,_,_)|*a=="discard_tab"));
        assert!(!second.steps.iter().any(|(a,_,_)|*a=="discard_tab"));
        l.last_tabs=Some(Instant::now()-Duration::from_secs(31));
        assert!(l.step(&mut w).steps.iter().any(|(a,_,_)|*a=="discard_tab"));
        assert_eq!(tabs.closed,2);
    }

    // ── Cumulative escalation ───────────────────────────────────────────────

    #[test]
    fn reaching_a_high_rung_also_runs_the_cheaper_ones() {
        let f = Fixture::new();
        let h = f.hierarchy();
        let det = f.detector(true);
        let wl = vec!["Discord".to_string()];
        let mut tabs = FakeTabs { closed: 0 };
        let mut l = ladder(cfg(), &f.root);

        let mut w = World {
            tabs: Some(&mut tabs),
            judge: None,
            ..world(&f, &h, &det, 16.0, 0.0, &wl)
        };
        let report = l.step(&mut w);

        assert_eq!(report.rung, Some(Rung::Suspend));
        let actions: Vec<&str> = report.steps.iter().map(|(a, _, _)| *a).collect();
        assert!(actions.contains(&"reclaim"), "{actions:?}");
        assert!(actions.contains(&"soft_cap"), "{actions:?}");
        assert!(actions.contains(&"discard_tab"), "{actions:?}");
        assert!(actions.contains(&"suspend"), "{actions:?}");
        assert_eq!(tabs.closed, 1);
    }

    #[test]
    fn the_lowest_rung_does_not_run_the_expensive_ones() {
        let f = Fixture::new();
        let h = f.hierarchy();
        let det = f.detector(true);
        let wl = vec!["Discord".to_string()];
        let mut l = ladder(cfg(), &f.root);
        let mut w = world(&f, &h, &det, 2.5, 0.0, &wl);
        let report = l.step(&mut w);

        assert_eq!(report.rung, Some(Rung::Reclaim));
        let actions: Vec<&str> = report.steps.iter().map(|(a, _, _)| *a).collect();
        assert!(actions.contains(&"reclaim"));
        assert!(!actions.contains(&"suspend"), "{actions:?}");
        assert!(!actions.contains(&"kill"), "{actions:?}");
    }

    // ── Fail-closed ─────────────────────────────────────────────────────────

    /// Before two samples there is no CPU delta, so nothing can honestly be
    /// called idle and the ladder must not act on anything.
    #[test]
    fn a_cold_detector_stops_the_ladder_from_acting() {
        let f = Fixture::new();
        let h = f.hierarchy();
        let det = f.detector(false);
        let wl = vec!["Discord".to_string()];
        let mut l = ladder(cfg(), &f.root);

        let mut w = world(&f, &h, &det, 30.0, 30.0, &wl);
        let report = l.step(&mut w);

        assert!(f.reclaim_requests().is_empty(), "nothing should be reclaimed");
        assert!(
            report.steps[0].2.notes[0].contains("warming up"),
            "{:?}",
            report.steps
        );
    }

    /// Undoing is always safe, so release is allowed even while cold.
    #[test]
    fn release_still_runs_while_the_detector_is_cold() {
        let f = Fixture::new();
        let h = f.hierarchy();
        let det = f.detector(false);
        let mut l = ladder(cfg(), &f.root);
        let mut w = world(&f, &h, &det, 0.0, 0.0, &[]);
        let report = l.step(&mut w);
        assert_eq!(report.rung, Some(Rung::Release));
        assert!(
            !report.steps.iter().any(|(_, _, o)| o
                .notes
                .iter()
                .any(|n| n.contains("warming up"))),
            "{:?}",
            report.steps
        );
    }

    #[test]
    fn without_a_delegated_memory_controller_the_kernel_rungs_are_skipped() {
        let f = Fixture::new();
        let h = f.hierarchy();
        let det = f.detector(true);
        let mut l = ladder(cfg(), &f.root);
        let mut w = World {
            hierarchy: None,
            ..world(&f, &h, &det, 3.0, 0.0, &[])
        };
        let report = l.step(&mut w);
        let note = &report.steps[0].2.notes[0];
        assert!(note.contains("delegated cgroup"), "{note}");
    }

    #[test]
    fn with_no_browser_connected_the_tab_rung_says_so() {
        let f = Fixture::new();
        let h = f.hierarchy();
        let det = f.detector(true);
        let mut l = ladder(cfg(), &f.root);
        let mut w = world(&f, &h, &det, 11.0, 0.0, &[]);
        let report = l.step(&mut w);
        assert!(
            report
                .steps
                .iter()
                .any(|(a, _, o)| *a == "discard_tab"
                    && o.notes.iter().any(|n| n.contains("no browser connected"))),
            "{:?}",
            report.steps
        );
    }

    // ── The kill rung ───────────────────────────────────────────────────────

    /// Autonomous, but observable: the first tick at kill pressure arms a
    /// countdown rather than acting, and the kill proceeds by default.
    #[test]
    fn the_kill_rung_arms_a_cancellable_countdown_before_acting() {
        let f = Fixture::new();
        let h = f.hierarchy();
        let det = f.detector(true);
        let wl = vec!["Discord".to_string()];
        let mut l = ladder(cfg(), &f.root);

        let mut w = world(&f, &h, &det, 1.0, 30.0, &wl);
        let report = l.step(&mut w);

        assert_eq!(report.rung, Some(Rung::Kill));
        let (_, target, outcome) = report
            .steps
            .iter()
            .find(|(a, _, _)| *a == "kill")
            .expect("a kill step");
        assert!(target.contains("Discord"));
        assert!(outcome.notes[0].contains("armed"), "{outcome:?}");
        assert!(l.pending_kill().is_some(), "the kill must be pending");
        assert!(!l.pending_kill().unwrap().due(), "grace has not elapsed");
    }

    #[test]
    fn an_armed_kill_can_be_cancelled() {
        let f = Fixture::new();
        let h = f.hierarchy();
        let det = f.detector(true);
        let wl = vec!["Discord".to_string()];
        let mut l = ladder(cfg(), &f.root);

        let mut w = world(&f, &h, &det, 1.0, 30.0, &wl);
        l.step(&mut w);
        assert!(l.cancel_kill(), "cancel should report that it cancelled");
        assert!(l.pending_kill().is_none());
        assert!(!l.cancel_kill(), "a second cancel has nothing to cancel");
    }

    /// Pressure passing must stand the kill down rather than leaving it armed.
    #[test]
    fn dropping_below_the_kill_rung_stands_the_kill_down() {
        let f = Fixture::new();
        let h = f.hierarchy();
        let det = f.detector(true);
        let wl = vec!["Discord".to_string()];
        let mut l = ladder(cfg(), &f.root);

        l.step(&mut world(&f, &h, &det, 1.0, 30.0, &wl));
        assert!(l.pending_kill().is_some());

        // Still under pressure, but no longer thrashing.
        l.step(&mut world(&f, &h, &det, 16.0, 0.0, &wl));
        assert!(l.pending_kill().is_none(), "the kill should have stood down");
    }

    #[test]
    fn the_hold_band_also_stands_a_pending_kill_down() {
        let f = Fixture::new();
        let h = f.hierarchy();
        let det = f.detector(true);
        let wl = vec!["Discord".to_string()];
        let mut l = ladder(cfg(), &f.root);
        l.step(&mut world(&f, &h, &det, 1.0, 30.0, &wl));
        l.step(&mut world(&f, &h, &det, 1.5, 0.0, &wl));
        assert!(l.pending_kill().is_none());
    }

    /// `kill_grace_seconds = 0` means no window at all — which is the setting
    /// for someone who wants the ladder to act without hesitation.
    #[test]
    fn a_zero_grace_kills_on_the_first_tick_with_no_window() {
        let f = Fixture::new();
        let h = f.hierarchy();
        let det = f.detector(true);
        let wl = vec!["Discord".to_string()];
        let mut c = cfg();
        c.kill_grace_seconds = 0;
        let mut l = ladder(c, &f.root);

        let report = l.step(&mut world(&f, &h, &det, 1.0, 30.0, &wl));
        let (_, _, outcome) = report.steps.iter().find(|(a, _, _)| *a == "kill").unwrap();
        assert!(
            !outcome.notes.iter().any(|n| n.contains("armed")),
            "{outcome:?}"
        );
        assert!(l.pending_kill().is_none(), "nothing left pending");
    }

    /// Nothing idle and watchlisted means nothing to kill — the ladder says so
    /// rather than reaching for something it was never allowed to touch.
    #[test]
    fn the_kill_rung_refuses_when_nothing_is_eligible() {
        let f = Fixture::new();
        let h = f.hierarchy();
        let det = f.detector(true);
        let mut l = ladder(cfg(), &f.root);

        let report = l.step(&mut world(&f, &h, &det, 1.0, 30.0, &[]));
        let (_, _, outcome) = report.steps.iter().find(|(a, _, _)| *a == "kill").unwrap();
        assert!(
            outcome.notes[0].contains("nothing is both idle and on the watchlist"),
            "{outcome:?}"
        );
        assert!(l.pending_kill().is_none());
    }

    /// At no rung, including the last, may a protected process be a target.
    #[test]
    fn the_compositor_is_never_a_target_at_any_rung() {
        let f = Fixture::new();
        let h = f.hierarchy();
        let det = f.detector(true);
        // A watchlist that names the compositor outright.
        let wl = vec!["cosmic-comp".to_string(), "Discord".to_string()];
        let mut c = cfg();
        c.kill_grace_seconds = 0;
        let mut l = ladder(c, &f.root);

        for (some, full) in [(2.0, 0.0), (6.0, 0.0), (11.0, 0.0), (16.0, 0.0), (1.0, 30.0)] {
            let report = l.step(&mut world(&f, &h, &det, some, full, &wl));
            for (action, target, _) in &report.steps {
                assert!(
                    !target.contains("cosmic-comp") && !target.contains("compositor"),
                    "{action} targeted the compositor at some={some} full={full}"
                );
            }
        }
    }

    // ── De-escalation ───────────────────────────────────────────────────────

    #[test]
    fn release_lifts_every_soft_cap_the_ladder_set() {
        let f = Fixture::new();
        let h = f.hierarchy();
        let det = f.detector(true);
        let mut l = ladder(cfg(), &f.root);

        // Climb to the soft-cap rung, then drop to nothing.
        l.step(&mut world(&f, &h, &det, 6.0, 0.0, &[]));
        assert!(l.actuator().has_caps(), "a cap should have been set");

        let report = l.step(&mut world(&f, &h, &det, 0.0, 0.0, &[]));
        assert_eq!(report.rung, Some(Rung::Release));
        assert!(!l.actuator().has_caps(), "every cap must be lifted");
        assert!(
            report.steps.iter().any(|(a, _, _)| *a == "release"),
            "{:?}",
            report.steps
        );
    }

    #[test]
    fn a_soft_cap_is_set_below_the_scopes_current_charge() {
        let f = Fixture::new();
        let h = f.hierarchy();
        let det = f.detector(true);
        let mut l = ladder(cfg(), &f.root);
        l.step(&mut world(&f, &h, &det, 6.0, 0.0, &[]));

        let scope = h.scope("app-brave.scope").unwrap();
        let cap = scope.high().unwrap().expect("capped");
        assert_eq!(cap, (4_000_000_000f64 * SOFT_CAP_FRACTION) as u64);
        assert!(cap < scope.current().unwrap());
    }

    #[test]
    fn holding_does_nothing_at_all() {
        let f = Fixture::new();
        let h = f.hierarchy();
        let det = f.detector(true);
        let mut l = ladder(cfg(), &f.root);
        let report = l.step(&mut world(&f, &h, &det, 1.5, 0.0, &[]));
        assert_eq!(report.rung, Some(Rung::Hold));
        assert!(report.steps.is_empty());
        assert!(!report.did_something());
        assert!(f.reclaim_requests().is_empty());
    }

    // ── Logging ─────────────────────────────────────────────────────────────

    /// The record that makes autonomous operation answerable afterwards.
    #[test]
    fn every_action_is_logged_with_the_pressure_that_caused_it() {
        let f = Fixture::new();
        let h = f.hierarchy();
        let det = f.detector(true);
        let mut l = ladder(cfg(), &f.root);

        l.step(&mut world(&f, &h, &det, 6.5, 0.0, &[]));

        let rows = l.history().unwrap().recent_actions(20).unwrap();
        assert!(!rows.is_empty(), "the ladder logged nothing");

        let reclaim = rows
            .iter()
            .find(|r| r.action == "reclaim")
            .expect("a reclaim row");
        assert_eq!(reclaim.psi_some, 6.5, "the pressure at decision time");
        assert_eq!(reclaim.rung, Rung::PageOut.index());
        assert_eq!(reclaim.trigger, "psi");
        assert!(reclaim.target.contains('x') || !reclaim.target.is_empty());
    }

    /// A refusal is as worth recording as an action: "RamWarden did nothing and
    /// here is why" answers most questions about unattended behaviour.
    #[test]
    fn a_refused_rung_is_logged_too() {
        let f = Fixture::new();
        let h = f.hierarchy();
        let det = f.detector(true);
        let mut l = ladder(cfg(), &f.root);

        // The tab rung with no browser attached refuses.
        l.step(&mut world(&f, &h, &det, 11.0, 0.0, &[]));

        let rows = l.history().unwrap().recent_actions(20).unwrap();
        let tab = rows.iter().find(|r| r.action == "discard_tab").unwrap();
        assert!(!tab.succeeded);
        assert!(tab.notes.contains("no browser connected"), "{}", tab.notes);
    }

    #[test]
    fn a_ladder_without_history_still_works() {
        let f = Fixture::new();
        let h = f.hierarchy();
        let det = f.detector(true);
        let mut l = Ladder::new(cfg(), Actuator::new(f.root.clone()));
        let report = l.step(&mut world(&f, &h, &det, 3.0, 0.0, &[]));
        assert_eq!(report.rung, Some(Rung::Reclaim));
    }

    // ── Planning ────────────────────────────────────────────────────────────

    #[test]
    fn a_plan_describes_the_rung_without_acting() {
        let f = Fixture::new();
        let h = f.hierarchy();
        let det = f.detector(true);
        let wl = vec!["Discord".to_string()];
        let l = ladder(cfg(), &f.root);

        let plan = l.plan(&world(&f, &h, &det, 16.0, 0.0, &wl));
        assert_eq!(plan.rung, Some(Rung::Suspend));
        assert!(plan.reclaim.iter().any(|(n, _)| !n.is_empty()));
        assert!(plan.soft_cap.is_some());
        assert_eq!(plan.suspend, vec!["Discord"]);
        assert!(plan.kill.is_empty(), "not at the kill rung");

        assert!(
            f.reclaim_requests().is_empty(),
            "planning must not touch the kernel"
        );
        assert!(!l.actuator_has_caps(), "planning must not set a cap");
    }

    /// Rung 1 does two things, and the page-out half must degrade visibly
    /// rather than silently when the capability is absent.
    #[test]
    fn the_pageout_rung_reports_the_capability_it_is_missing() {
        let f = Fixture::new();
        let h = f.hierarchy();
        let det = f.detector(true);
        let mut l = ladder(cfg(), &f.root);

        let report = l.step(&mut world(&f, &h, &det, 6.0, 0.0, &[]));
        let pageout = report.steps.iter().find(|(a, _, _)| *a == "pageout");

        match pageout {
            Some((_, _, o)) if o.did_nothing() => assert!(
                !o.notes.is_empty(),
                "a refused page-out must say what it needs"
            ),
            // With the capability granted it may genuinely do nothing useful
            // against a synthetic /proc; either way it must not claim success
            // it did not have.
            Some(_) => {}
            None => panic!("the pageout rung did not run: {:?}", report.steps),
        }
    }

    #[test]
    fn a_plan_at_the_hold_rung_is_empty() {
        let f = Fixture::new();
        let h = f.hierarchy();
        let det = f.detector(true);
        let l = ladder(cfg(), &f.root);
        let plan = l.plan(&world(&f, &h, &det, 1.5, 0.0, &[]));
        assert_eq!(plan.rung, Some(Rung::Hold));
        assert!(plan.reclaim.is_empty());
        assert!(plan.suspend.is_empty());
        assert!(plan.blocked.is_none());
    }

    #[test]
    fn a_plan_says_why_it_is_blocked_while_the_detector_is_cold() {
        let f = Fixture::new();
        let h = f.hierarchy();
        let det = f.detector(false);
        let l = ladder(cfg(), &f.root);
        let plan = l.plan(&world(&f, &h, &det, 20.0, 0.0, &[]));
        assert!(plan.blocked.unwrap().contains("warming up"));
    }

    #[test]
    fn a_plan_never_names_a_protected_scope() {
        let f = Fixture::new();
        let h = f.hierarchy();
        let det = f.detector(true);
        let wl = vec!["cosmic-comp".to_string(), "Discord".to_string()];
        let l = ladder(cfg(), &f.root);

        let plan = l.plan(&world(&f, &h, &det, 1.0, 30.0, &wl));
        assert_eq!(plan.rung, Some(Rung::Kill));
        for (name, _) in &plan.reclaim {
            assert!(!name.contains("cosmic"), "{name}");
        }
        assert!(!plan.suspend.iter().any(|n| n.contains("cosmic")));
        assert!(!plan.kill.iter().any(|n| n.contains("cosmic")));
    }

    #[test]
    fn a_plan_matches_what_a_step_then_does() {
        let f = Fixture::new();
        let h = f.hierarchy();
        let det = f.detector(true);
        let mut l = ladder(cfg(), &f.root);

        let plan = l.plan(&world(&f, &h, &det, 3.0, 0.0, &[]));
        let planned: Vec<&str> = plan.reclaim.iter().map(|(n, _)| n.as_str()).collect();

        let report = l.step(&mut world(&f, &h, &det, 3.0, 0.0, &[]));
        let done: Vec<&str> = report
            .steps
            .iter()
            .filter(|(a, _, _)| *a == "reclaim")
            .map(|(_, t, _)| t.as_str())
            .collect();
        assert_eq!(planned, done);
    }

    // ── Helpers ─────────────────────────────────────────────────────────────

    #[test]
    fn available_memory_is_read_from_meminfo() {
        let f = Fixture::new();
        assert_eq!(available_bytes(&f.root), 7_000_000 * 1024);
    }

    /// An unreadable meminfo must not look like an out-of-memory condition and
    /// trigger the kill rung.
    #[test]
    fn unreadable_meminfo_reports_unlimited_rather_than_zero() {
        let dir = tempfile::tempdir().unwrap();
        let root = Root::at(dir.path());
        assert_eq!(available_bytes(&root), u64::MAX);
        assert_eq!(
            rung_for(&cfg(), &psi(0.0, 0.0), available_bytes(&root)),
            Rung::Release
        );
    }

    #[test]
    fn meminfo_without_the_available_field_reports_unlimited() {
        let dir = tempfile::tempdir().unwrap();
        let root = Root::at(dir.path());
        fs::create_dir_all(root.join("proc")).unwrap();
        fs::write(root.join("proc/meminfo"), "MemTotal: 32000000 kB\n").unwrap();
        assert_eq!(available_bytes(&root), u64::MAX);
    }

    #[test]
    fn idle_bytes_sums_only_what_the_detector_calls_idle() {
        let f = Fixture::new();
        let det = f.detector(true);
        // Three idle processes at 500 MB each; the compositor is excluded.
        assert_eq!(idle_bytes(&det), 4 * 500 * 1024 * 1024);
    }

    // ── LRU order, the judge, and the kill queue ────────────────────────────

    /// Two idle watchlisted apps; `hot` was focused at some point, the other never.
    fn lru_detector(f: &Fixture, hot: &str) -> Detector {
        let mut sigs = Vec::new();
        for (pid, name) in [(30, "Discord"), (31, "Slack")] {
            let mut s = Signals::new(pid, name, classify(name, "", None));
            s.pss = 500 * 1024 * 1024;
            s.age_minutes = 600.0;
            s.is_focused = name == hot;
            sigs.push(s);
        }
        Detector::preloaded(f.root.clone(), true, sigs)
    }

    struct FakeJudge {
        calls: Vec<(Intent, Vec<String>)>,
        allow: Vec<&'static str>,
        fail: bool,
    }

    impl Judge for FakeJudge {
        fn judge(&mut self, intent: Intent, c: &[Candidate]) -> Result<Vec<crate::judge::Judgement>, String> {
            self.calls.push((intent, c.iter().map(|c| c.name.clone()).collect()));
            if self.fail {
                return Err("timed out".into());
            }
            Ok(c.iter()
                .map(|c| crate::judge::Judgement {
                    name: c.name.clone(),
                    close: self.allow.contains(&c.name.as_str()),
                    confidence: 0.61,
                    reason: "Jev is not convinced (61%)".into(),
                })
                .collect())
        }
    }

    #[test]
    fn candidates_come_from_the_lru_tail_first() {
        let f = Fixture::new();
        let h = f.hierarchy();
        let wl = vec!["Discord".to_string(), "Slack".to_string()];
        let l = ladder(cfg(), &f.root);

        // The focused flag makes an app ineligible this tick, so check order on
        // a detector where both are idle but one was touched earlier.
        let mut det = lru_detector(&f, "Slack");
        let w = world(&f, &h, &det, 1.0, 30.0, &wl);
        assert_eq!(l.eligible_names(&w), vec!["Discord".to_string()]);

        det = lru_detector(&f, "none");
        let w = world(&f, &h, &det, 1.0, 30.0, &wl);
        let names = l.eligible_names(&w);
        assert_eq!(names.len(), 2);
        // Both cold and untouched: order follows the LRU, which is stable.
        let lru: Vec<String> = det.coldest().map(|(n, _)| n.to_string()).collect();
        assert_eq!(names, lru);
    }

    #[test]
    fn the_judge_sees_lru_candidates_and_its_veto_is_reported() {
        let f = Fixture::new();
        let h = f.hierarchy();
        let det = lru_detector(&f, "none");
        let wl = vec!["Discord".to_string(), "Slack".to_string()];
        let mut l = ladder(cfg(), &f.root);
        let mut j = FakeJudge { calls: vec![], allow: vec![], fail: false };

        let mut w = World { judge: Some(&mut j), ..world(&f, &h, &det, 1.0, 30.0, &wl) };
        let report = l.step(&mut w);

        assert_eq!(j.calls[0].0, Intent::Suspend);
        assert_eq!(j.calls[1].0, Intent::Kill);
        assert_eq!(j.calls[0].1.len(), 2);
        let vetoes: Vec<_> = report
            .steps
            .iter()
            .filter(|(_, _, o)| o.notes.iter().any(|n| n.contains("Jev is not convinced")))
            .collect();
        assert_eq!(vetoes.len(), 4, "{:?}", report.steps); // 2 apps x suspend + kill
        assert!(l.pending_kill().is_none(), "nothing approved, nothing armed");
    }

    #[test]
    fn only_what_the_judge_approves_is_armed() {
        let f = Fixture::new();
        let h = f.hierarchy();
        let det = lru_detector(&f, "none");
        let wl = vec!["Discord".to_string(), "Slack".to_string()];
        let mut l = ladder(cfg(), &f.root);
        let mut j = FakeJudge { calls: vec![], allow: vec!["Slack"], fail: false };

        l.step(&mut World { judge: Some(&mut j), ..world(&f, &h, &det, 1.0, 30.0, &wl) });
        assert_eq!(l.pending_kill().unwrap().targets, vec!["Slack".to_string()]);
    }

    #[test]
    fn an_unreachable_judge_blocks_kills_but_not_suspends() {
        let f = Fixture::new();
        let h = f.hierarchy();
        let det = lru_detector(&f, "none");
        let wl = vec!["Discord".to_string(), "Slack".to_string()];
        let mut l = ladder(cfg(), &f.root);
        let mut j = FakeJudge { calls: vec![], allow: vec![], fail: true };

        let report = l.step(&mut World { judge: Some(&mut j), ..world(&f, &h, &det, 1.0, 30.0, &wl) });
        let suspends = report.steps.iter().filter(|(a, _, _)| *a == "suspend").count();
        assert_eq!(suspends, 2, "suspend falls back to the gate: {:?}", report.steps);
        let (_, _, kill) = report.steps.iter().find(|(a, _, _)| *a == "kill").unwrap();
        assert!(kill.notes[0].contains("not killing without it"), "{kill:?}");
        assert!(l.pending_kill().is_none());
    }

    #[test]
    fn due_kills_leave_the_queue_one_per_tick_and_stop_when_pressure_drops() {
        let f = Fixture::new();
        let h = f.hierarchy();
        let det = lru_detector(&f, "none");
        let wl = vec!["Discord".to_string(), "Slack".to_string()];
        let mut c = cfg();
        c.kill_grace_seconds = 0;
        let mut l = ladder(c, &f.root);

        let report = l.step(&mut world(&f, &h, &det, 1.0, 30.0, &wl));
        let kills = report.steps.iter().filter(|(a, _, _)| *a == "kill").count();
        assert_eq!(kills, 1, "{:?}", report.steps);
        assert_eq!(l.queued_kills().len(), 1);

        // Pressure eases below the kill rung: the rest of the queue is spared.
        l.step(&mut world(&f, &h, &det, 16.0, 0.0, &wl));
        assert!(l.queued_kills().is_empty());
    }

    #[test]
    fn cancel_also_empties_the_kill_queue() {
        let f = Fixture::new();
        let h = f.hierarchy();
        let det = lru_detector(&f, "none");
        let wl = vec!["Discord".to_string(), "Slack".to_string()];
        let mut c = cfg();
        c.kill_grace_seconds = 0;
        let mut l = ladder(c, &f.root);
        l.step(&mut world(&f, &h, &det, 1.0, 30.0, &wl));
        assert!(l.cancel_kill());
        assert!(l.queued_kills().is_empty());
    }

    #[test]
    fn undo_skips_entries_that_are_no_longer_suspended() {
        let f = Fixture::new();
        let mut l = ladder(cfg(), &f.root);
        l.undo.push("Discord".into());
        l.undo.push("Slack".into());
        // Neither is actually suspended in the fixture.
        assert!(l.undo_last().is_none());
        assert!(l.undo_stack().is_empty());
    }
}
