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

use ramwarden_kernel::{Root, cgroup, net, process, smaps};

use crate::pattern;
use crate::roles::classify;
use crate::signals::{Protection, Signals, Verdict};

/// Resident size below which PSS is approximated rather than measured.
///
/// Reading `smaps_rollup` costs a syscall and a parse per process. For a 4 MB
/// helper the distinction between RSS and PSS cannot change any decision — it is
/// far below [`crate::signals::IDLE_FLOOR_BYTES`] either way — so the cheap
/// figure from `stat` is used instead. v1 paid full price for every process.
const PSS_MEASURE_FLOOR: u64 = 24 * 1024 * 1024;

/// Signals that do not come from `/proc`.
///
/// Separated so tests can supply them directly: collecting them means running
/// `pactl` and `wmctrl`, which have no business executing in a unit test.
#[derive(Clone, Debug, Default)]
pub struct Desktop {
    /// PIDs with an active audio stream, playing or recording.
    pub audio_pids: HashSet<i32>,
    /// PIDs owning a mapped window.
    ///
    /// Only ever a positive signal. Under Wayland, `wmctrl` sees XWayland
    /// clients but not native ones, so an absent window proves nothing — which
    /// is why [`Signals::score`] never treats its absence as evidence.
    pub window_pids: HashSet<i32>,
    /// PID owning the focused window, if known.
    pub focused_pid: Option<i32>,
}

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
    root: Root,
    signals: HashMap<i32, Signals>,
    /// pid -> cumulative CPU ticks at the previous sample. The CPU signal is a
    /// difference, which is the whole reason [`Detector::is_warm`] exists.
    cpu_prev: HashMap<i32, u64>,
    ticks: u32,
    our_pid: i32,
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

impl Detector {
    pub fn new(root: Root) -> Self {
        Detector {
            root,
            signals: HashMap::new(),
            cpu_prev: HashMap::new(),
            ticks: 0,
            our_pid: std::process::id() as i32,
            pss_unavailable: false,
        }
    }

    /// Whether PSS measurement is working.
    ///
    /// False means accounting has degraded to summed RSS and over-reports any
    /// process that shares pages — which is every browser.
    pub fn pss_available(&self) -> bool {
        !self.pss_unavailable
    }

