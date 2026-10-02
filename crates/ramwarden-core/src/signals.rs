//! One process's activity signals, and the verdict they produce.
//!
//! # The question this answers
//!
//! Not "is this process busy?" but *"is anything depending on this right now?"*
//! v1's insight, kept intact: collect cheap, factual signals and let them decide
//! what to spare, rather than consulting a list of program names.
//!
//! ```text
//! serving      holds a listening socket — killing it strands clients
//! connected    has established connections
//! focused      owns the window the user is looking at
//! windowed     owns any mapped window
//! audio        playing or capturing sound
//! tty          attached to a terminal the user has open
//! busy         measurable CPU in the recent sample window
//! fresh        started in the last few minutes
//! descendant   a child or grandchild is itself active
//! scope        the cgroup it shares with the rest of its application
//! ```
//!
//! # What changed from v1
//!
//! Size is now PSS rather than summed RSS (see [`ramwarden_kernel::smaps`]), and
//! `scope` is new — it ties a process to the cgroup the ladder can reclaim from.
//! The verdicts, the ordering, and the user-facing reason strings are unchanged,
//! because they were right.

use ramwarden_kernel::smaps::Rollup;

use crate::roles::Role;

/// CPU seconds consumed within one sample window that count as "busy".
pub const CPU_BUSY_SECONDS: f64 = 0.5;

/// A process started this recently was just launched by the user.
pub const FRESH_MINUTES: f64 = 10.0;

/// Below this, reclaiming a process is not worth the risk of being wrong.
///
/// v1 compared this against summed RSS. Comparing against PSS makes it a
/// slightly stricter bar — a process has to genuinely own this much memory, not
/// merely map it — which is the correct reading of "worth the risk".
pub const IDLE_FLOOR_BYTES: u64 = 120 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Verdict {
    /// Off-limits. Never suspend, never close.
    Protected,
    /// The user is demonstrably using this right now.
    InUse,
    /// No signal in the sample window. The only verdict the ladder may act on,
    /// and only for processes the user opted in via the watchlist.
    Idle,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Protected => "PROTECTED",
            Verdict::InUse => "IN_USE",
            Verdict::Idle => "IDLE",
        }
    }
}

/// Why a process is protected, and whether that can be overridden.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Protection {
    /// Not protected.
    None,
    /// Protected by what it *is* — a VM, a compositor, a container runtime.
    /// Absolute: no watchlist entry and no `force` flag may override it.
    Structural,
    /// Protected because something may be depending on it over a socket. Soft:
    /// an explicit watchlist entry overrides this, with the consequence logged.
    Serving,
}

/// Everything known about one process at one moment.
#[derive(Clone, Debug)]
pub struct Signals {
    pub pid: i32,
    pub name: String,
    /// Proportional set size in bytes — the figure that is meaningful to sum.
    pub pss: u64,
    /// Approximate resident bytes from `stat`. Kept only to show the user how
    /// far a naive reading would mislead.
    pub rss: u64,
    /// Process state: `S`, `R`, `T` (stopped), `Z`, ...
    pub state: char,
    pub role: Role,
    /// The cgroup scope this process shares with the rest of its application.
    /// `None` for processes outside the user's session, which the ladder may
    /// never touch anyway.
    pub scope: Option<String>,

    pub listening_ports: Vec<u16>,
    pub established: u32,
    pub is_focused: bool,
    pub has_window: bool,
    pub playing_audio: bool,
    pub has_tty: bool,
    pub cpu_seconds_recent: f64,
    pub age_minutes: f64,
    pub active_descendant: Option<String>,

    pub verdict: Verdict,
    pub protection: Protection,
    /// Human-readable justifications, shown in the UI on hover. Order matters:
    /// the first is the headline reason.
    pub reasons: Vec<String>,
}

impl Signals {
    /// A bare process with no activity signals yet. Score it with
    /// [`Signals::score`] before reading the verdict.
    pub fn new(pid: i32, name: impl Into<String>, role: Role) -> Self {
        Signals {
            pid,
            name: name.into(),
            pss: 0,
            rss: 0,
            state: 'S',
            role,
            scope: None,
            listening_ports: Vec::new(),
            established: 0,
            is_focused: false,
            has_window: false,
            playing_audio: false,
            has_tty: false,
            cpu_seconds_recent: 0.0,
            age_minutes: 0.0,
            active_descendant: None,
            verdict: Verdict::Idle,
            protection: Protection::None,
            reasons: Vec::new(),
        }
    }

