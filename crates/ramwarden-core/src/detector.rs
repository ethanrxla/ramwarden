//! The activity detector: collect signals once per tick, score every process,
//! and answer the one question the ladder depends on — may this be reclaimed?
//!
//! # The gate
//!
//! [`Detector::may_suspend`] is the single checkpoint every destructive action
//! passes through. Its contract, inherited from v1 and worth restating because
//! the autonomous ladder leans on it entirely:
//!
//! * **Structural protection is absolute.** Checked first, before the watchlist,
//!   so the refusal names the real reason rather than "not on the list".
//! * **Dynamic detection only ever narrows.** A process must be on the
//!   user's watchlist to be touchable at all; signals decide what to *spare*,
//!   never what to add.
//! * **It fails closed.** Before two samples exist there is no CPU delta, so
//!   nothing can honestly be called idle and everything is refused.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use ramwarden_platform::{Instances, Owner, ProcessSample, ProcessSource};

use crate::pattern;
use crate::roles::classify;
use crate::signals::{Protection, Signals, Verdict};
use crate::structures::Lru;
use crate::structures::circular::has_cycle;

/// Signals that do not come from the process table — audio, windows, focus.
/// Supplied separately so tests can hand them in directly.
pub use ramwarden_platform::Desktop;

/// The outcome of asking the gate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Permitted. Carries the justification, which is logged and shown — a
    /// permitted suspend of a serving process still warns about the sockets.
    Allow(String),
    /// Refused, with the reason the user should see.
    Refuse(String),
}

impl Decision {
    pub fn is_allowed(&self) -> bool {
        matches!(self, Decision::Allow(_))
    }

    pub fn reason(&self) -> &str {
        match self {
            Decision::Allow(r) | Decision::Refuse(r) => r,
        }
    }
}

/// Scores every process from live system signals.
pub struct Detector {
    /// Where process facts come from: `/proc` on Linux, the NT process table on
    /// Windows. The detector itself only interprets them.
    source: Box<dyn ProcessSource>,
    /// The source's last [`ProcessSource::accounting_degraded`].
    degraded: bool,
    signals: HashMap<i32, Signals>,
    /// Last tick's map, emptied, kept for its capacity.
    spare: HashMap<i32, Signals>,
    /// Per process instance: cumulative CPU at the previous sample (the CPU
    /// signal is a difference, which is the whole reason
    /// [`Detector::is_warm`] exists), and what it was classified from.
    instances: Instances<Instance>,
    ticks: u32,
    our_pid: i32,
    /// pid -> parent pid and the focused pid at the last sample, so "is this
    /// process part of the app the user is looking at?" needs no new syscalls.
    parents: HashMap<i32, i32>,
    focused: Option<i32>,
    /// When any process of an app last showed real activity (focus, audio, CPU or
    /// a working child). An app first seen is dated from that sighting, so idle
    /// time is only known since the daemon started.
    app_active: HashMap<String, Instant>,
    /// Since when an app has been using CPU while nobody is using it: not in
    /// front, silent, no working child. A runaway viewer looks like this, and so
    /// does a long legitimate job, so this only measures; Jev judges.
    app_busy_since: HashMap<String, Instant>,
    /// Application names, most recently used first. See [`Recency`].
    recency: Lru<String, Recency>,
}

/// When the user last used an application, as opposed to when it last ran.
///
/// Only user-facing signals count as a touch: focus, audio, and having just
/// been launched. CPU time and network traffic deliberately do not. Chat apps
/// wake up for background sync every few minutes, and counting that as use
/// made them look permanently fresh — their "quiet" timer kept resetting to
/// zero and they could never reach the tail.
#[derive(Clone, Copy, Debug)]
pub struct Recency {
    /// When this name was last touched, or first seen if never.
    pub since: Instant,
    /// Whether it has been touched at all since the daemon started. An
    /// untouched entry's age is a lower bound.
    pub touched: bool,
}

impl Recency {
    pub fn idle(&self) -> Duration {
        self.since.elapsed()
    }
}

/// What the detector remembers about one process instance between ticks.
struct Instance {
    cpu_total: f64,
    /// What its role, app and part were classified from. A change to either
    /// means classifying again.
    owner: Owner,
    cmdline: std::sync::Arc<str>,
}

/// One application's activity in one tick.
#[derive(Default)]
struct AppTick {
    used: bool,
    cpu: bool,
}

/// A source with nothing in it, for detectors that are only ever preloaded.
struct NoProcesses;

impl ProcessSource for NoProcesses {
    fn sample(&mut self) -> ramwarden_platform::Result<Vec<ProcessSample>> {
        Ok(Vec::new())
    }
}

impl Detector {
    pub fn new(source: Box<dyn ProcessSource>) -> Self {
        Detector {
            source,
            degraded: false,
            signals: HashMap::new(),
            spare: HashMap::new(),
            instances: Instances::new(),
            ticks: 0,
            our_pid: std::process::id() as i32,
            parents: HashMap::new(),
            focused: None,
            app_active: HashMap::new(),
            app_busy_since: HashMap::new(),
            recency: Lru::new(),
        }
    }