    /// A detector holding hand-built signals, for tests and for replaying a
    /// captured snapshot. `warm` controls whether the CPU signal is trusted.
    pub fn preloaded(root: Root, warm: bool, sigs: Vec<Signals>) -> Self {
        let mut d = Detector::new(root);
        d.ticks = if warm { 2 } else { 1 };
        for mut s in sigs {
            s.score(warm);
            d.signals.insert(s.pid, s);
        }
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
    /// `desktop` carries the signals that require subprocesses; the caller
    /// refreshes it on its own slower cadence.
    pub fn sample(&mut self, desktop: &Desktop) -> ramwarden_kernel::Result<()> {
        let now_epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let boot = process::boot_time(&self.root).unwrap_or(0);
        let sockets = net::sockets(&self.root).unwrap_or_default();
        let scopes = self.scope_map();

        let table = process::table(&self.root)?;
        let mut fresh: HashMap<i32, Signals> = HashMap::with_capacity(table.len());
        let mut parents: HashMap<i32, i32> = HashMap::with_capacity(table.len());
        let mut cpu_now: HashMap<i32, u64> = HashMap::with_capacity(table.len());

        for p in table {
            if p.pid == self.our_pid {
                continue;
            }
            parents.insert(p.pid, p.ppid);
            cpu_now.insert(p.pid, p.cpu_ticks);

            let cpu_delta = match self.cpu_prev.get(&p.pid) {
                Some(&prev) => {
                    (p.cpu_ticks.saturating_sub(prev)) as f64 / process::clock_ticks() as f64
                }
                None => 0.0,
            };

            let rss = p.rss_bytes();
            // Measure PSS properly only where it could matter.
            let pss = if rss >= PSS_MEASURE_FLOOR {
                match smaps::rollup(&self.root, p.pid) {
                    Ok(r) => r.pss,
                    Err(e) if e.is_missing() => rss, // the process just exited
                    Err(e) => {
                        // Not a vanished process: we are being denied. Say so
                        // loudly and once, because the fallback silently restores
                        // v1's overcount.
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
            } else {
                rss
            };

            let cmdline = process::cmdline(&self.root, p.pid).unwrap_or_default();
            let uid = process::uid(&self.root, p.pid).ok();

            let mut s = Signals::new(p.pid, &p.comm, classify(&p.comm, &cmdline, uid));
            s.pss = pss;
            s.rss = rss;
            s.state = p.state;
            s.scope = scopes.get(&p.pid).cloned();
            s.listening_ports = sockets.ports_for(p.pid).to_vec();
            s.established = sockets.established_for(p.pid);
            s.is_focused = desktop.focused_pid == Some(p.pid);
            s.has_window = desktop.window_pids.contains(&p.pid);
            s.playing_audio = desktop.audio_pids.contains(&p.pid);
            s.has_tty = p.has_tty();
            s.cpu_seconds_recent = cpu_delta;
            s.age_minutes = p.age_seconds(boot, now_epoch) / 60.0;

            fresh.insert(p.pid, s);
        }

        propagate(&mut fresh, &parents);

        self.ticks += 1;
        let warm = self.is_warm();
        for s in fresh.values_mut() {
            s.score(warm);
        }

        self.cpu_prev = cpu_now;
        self.signals = fresh;
        // Recoverable: if the unit is fixed and reloaded, a later tick will read
        // rollups again and the warning should be able to fire once more.
        if self.pss_unavailable && self.signals.values().any(|s| s.pss != s.rss) {
            self.pss_unavailable = false;
            tracing::info!("smaps_rollup is readable again — PSS accounting restored");
        }
        Ok(())
    }

    /// pid -> cgroup scope name, for every process in the user's session.
    ///
    /// Processes outside it (system services, every Docker container) get no
    /// scope, which is also a reminder that the ladder cannot reach them.
    fn scope_map(&self) -> HashMap<i32, String> {
        let mut out = HashMap::new();
        // SAFETY: getuid cannot fail and touches no memory.
        let uid = unsafe { libc_getuid() };
        let Ok(h) = cgroup::Hierarchy::user_session(&self.root, uid) else {
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

    // ── Queries ─────────────────────────────────────────────────────────────

    pub fn snapshot(&self) -> &HashMap<i32, Signals> {
        &self.signals
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
    pub fn reclaimable(&self, watchlist: &[String]) -> Vec<&Signals> {
        let mut out: Vec<&Signals> = Vec::new();
        for pat in watchlist {
            if !self.may_suspend(pat, watchlist).is_allowed() {
                continue;
            }
            for s in self.matching(pat) {
                if s.worth_reclaiming() && !out.iter().any(|e| e.pid == s.pid) {
                    out.push(s);
                }
            }
        }
        out.sort_by_key(|s| std::cmp::Reverse(s.pss));
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

    for (pid, name, from_pid) in seeds {
        let mut seen: HashSet<i32> = HashSet::from([pid]);
        let mut cur = parents.get(&pid).copied().unwrap_or(0);
        // The `seen` set guards against a cycle in a malformed parent map,
        // which would otherwise spin forever.
        while cur != 0 && !seen.contains(&cur) && sigs.contains_key(&cur) {
            seen.insert(cur);
            if let Some(anc) = sigs.get_mut(&cur)
                && anc.active_descendant.is_none()
            {
                anc.active_descendant = Some(format!("{name} (pid {from_pid})"));
            }
            cur = parents.get(&cur).copied().unwrap_or(0);
        }
    }
}

unsafe extern "C" {
    #[link_name = "getuid"]
    fn libc_getuid() -> u32;
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
        Detector::preloaded(Root::at("/nonexistent"), warm, sigs)
    }

    fn sig(pid: i32, name: &str) -> Signals {
        let mut s = Signals::new(pid, name, classify(name, "", None));
        s.pss = 500 * 1024 * 1024;
        s.age_minutes = 600.0;
        s
    }

    // ── Structural protection ───────────────────────────────────────────────

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
}