    pub fn with_memory(mut self, r: &Rollup) -> Self {
        self.pss = r.pss;
        self.rss = r.rss;
        self
    }

    pub fn is_serving(&self) -> bool {
        !self.listening_ports.is_empty()
    }

    pub fn is_stopped(&self) -> bool {
        self.state == 'T'
    }

    /// Activity from this process alone, ignoring its children.
    ///
    /// Used to seed tree propagation: an active process marks its ancestors, but
    /// only "really doing something" counts as a seed. A mapped window or an
    /// idle connection is not enough — otherwise every ancestor of every
    /// windowed app would be in use, which is every process on the desktop.
    pub fn self_active(&self) -> bool {
        self.is_focused
            || self.playing_audio
            || self.cpu_seconds_recent >= CPU_BUSY_SECONDS
            || matches!(
                self.role,
                Role::Hypervisor | Role::Container | Role::Agent | Role::Build | Role::Sync | Role::Media
            )
    }

    /// Assign a verdict from the collected signals.
    ///
    /// `warm` is whether at least two samples have been taken. It matters
    /// because the CPU signal is a *delta*: with one sample there is nothing to
    /// compare against, so "no CPU" means "not measured yet". Calling that idle
    /// is how a freshly-started daemon suspends something the user is using.
    pub fn score(&mut self, warm: bool) {
        self.reasons.clear();

        // 1. Structural protection outranks everything, including activity. A
        // quiet VM is still a VM.
        if self.role.is_structural() {
            self.verdict = Verdict::Protected;
            self.protection = Protection::Structural;
            self.reasons = vec![self.role.protection_reason().to_string()];
            return;
        }

        // 2. A listening socket means something may be depending on this, even
        // if nobody wrote its name down.
        if self.is_serving() {
            let ports: Vec<String> = self
                .listening_ports
                .iter()
                .take(4)
                .map(|p| p.to_string())
                .collect();
            self.verdict = Verdict::Protected;
            self.protection = Protection::Serving;
            self.reasons = vec![format!(
                "serving on port {} — clients would hang",
                ports.join(", ")
            )];
            return;
        }

        self.protection = Protection::None;

        // 3. Positive evidence the user is using it.
        if self.is_focused {
            self.reasons.push("this is the window you are looking at".into());
        }
        if self.playing_audio {
            self.reasons.push("playing audio".into());
        }
        if self.cpu_seconds_recent >= CPU_BUSY_SECONDS {
            self.reasons
                .push(format!("used {:.1}s CPU just now", self.cpu_seconds_recent));
        }
        if let Some(desc) = &self.active_descendant {
            self.reasons.push(format!("child still working: {desc}"));
        }
        if self.age_minutes < FRESH_MINUTES {
            self.reasons
                .push(format!("started {:.0} min ago", self.age_minutes));
        }

        if !self.reasons.is_empty() {
            self.verdict = Verdict::InUse;
            return;
        }

        // 4. Fail closed while the CPU signal is still meaningless.
        if !warm {
            self.verdict = Verdict::InUse;
            self.reasons = vec!["still sampling — no verdict yet".into()];
            return;
        }

        // 5. Nothing. Reclaimable, if the user opted in.
        self.verdict = Verdict::Idle;
        if self.has_window {
            self.reasons.push("window open but untouched".into());
        }
        if self.established > 0 {
            self.reasons
                .push(format!("{} idle connection(s)", self.established));
        }
        self.reasons.push("no CPU since the last sample".into());
    }