    /// A detector over a real or synthetic `/proc` tree.
    #[cfg(target_os = "linux")]
    pub fn linux(root: ramwarden_kernel::Root) -> Self {
        Detector::new(Box::new(ramwarden_kernel::platform::LinuxProcesses::new(root)))
    }

    /// Whether proportional accounting (PSS, private working set) is working.
    ///
    /// False means accounting has degraded to summed resident size and
    /// over-reports any process that shares pages — which is every browser.
    pub fn pss_available(&self) -> bool {
        !self.degraded
    }

    /// Hand the process source to the caller, leaving an empty one behind.
    ///
    /// For a caller that samples on its own thread and must not hold the
    /// detector's lock while it does: walking the process table is the slow
    /// part of a tick, and every reader of the detector (the HTTP API, the
    /// ladder) used to wait for it. Sample the returned source, then pass the
    /// table to [`Detector::ingest`] under the lock.
    pub fn take_source(&mut self) -> Box<dyn ProcessSource> {
        std::mem::replace(&mut self.source, Box::new(NoProcesses))
    }

    /// A detector holding hand-built signals, for tests and for replaying a
    /// captured snapshot. `warm` controls whether the CPU signal is trusted.
    pub fn preloaded(warm: bool, sigs: Vec<Signals>) -> Self {
        let mut d = Detector::new(Box::new(NoProcesses));
        d.ticks = if warm { 2 } else { 1 };
        for mut s in sigs {
            s.score(warm);
            d.signals.insert(s.pid, s);
        }
        d.update_recency();
        d
    }

    /// CPU deltas need two samples; before that, nothing can be called idle.
    pub fn is_warm(&self) -> bool {
        self.ticks >= 2
    }

    pub fn ticks(&self) -> u32 {
        self.ticks
    }

    /// Collect one sample and score everything.
    ///
    /// `desktop` carries the signals that require subprocesses or window-system
    /// calls; the caller refreshes it on its own slower cadence.
    ///
    /// # What is reused between ticks
    ///
    /// A process seen last tick keeps its [`Signals`] entry: its name, app and
    /// part strings, its reasons buffer, and its role are carried over, and
    /// only the per-tick facts are overwritten. The role, app and part are what
    /// classification produces from the command line, which lower-cases and
    /// scans kilobytes of Chromium flags; they are recomputed only for a new
    /// process instance (PID *and* start time, so a recycled PID is never
    /// mistaken for the process it replaced), or when its name, owner or
    /// command line changed. The maps themselves are cleared rather than
    /// reallocated, so their capacity survives.
    pub fn sample(&mut self, desktop: &Desktop) -> ramwarden_platform::Result<()> {
        let table = self.source.sample()?;
        let degraded = self.source.accounting_degraded();
        self.ingest(table, desktop, degraded);
        Ok(())
    }

    /// Score a process table sampled elsewhere (see [`Detector::take_source`]).
    /// `degraded` is the source's [`ProcessSource::accounting_degraded`].
    pub fn ingest(&mut self, table: Vec<ProcessSample>, desktop: &Desktop, degraded: bool) {
        self.degraded = degraded;
        let mut fresh = std::mem::take(&mut self.spare);
        fresh.clear();
        fresh.reserve(table.len());
        let mut old = std::mem::take(&mut self.signals);
        self.parents.clear();
        self.instances.begin();

        for p in table {
            if p.pid == self.our_pid {
                continue;
            }
            self.parents.insert(p.pid, p.ppid);

            let (inst, new) = self.instances.entry(p.pid, p.started, || Instance {
                cpu_total: p.cpu_seconds,
                owner: p.owner,
                cmdline: std::sync::Arc::clone(&p.cmdline),
            });
            let cpu_delta = if new { 0.0 } else { (p.cpu_seconds - inst.cpu_total).max(0.0) };
            inst.cpu_total = p.cpu_seconds;
            let same_image = !new
                && inst.owner == p.owner
                && (std::sync::Arc::ptr_eq(&inst.cmdline, &p.cmdline) || *inst.cmdline == *p.cmdline);
            if !same_image {
                inst.owner = p.owner;
                inst.cmdline = std::sync::Arc::clone(&p.cmdline);
            }

            let mut s = match old.remove(&p.pid) {
                Some(s) if same_image && s.name == p.name => s,
                _ => {
                    let role = classify(&p.name, &p.cmdline, uid_hint(p.owner));
                    let mut s = Signals::new(p.pid, p.name, role);
                    s.app = crate::describe::app_of(&s.name, &p.cmdline);
                    s.part = crate::describe::part_of(&s.name, &p.cmdline);
                    s
                }
            };
            s.pss = p.proportional;
            s.rss = p.resident;
            s.state = p.state;
            s.scope = p.group;
            s.ppid = p.ppid;
            s.listening_ports = p.listening_ports;
            s.established = p.established;
            s.is_focused = desktop.focused_pid == Some(p.pid);
            s.has_window = desktop.window_pids.contains(&p.pid);
            s.playing_audio = desktop.audio_pids.contains(&p.pid);
            s.has_tty = p.has_tty;
            s.cpu_seconds_recent = cpu_delta;
            s.age_minutes = p.age_seconds / 60.0;
            s.active_descendant = None;

            fresh.insert(p.pid, s);
        }
        self.instances.sweep();
        old.clear();
        self.spare = old;

        propagate(&mut fresh, &self.parents);

        self.ticks += 1;
        let warm = self.is_warm();
        for s in fresh.values_mut() {
            s.score(warm);
        }

        // Per application: is any process of it in use (focus, audio, a busy
        // child), and is any using CPU? One pass over the processes; the old
        // code rescanned every process once per application to prune.
        let now = Instant::now();
        let mut apps: HashMap<&str, AppTick> = HashMap::with_capacity(self.app_active.len() + 8);
        for s in fresh.values() {
            let used = s.is_focused || s.playing_audio || s.active_descendant.is_some();
            let cpu = s.cpu_seconds_recent >= crate::signals::CPU_BUSY_SECONDS;
            let a = apps.entry(s.app.as_str()).or_default();
            a.used |= used;
            a.cpu |= cpu;
        }
        for (app, a) in &apps {
            let busy = a.used || a.cpu;
            match self.app_active.get_mut(*app) {
                Some(t) if busy => *t = now,
                Some(_) => {}
                None => {
                    self.app_active.insert(app.to_string(), now);
                }
            }
            if a.cpu && !a.used {
                if !self.app_busy_since.contains_key(*app) {
                    self.app_busy_since.insert(app.to_string(), now);
                }
            } else {
                self.app_busy_since.remove(*app);
            }
        }
        self.app_active.retain(|app, _| apps.contains_key(app.as_str()));
        self.app_busy_since.retain(|app, _| apps.contains_key(app.as_str()));
        drop(apps);

        self.signals = fresh;
        self.focused = desktop.focused_pid;
        self.update_recency();
    }

    /// Fold this sample into the LRU.
    ///
    /// A name any of whose processes was used moves to the head. A name seen for
    /// the first time without being used joins at the tail, so an app that was
    /// already sitting idle when the daemon started is treated as cold rather
    /// than brand new. Names with no process left are dropped.
    fn update_recency(&mut self) {
        let now = Instant::now();
        let mut used: HashMap<&str, bool> = HashMap::with_capacity(self.recency.len() + 8);
        for s in self.signals.values() {
            *used.entry(s.name.as_str()).or_default() |= s.recently_used();
        }
        self.recency.retain(|name, _| used.contains_key(name.as_str()));
        for (name, was_used) in used {
            let fresh = Recency { since: now, touched: true };
            if was_used {
                // Move an existing entry without allocating its key again.
                match self.recency.touch(name) {
                    Some(r) => *r = fresh,
                    None => self.recency.put_hot(name.to_string(), fresh),
                }
            } else if !self.recency.contains(name) {
                self.recency.put_cold(name.to_string(), Recency { since: now, touched: false });
            }
        }
    }

    // ── Queries ─────────────────────────────────────────────────────────────

    /// Application names, least recently used first.
    pub fn coldest(&self) -> impl Iterator<Item = (&str, &Recency)> {
        self.recency.iter_cold().map(|(k, v)| (k.as_str(), v))
    }

    /// When `name` was last used, if it is running.
    pub fn recency(&self, name: &str) -> Option<&Recency> {
        self.recency.peek(name)
    }

    /// Whether `pid` is the focused application or one of its descendants: a
    /// helper, renderer or child the user is, in effect, looking at. Walks the
    /// ancestor chain, which is a handful of steps; the `seen` guard stops a
    /// malformed parent map from looping.
    pub fn in_focused_app(&self, pid: i32) -> bool {
        let Some(focused) = self.focused else { return false };
        let mut seen: HashSet<i32> = HashSet::new();
        let mut cur = pid;
        while cur > 1 && seen.insert(cur) {
            if cur == focused {
                return true;
            }
            cur = self.parents.get(&cur).copied().unwrap_or(0);
        }
        false
    }

    pub fn snapshot(&self) -> &HashMap<i32, Signals> {
        &self.signals
    }

    /// Minutes since any process of `app` last did anything, as of `now`.
    pub fn quiet_minutes(&self, app: &str, now: Instant) -> Option<f64> {
        self.app_active.get(app).map(|t| now.saturating_duration_since(*t).as_secs_f64() / 60.0)
    }

    /// Minutes `app` has been using CPU while nobody is using it, or 0.
    pub fn runaway_minutes(&self, app: &str, now: Instant) -> f64 {
        self.app_busy_since.get(app).map_or(0.0, |t| now.saturating_duration_since(*t).as_secs_f64() / 60.0)
    }

    pub fn get(&self, pid: i32) -> Option<&Signals> {
        self.signals.get(&pid)
    }

    pub fn is_empty(&self) -> bool {
        self.signals.is_empty()
    }