    /// Big enough, and in the right state, to be worth reclaiming.
    pub fn worth_reclaiming(&self) -> bool {
        !self.is_stopped() && self.pss >= IDLE_FLOOR_BYTES
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::roles::classify;

    /// Build a scored signal, mirroring v1's `_sig` test helper.
    fn sig(name: &str, warm: bool, f: impl FnOnce(&mut Signals)) -> Signals {
        let mut s = Signals::new(1, name, classify(name, "", None));
        s.pss = 500 * 1024 * 1024;
        s.age_minutes = 600.0; // old enough not to be "fresh"
        f(&mut s);
        s.score(warm);
        s
    }

    fn warm(name: &str, f: impl FnOnce(&mut Signals)) -> Signals {
        sig(name, true, f)
    }

    // ── Structural protection ────────────────────────────────────────────────

    /// v1's `test_virtual_machine_is_never_suspendable_even_if_watchlisted`.
    #[test]
    fn a_quiet_virtual_machine_is_protected_not_idle() {
        let s = warm("qemu-system-x86_64", |s| s.pss = 4200 * 1024 * 1024);
        assert_eq!(s.verdict, Verdict::Protected);
        assert_eq!(s.protection, Protection::Structural);
        assert!(s.reasons[0].contains("virtual machine"));
    }

    #[test]
    fn structural_protection_outranks_every_activity_signal() {
        // Even with nothing happening at all, the role decides.
        let s = warm("dockerd", |s| {
            s.cpu_seconds_recent = 0.0;
            s.age_minutes = 99_999.0;
        });
        assert_eq!(s.verdict, Verdict::Protected);
        assert_eq!(s.protection, Protection::Structural);
    }

    // ── Serving ─────────────────────────────────────────────────────────────

    /// v1's `test_listening_socket_protects_an_unknown_process`.
    #[test]
    fn a_listening_socket_protects_a_process_nobody_wrote_down() {
        let s = warm("my-api", |s| s.listening_ports = vec![8000]);
        assert_eq!(s.verdict, Verdict::Protected);
        assert_eq!(s.protection, Protection::Serving);
        assert!(s.reasons[0].contains("8000"), "{:?}", s.reasons);
        assert!(s.reasons[0].contains("clients would hang"));
    }

    #[test]
    fn serving_protection_is_soft_and_distinguishable_from_structural() {
        let serving = warm("my-api", |s| s.listening_ports = vec![8000]);
        let structural = warm("dockerd", |_| {});
        assert_ne!(serving.protection, structural.protection);
        assert_eq!(serving.protection, Protection::Serving);
    }

    #[test]
    fn only_the_first_four_ports_are_listed_so_the_reason_stays_readable() {
        let s = warm("my-api", |s| s.listening_ports = vec![1, 2, 3, 4, 5, 6]);
        assert!(s.reasons[0].contains("1, 2, 3, 4"), "{:?}", s.reasons);
        assert!(!s.reasons[0].contains('5'), "{:?}", s.reasons);
    }

    // ── In-use signals ──────────────────────────────────────────────────────

    /// v1's parametrized `test_each_signal_marks_a_process_in_use`.
    #[test]
    fn each_signal_marks_a_process_in_use() {
        type Apply = Box<dyn Fn(&mut Signals)>;
        let cases: Vec<(&str, Apply)> = vec![
            ("looking at", Box::new(|s: &mut Signals| s.is_focused = true)),
            ("playing audio", Box::new(|s: &mut Signals| s.playing_audio = true)),
            ("CPU", Box::new(|s: &mut Signals| s.cpu_seconds_recent = CPU_BUSY_SECONDS + 0.1)),
            ("started", Box::new(|s: &mut Signals| s.age_minutes = 1.0)),
            (
                "child still working",
                Box::new(|s: &mut Signals| s.active_descendant = Some("cargo (pid 99)".into())),
            ),
        ];
        for (fragment, apply) in cases {
            let s = warm("Discord", |s| apply(s));
            assert_eq!(s.verdict, Verdict::InUse, "{fragment}");
            assert!(
                s.reasons.iter().any(|r| r.contains(fragment)),
                "{fragment} missing from {:?}",
                s.reasons
            );
        }
    }

    #[test]
    fn cpu_just_below_the_threshold_is_not_busy() {
        let s = warm("Discord", |s| s.cpu_seconds_recent = CPU_BUSY_SECONDS - 0.01);
        assert_eq!(s.verdict, Verdict::Idle);
    }

    #[test]
    fn a_process_at_the_freshness_boundary_is_not_fresh() {
        let s = warm("Discord", |s| s.age_minutes = FRESH_MINUTES);
        assert_eq!(s.verdict, Verdict::Idle, "{:?}", s.reasons);
        let s = warm("Discord", |s| s.age_minutes = FRESH_MINUTES - 0.1);
        assert_eq!(s.verdict, Verdict::InUse);
    }

    /// v1's `test_quiet_watchlisted_app_is_reclaimable`, verdict half.
    #[test]
    fn a_quiet_app_is_idle_and_explains_why() {
        let s = warm("Discord", |_| {});
        assert_eq!(s.verdict, Verdict::Idle);
        assert_eq!(s.protection, Protection::None);
        assert!(s.reasons.iter().any(|r| r.contains("no CPU")));
        assert!(s.worth_reclaiming());
    }

    #[test]
    fn an_idle_process_still_reports_its_window_and_connections() {
        let s = warm("Discord", |s| {
            s.has_window = true;
            s.established = 3;
        });
        assert_eq!(s.verdict, Verdict::Idle);
        assert!(s.reasons.iter().any(|r| r.contains("window open but untouched")));
        assert!(s.reasons.iter().any(|r| r.contains("3 idle connection")));
    }

    /// A mapped window alone must not mean "in use" — under Wayland, wmctrl
    /// sees only XWayland clients, so window presence is a weak positive signal
    /// and its absence proves nothing.
    #[test]
    fn a_mapped_window_alone_does_not_make_a_process_in_use() {
        let s = warm("Discord", |s| s.has_window = true);
        assert_eq!(s.verdict, Verdict::Idle);
    }

    // ── Fail-closed ─────────────────────────────────────────────────────────

    /// v1's `test_nothing_is_idle_before_two_samples`.
    #[test]
    fn nothing_is_idle_before_two_samples() {
        let s = sig("Discord", false, |_| {});
        assert_eq!(s.verdict, Verdict::InUse);
        assert!(s.reasons[0].contains("still sampling"));
    }

    #[test]
    fn a_cold_start_does_not_mask_real_activity() {
        // Even unwarmed, a focused process reports the real reason.
        let s = sig("Discord", false, |s| s.is_focused = true);
        assert_eq!(s.verdict, Verdict::InUse);
        assert!(s.reasons[0].contains("looking at"));
    }

    // ── Reclaim eligibility ─────────────────────────────────────────────────

    /// v1's `test_stopped_processes_are_not_offered_again`.
    #[test]
    fn an_already_stopped_process_is_not_worth_reclaiming() {
        let s = warm("Discord", |s| s.state = 'T');
        assert!(s.is_stopped());
        assert!(!s.worth_reclaiming());
    }

    /// v1's `test_small_processes_are_not_worth_the_risk`.
    #[test]
    fn a_small_process_is_not_worth_the_risk() {
        let s = warm("Discord", |s| s.pss = 20 * 1024 * 1024);
        assert_eq!(s.verdict, Verdict::Idle);
        assert!(!s.worth_reclaiming(), "20 MB is below the floor");
    }

    #[test]
    fn the_floor_is_measured_in_pss_not_rss() {
        // A process mapping 500 MB but owning only 100 MB of it is below the bar.
        let s = warm("Discord", |s| {
            s.rss = 500 * 1024 * 1024;
            s.pss = 100 * 1024 * 1024;
        });
        assert!(!s.worth_reclaiming());
    }

    // ── Tree propagation seeds ──────────────────────────────────────────────

    #[test]
    fn self_active_counts_real_work_not_mere_presence() {
        assert!(warm("x", |s| s.is_focused = true).self_active());
        assert!(warm("x", |s| s.playing_audio = true).self_active());
        assert!(warm("x", |s| s.cpu_seconds_recent = 1.0).self_active());
        // Presence signals are not seeds.
        assert!(!warm("x", |s| s.has_window = true).self_active());
        assert!(!warm("x", |s| s.established = 10).self_active());
        assert!(!warm("x", |_| {}).self_active());
    }

    #[test]
    fn structural_roles_are_always_their_own_activity_seed() {
        for name in ["cargo", "dockerd", "qemu-system-x86_64", "claude", "syncthing", "mpv"] {
            assert!(warm(name, |_| {}).self_active(), "{name}");
        }
        // A terminal is protected but is not a seed: an idle shell should not
        // mark its ancestors busy.
        assert!(!warm("cosmic-term", |_| {}).self_active());
    }

    #[test]
    fn rescoring_does_not_accumulate_stale_reasons() {
        let mut s = Signals::new(1, "Discord", Role::App);
        s.age_minutes = 600.0;
        s.pss = 500 * 1024 * 1024;
        s.is_focused = true;
        s.score(true);
        assert_eq!(s.reasons.len(), 1);

        s.is_focused = false;
        s.score(true);
        assert_eq!(s.verdict, Verdict::Idle);
        assert!(
            !s.reasons.iter().any(|r| r.contains("looking at")),
            "stale reason survived: {:?}",
            s.reasons
        );
    }

    #[test]
    fn verdict_strings_match_what_v1_emitted_over_the_api() {
        assert_eq!(Verdict::Protected.as_str(), "PROTECTED");
        assert_eq!(Verdict::InUse.as_str(), "IN_USE");
        assert_eq!(Verdict::Idle.as_str(), "IDLE");
    }
}