    /// Every process whose name equals or globs `name_pattern`, case-insensitively.
    pub fn matching(&self, name_pattern: &str) -> Vec<&Signals> {
        let mut out: Vec<&Signals> = self
            .signals
            .values()
            .filter(|s| pattern::matches_ci(name_pattern, &s.name))
            .collect();
        out.sort_by_key(|s| s.pid);
        out
    }

    /// Aggregate verdict across every process sharing a name.
    ///
    /// The strictest verdict wins. Suspending half an application is worse than
    /// suspending none of it: a browser with one frozen renderer looks broken
    /// rather than smaller.
    pub fn verdict_for_name(&self, name_pattern: &str) -> (Verdict, Vec<String>, Vec<i32>) {
        let matched = self.matching(name_pattern);
        if matched.is_empty() {
            return (Verdict::Idle, vec!["no such process running".into()], vec![]);
        }
        let pids: Vec<i32> = matched.iter().map(|s| s.pid).collect();

        let pick = |p: Protection| matched.iter().find(|s| s.protection == p);
        if let Some(s) = pick(Protection::Structural) {
            return (Verdict::Protected, s.reasons.clone(), pids);
        }
        if let Some(s) = pick(Protection::Serving) {
            return (Verdict::Protected, s.reasons.clone(), pids);
        }
        if let Some(s) = matched.iter().find(|s| s.verdict == Verdict::InUse) {
            return (Verdict::InUse, s.reasons.clone(), pids);
        }
        (Verdict::Idle, matched[0].reasons.clone(), pids)
    }

    /// The single gate every suspend, page-out, and kill passes through.
    pub fn may_suspend(&self, name_pattern: &str, watchlist: &[String]) -> Decision {
        let matched = self.matching(name_pattern);
        Self::gate(name_pattern, &matched, watchlist)
    }

    /// [`Detector::may_suspend`] over processes already matched to the pattern,
    /// so a caller that needs both the decision and the processes scans once.
    fn gate(name_pattern: &str, matched: &[&Signals], watchlist: &[String]) -> Decision {
        if matched.is_empty() {
            return Decision::Refuse(format!("no running process matches {name_pattern:?}"));
        }

        // Structural protection outranks everything, including an explicit
        // watchlist entry — report it first because it is the real reason.
        if let Some(s) = matched.iter().find(|s| s.protection == Protection::Structural) {
            return Decision::Refuse(format!("{name_pattern}: {}", s.reasons[0]));
        }

        if !pattern::any_ci(watchlist, name_pattern) {
            return Decision::Refuse(format!("{name_pattern:?} is not on the watchlist"));
        }

        if let Some(s) = matched.iter().find(|s| s.verdict == Verdict::InUse) {
            return Decision::Refuse(format!("{name_pattern} is in use — {}", s.reasons[0]));
        }

        if let Some(s) = matched.iter().find(|s| s.protection == Protection::Serving) {
            // Soft protection: the user explicitly watchlisted this, so allow it
            // but keep the consequence visible in the log and the UI.
            return Decision::Allow(format!("{name_pattern} is idle, but {}", s.reasons[0]));
        }

        Decision::Allow(format!("{name_pattern} is idle"))
    }

    /// Whether a forced action may proceed.
    ///
    /// `force` skips the watchlist and in-use checks — the user clicked a
    /// specific row and said so. It never skips structural protection; there is
    /// no gesture in the UI that should be able to freeze the compositor.
    pub fn may_suspend_forced(&self, name_pattern: &str) -> Decision {
        let matched = self.matching(name_pattern);
        if matched.is_empty() {
            return Decision::Refuse(format!("no running process matches {name_pattern:?}"));
        }
        if let Some(s) = matched.iter().find(|s| s.protection == Protection::Structural) {
            return Decision::Refuse(format!(
                "{name_pattern}: {} (not forceable)",
                s.reasons[0]
            ));
        }
        Decision::Allow(format!("forced by user for {name_pattern}"))
    }

    /// Processes the user opted into reclaiming that are genuinely idle.
    ///
    /// Nothing outside the watchlist is ever returned.
    ///
    /// One scan of the process table per watchlist entry, which serves both the
    /// gate and the result. Largest first; equal sizes by pid, so the order is
    /// stable from tick to tick.
    pub fn reclaimable(&self, watchlist: &[String]) -> Vec<&Signals> {
        let mut out: Vec<&Signals> = Vec::new();
        let mut taken: HashSet<i32> = HashSet::new();
        let mut matched: Vec<&Signals> = Vec::new();
        for pat in watchlist {
            matched.clear();
            matched.extend(self.signals.values().filter(|s| pattern::matches_ci(pat, &s.name)));
            // The gate reports the first offending process; pid order keeps
            // that the same one every time.
            matched.sort_unstable_by_key(|s| s.pid);
            if !Self::gate(pat, &matched, watchlist).is_allowed() {
                continue;
            }
            for s in &matched {
                if s.worth_reclaiming() && taken.insert(s.pid) {
                    out.push(s);
                }
            }
        }
        out.sort_unstable_by_key(|s| (std::cmp::Reverse(s.pss), s.pid));
        out
    }

    /// Total PSS per verdict — what the UI shows as protected / in use / idle.
    pub fn totals(&self) -> HashMap<&'static str, u64> {
        let mut out = HashMap::from([("PROTECTED", 0u64), ("IN_USE", 0), ("IDLE", 0)]);
        for s in self.signals.values() {
            *out.get_mut(s.verdict.as_str()).unwrap() += s.pss;
        }
        out
    }
}

/// Mark every ancestor of an active process as having an active descendant.
///
/// A shell whose child is compiling is itself in use — without this, the ladder
/// would happily freeze the terminal holding a running build.
fn propagate(sigs: &mut HashMap<i32, Signals>, parents: &HashMap<i32, i32>) {
    let seeds: Vec<(i32, String, i32)> = sigs
        .values()
        .filter(|s| s.self_active())
        .map(|s| (s.pid, s.name.clone(), s.pid))
        .collect();

    let parent = |p: i32| parents.get(&p).copied().filter(|&pp| pp != 0);

    for (pid, name, from_pid) in seeds {
        // A parent map read from /proc mid-churn can contain a cycle (a pid
        // reused while we were reading). Floyd's check costs no allocation, so
        // the visited set is only built for the rare chain that needs it.
        let cyclic = has_cycle(pid, parent);
        let mut seen: HashSet<i32> = if cyclic { HashSet::from([pid]) } else { HashSet::new() };
        let mut cur = parents.get(&pid).copied().unwrap_or(0);
        while cur != 0 && !seen.contains(&cur) && sigs.contains_key(&cur) {
            if cyclic {
                seen.insert(cur);
            }
            if let Some(anc) = sigs.get_mut(&cur)
                && anc.active_descendant.is_none()
            {
                anc.active_descendant = Some(format!("{name} (pid {from_pid})"));
            }
            cur = parents.get(&cur).copied().unwrap_or(0);
        }
    }
}

/// The owner as the UID [`classify`] understands: system-owned processes read
/// as root, so they classify as [`crate::roles::Role::System`] on every
/// platform. Anything else carries no UID, which classify treats as ordinary.
fn uid_hint(owner: Owner) -> Option<u32> {
    match owner {
        Owner::System => Some(0),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WATCHLIST: &[&str] = &["Discord", "burpsuite"];

    fn watchlist() -> Vec<String> {
        WATCHLIST.iter().map(|s| s.to_string()).collect()
    }

    /// A detector preloaded with hand-built signals, mirroring v1's `_detector`
    /// helper. Asserts on policy, not on whatever is running.
    fn detector(warm: bool, sigs: Vec<Signals>) -> Detector {
        Detector::preloaded(warm, sigs)
    }

    fn sig(pid: i32, name: &str) -> Signals {
        let mut s = Signals::new(pid, name, classify(name, "", None));
        s.pss = 500 * 1024 * 1024;
        s.age_minutes = 600.0;
        s
    }

    // ── Structural protection ───────────────────────────────────────────────

    // ── Reuse between ticks ─────────────────────────────────────────────────

    fn fake_detector(procs: Vec<ProcessSample>) -> (Detector, ramwarden_platform::fake::Shared) {
        let world = std::sync::Arc::new(std::sync::Mutex::new(ramwarden_platform::fake::World::default()));
        world.lock().unwrap().processes = procs;
        let platform = ramwarden_platform::fake::platform(world.clone());
        (Detector::new(platform.processes()), world)
    }

    /// A recycled pid is a new process: it is classified afresh and its CPU
    /// is not differenced against the dead process's.
    #[test]
    fn a_recycled_pid_is_classified_afresh() {
        let mut vm = ramwarden_platform::fake::process(50, "qemu-system-x86", 4 << 30);
        vm.started = 100;
        vm.cpu_seconds = 10.0;
        let (mut d, world) = fake_detector(vec![vm]);
        d.sample(&Desktop::default()).unwrap();
        d.sample(&Desktop::default()).unwrap();
        assert_eq!(d.get(50).unwrap().protection, Protection::Structural);

        let mut other = ramwarden_platform::fake::process(50, "Discord", 500 << 20);
        other.started = 900;
        other.cpu_seconds = 50.0;
        world.lock().unwrap().processes = vec![other];
        d.sample(&Desktop::default()).unwrap();
        let s = d.get(50).unwrap();
        assert_eq!(s.name, "Discord");
        assert_eq!(s.protection, Protection::None);
        assert_eq!(s.cpu_seconds_recent, 0.0, "no delta across two different processes");
    }

    /// The same process keeps its entry, and per-tick facts are overwritten,
    /// never carried over: a child that stopped working no longer marks its
    /// parent, and a lost focus is lost.
    #[test]
    fn a_reused_entry_carries_nothing_per_tick_over() {
        let mut shell = ramwarden_platform::fake::process(60, "bash", 10 << 20);
        shell.started = 1;
        let mut child = ramwarden_platform::fake::process(61, "worker", 10 << 20);
        child.started = 2;
        child.ppid = 60;
        child.cpu_seconds = 0.0;
        let (mut d, world) = fake_detector(vec![shell, child]);
        d.sample(&Desktop::default()).unwrap();
        world.lock().unwrap().processes[1].cpu_seconds = 5.0;
        let focused = Desktop { focused_pid: Some(60), ..Default::default() };
        d.sample(&focused).unwrap();
        assert!(d.get(60).unwrap().active_descendant.is_some());
        assert!(d.get(60).unwrap().is_focused);

        d.sample(&Desktop::default()).unwrap();
        let s = d.get(60).unwrap();
        assert!(s.active_descendant.is_none(), "{s:?}");
        assert!(!s.is_focused);
        assert_eq!(d.get(61).unwrap().cpu_seconds_recent, 0.0);
    }

    /// v1's `test_virtual_machine_is_never_suspendable_even_if_watchlisted`.
    /// A detector that cannot read PSS has degraded to v1's accounting, and that
    /// must be reportable rather than silent.
    #[test]
    fn pss_availability_is_reported() {
        let d = detector(true, vec![sig(1, "brave")]);
        assert!(d.pss_available(), "a fresh detector has not failed yet");
    }

    #[test]
    fn a_virtual_machine_is_refused_even_when_watchlisted() {
        let mut vm = sig(10, "qemu-system-x86_64");
        vm.pss = 4200 * 1024 * 1024;
        let d = detector(true, vec![vm]);

        assert_eq!(d.get(10).unwrap().verdict, Verdict::Protected);
        let decision = d.may_suspend("qemu-system-x86_64", &["qemu-system-x86_64".into()]);
        assert!(!decision.is_allowed());
        assert!(decision.reason().contains("virtual machine"), "{decision:?}");
    }

    /// v1's `test_structural_protection_cannot_be_forced`.
    #[test]
    fn structural_protection_cannot_be_forced() {
        let d = detector(true, vec![sig(11, "dockerd")]);
        let decision = d.may_suspend_forced("dockerd");
        assert!(!decision.is_allowed());
        assert!(decision.reason().contains("not forceable"), "{decision:?}");
    }

    #[test]
    fn forcing_works_for_an_ordinary_app_the_gate_would_otherwise_refuse() {
        // In use, and not on the watchlist: refused normally, allowed when forced.
        let mut s = sig(12, "steamwebhelper");
        s.is_focused = true;
        let d = detector(true, vec![s]);
        assert!(!d.may_suspend("steamwebhelper", &watchlist()).is_allowed());
        assert!(d.may_suspend_forced("steamwebhelper").is_allowed());
    }

    /// The refusal must name the real reason, not the first check that happens
    /// to fail. A VM off the watchlist is refused *because it is a VM*.
    #[test]
    fn structural_refusal_outranks_the_watchlist_refusal() {
        let d = detector(true, vec![sig(13, "qemu-system-x86_64")]);
        let decision = d.may_suspend("qemu-system-x86_64", &watchlist());
        assert!(decision.reason().contains("virtual machine"), "{decision:?}");
        assert!(!decision.reason().contains("watchlist"), "{decision:?}");
    }

    // ── Serving ─────────────────────────────────────────────────────────────

    /// v1's `test_listening_socket_protects_an_unknown_process`.
    #[test]
    fn an_unwatchlisted_server_is_refused_for_not_being_on_the_list() {
        let mut s = sig(20, "my-api");
        s.listening_ports = vec![8000];
        let d = detector(true, vec![s]);

        assert_eq!(d.get(20).unwrap().protection, Protection::Serving);
        let decision = d.may_suspend("my-api", &watchlist());
        assert!(!decision.is_allowed());
        assert!(decision.reason().contains("not on the watchlist"), "{decision:?}");
    }

    /// v1's `test_watchlist_overrides_soft_serving_protection_with_a_warning`.
    /// Discord's RPC port is not a service anyone depends on — the user decides,
    /// but the consequence stays visible.
    #[test]
    fn the_watchlist_overrides_serving_protection_but_keeps_the_warning() {
        let mut s = sig(21, "Discord");
        s.listening_ports = vec![6463];
        let d = detector(true, vec![s]);

        let decision = d.may_suspend("Discord", &watchlist());
        assert!(decision.is_allowed(), "{decision:?}");
        assert!(decision.reason().contains("6463"), "{decision:?}");
    }

    // ── In-use ──────────────────────────────────────────────────────────────

    #[test]
    fn an_in_use_process_is_refused_with_its_reason() {
        let mut s = sig(30, "Discord");
        s.playing_audio = true;
        let d = detector(true, vec![s]);
        let decision = d.may_suspend("Discord", &watchlist());
        assert!(!decision.is_allowed());
        assert!(decision.reason().contains("playing audio"), "{decision:?}");
    }

    /// v1's `test_quiet_watchlisted_app_is_reclaimable`.
    #[test]
    fn a_quiet_watchlisted_app_is_reclaimable() {
        let d = detector(true, vec![sig(31, "Discord")]);
        assert_eq!(d.get(31).unwrap().verdict, Verdict::Idle);
        assert!(d.may_suspend("Discord", &watchlist()).is_allowed());
        assert_eq!(
            d.reclaimable(&watchlist()).iter().map(|s| s.pid).collect::<Vec<_>>(),
            vec![31]
        );
    }

    // ── Fail closed ─────────────────────────────────────────────────────────

    /// v1's `test_nothing_is_idle_before_two_samples`.
    #[test]
    fn nothing_is_reclaimable_before_two_samples() {
        let d = detector(false, vec![sig(40, "Discord")]);
        assert!(!d.is_warm());
        assert_eq!(d.get(40).unwrap().verdict, Verdict::InUse);
        assert!(!d.may_suspend("Discord", &watchlist()).is_allowed());
        assert!(d.reclaimable(&watchlist()).is_empty());
    }

    #[test]
    fn an_unknown_name_is_refused_rather_than_silently_matching_nothing() {
        let d = detector(true, vec![sig(41, "Discord")]);
        let decision = d.may_suspend("not-running", &watchlist());
        assert!(!decision.is_allowed());
        assert!(decision.reason().contains("no running process"), "{decision:?}");
    }

    // ── Watchlist is a ceiling ──────────────────────────────────────────────

    /// v1's `test_reclaimable_never_leaves_the_watchlist`.
    #[test]
    fn reclaimable_never_leaves_the_watchlist() {
        let mut other = sig(51, "steamwebhelper");
        other.pss = 900 * 1024 * 1024;
        let d = detector(true, vec![sig(50, "Discord"), other]);

        assert_eq!(d.get(51).unwrap().verdict, Verdict::Idle, "idle, but off the list");
        let names: Vec<&str> = d.reclaimable(&watchlist()).iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["Discord"]);
    }

    #[test]
    fn an_empty_watchlist_makes_nothing_reclaimable() {
        let d = detector(true, vec![sig(52, "Discord")]);
        assert!(d.reclaimable(&[]).is_empty());
    }

    #[test]
    fn a_glob_watchlist_entry_matches_what_it_did_in_v1() {
        let d = detector(true, vec![sig(53, "burpsuite"), sig(54, "BurpSuiteCommunity")]);
        let wl = vec!["burp*".to_string()];
        let pids: Vec<i32> = d.reclaimable(&wl).iter().map(|s| s.pid).collect();
        assert_eq!(pids.len(), 2, "{pids:?}");
    }

    /// v1's `test_stopped_processes_are_not_offered_again`.
    #[test]
    fn an_already_stopped_process_is_not_offered_again() {
        let mut s = sig(60, "Discord");
        s.state = 'T';
        let d = detector(true, vec![s]);
        assert!(d.reclaimable(&watchlist()).is_empty());
    }

    /// v1's `test_small_processes_are_not_worth_the_risk`.
    #[test]
    fn a_small_process_is_not_worth_the_risk() {
        let mut s = sig(61, "Discord");
        s.pss = 20 * 1024 * 1024;
        let d = detector(true, vec![s]);
        assert!(d.reclaimable(&watchlist()).is_empty());
    }

    #[test]
    fn reclaimable_is_ordered_biggest_first() {
        let mut a = sig(62, "Discord");
        a.pss = 200 * 1024 * 1024;
        let mut b = sig(63, "Discord");
        b.pss = 900 * 1024 * 1024;
        let d = detector(true, vec![a, b]);
        let pids: Vec<i32> = d.reclaimable(&watchlist()).iter().map(|s| s.pid).collect();
        assert_eq!(pids, vec![63, 62], "largest first");
    }

    #[test]
    fn overlapping_watchlist_patterns_do_not_duplicate_a_process() {
        let d = detector(true, vec![sig(64, "Discord")]);
        let wl = vec!["Discord".to_string(), "Disc*".to_string(), "*cord".to_string()];
        assert_eq!(d.reclaimable(&wl).len(), 1);
    }

    // ── Group semantics ─────────────────────────────────────────────────────

    /// v1's `test_the_busiest_member_decides_the_whole_group`.
    #[test]
    fn the_busiest_member_decides_the_whole_group() {
        let mut busy = sig(71, "Discord");
        busy.playing_audio = true;
        let d = detector(true, vec![sig(70, "Discord"), busy]);

        let (verdict, _reasons, pids) = d.verdict_for_name("Discord");
        assert_eq!(verdict, Verdict::InUse);
        assert_eq!(pids, vec![70, 71]);
        assert!(!d.may_suspend("Discord", &watchlist()).is_allowed());
    }

    #[test]
    fn a_group_with_one_protected_member_is_protected_as_a_whole() {
        let mut serving = sig(73, "Discord");
        serving.listening_ports = vec![6463];
        let d = detector(true, vec![sig(72, "Discord"), serving]);
        let (verdict, reasons, _) = d.verdict_for_name("Discord");
        assert_eq!(verdict, Verdict::Protected);
        assert!(reasons[0].contains("6463"));
    }

    #[test]
    fn a_name_with_no_processes_reports_so() {
        let d = detector(true, vec![sig(74, "Discord")]);
        let (verdict, reasons, pids) = d.verdict_for_name("nothing");
        assert_eq!(verdict, Verdict::Idle);
        assert!(pids.is_empty());
        assert!(reasons[0].contains("no such process"));
    }

    // ── Tree propagation ────────────────────────────────────────────────────

    /// v1's `test_activity_propagates_up_the_process_tree`.
    #[test]
    fn activity_propagates_up_the_process_tree() {
        let mut sigs = HashMap::new();
        sigs.insert(80, sig(80, "bash"));
        sigs.insert(81, sig(81, "cargo"));
        let parents = HashMap::from([(81, 80), (80, 1)]);

        propagate(&mut sigs, &parents);
        for s in sigs.values_mut() {
            s.score(true);
        }

        let parent = &sigs[&80];
        assert!(
            parent.active_descendant.as_deref().unwrap_or("").contains("cargo"),
            "{:?}",
            parent.active_descendant
        );
        assert_eq!(parent.verdict, Verdict::InUse);
    }

    #[test]
    fn propagation_reaches_grandparents() {
        let mut sigs = HashMap::new();
        for (pid, name) in [(90, "cosmic-term"), (91, "bash"), (92, "cargo")] {
            sigs.insert(pid, sig(pid, name));
        }
        let parents = HashMap::from([(92, 91), (91, 90), (90, 1)]);
        propagate(&mut sigs, &parents);
        assert!(sigs[&91].active_descendant.is_some(), "parent");
        assert!(sigs[&90].active_descendant.is_some(), "grandparent");
    }

    #[test]
    fn an_idle_tree_propagates_nothing() {
        let mut sigs = HashMap::new();
        sigs.insert(95, sig(95, "bash"));
        sigs.insert(96, sig(96, "sleep"));
        propagate(&mut sigs, &HashMap::from([(96, 95), (95, 1)]));
        assert!(sigs[&95].active_descendant.is_none());
    }

    /// A cycle in the parent map must not spin forever. It should not happen,
    /// but a /proc read racing with process exit can produce surprising pairs.
    #[test]
    fn a_cycle_in_the_parent_map_terminates() {
        let mut sigs = HashMap::new();
        let mut busy = sig(97, "cargo");
        busy.cpu_seconds_recent = 5.0;
        sigs.insert(97, busy);
        sigs.insert(98, sig(98, "bash"));
        // 97 -> 98 -> 97
        propagate(&mut sigs, &HashMap::from([(97, 98), (98, 97)]));
        assert!(sigs[&98].active_descendant.is_some());
    }

    // ── Reporting ───────────────────────────────────────────────────────────

    #[test]
    fn totals_are_grouped_by_verdict_and_summed_in_pss() {
        let mut vm = sig(100, "qemu-system-x86_64");
        vm.pss = 1000;
        let mut busy = sig(101, "Discord");
        busy.pss = 200;
        busy.is_focused = true;
        let mut idle = sig(102, "Discord");
        idle.pss = 30;

        let d = detector(true, vec![vm, busy, idle]);
        let t = d.totals();
        assert_eq!(t["PROTECTED"], 1000);
        assert_eq!(t["IN_USE"], 200);
        assert_eq!(t["IDLE"], 30);
    }

    #[test]
    fn matching_is_case_insensitive_and_sorted_by_pid() {
        let d = detector(true, vec![sig(5, "Discord"), sig(3, "discord")]);
        let pids: Vec<i32> = d.matching("DISCORD").iter().map(|s| s.pid).collect();
        assert_eq!(pids, vec![3, 5]);
    }

    // ── Focus ancestry ──────────────────────────────────────────────────────

    struct Table(Vec<ProcessSample>);

    impl ProcessSource for Table {
        fn sample(&mut self) -> ramwarden_platform::Result<Vec<ProcessSample>> {
            Ok(self.0.clone())
        }
    }

    fn proc(pid: i32, ppid: i32, name: &str) -> ProcessSample {
        ProcessSample { pid, ppid, name: name.into(), proportional: 1 << 20, ..Default::default() }
    }

    #[test]
    fn a_helper_belongs_to_the_focused_app_and_a_stranger_does_not() {
        let table = vec![
            proc(100, 1, "Cursor"),
            proc(101, 100, "Cursor Helper (Renderer)"),
            proc(102, 101, "Cursor Helper (Plugin)"),
            proc(200, 1, "Arabesque"),
        ];
        let mut d = Detector::new(Box::new(Table(table)));
        d.sample(&Desktop { focused_pid: Some(100), ..Default::default() }).unwrap();
        for pid in [100, 101, 102] {
            assert!(d.in_focused_app(pid), "{pid}");
        }
        assert!(!d.in_focused_app(200));

        d.sample(&Desktop { focused_pid: None, ..Default::default() }).unwrap();
        assert!(!d.in_focused_app(101), "no focus known: nothing is thawed");
    }

    #[test]
    fn a_cyclic_parent_map_terminates() {
        let table = vec![proc(300, 301, "a"), proc(301, 300, "b"), proc(400, 1, "focused")];
        let mut d = Detector::new(Box::new(Table(table)));
        d.sample(&Desktop { focused_pid: Some(400), ..Default::default() }).unwrap();
        assert!(!d.in_focused_app(300));
    }
}
