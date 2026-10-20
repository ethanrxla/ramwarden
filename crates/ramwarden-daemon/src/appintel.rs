//! Jev for whole applications: is this one needed, and is it safe to quit?
//!
//! Tabs were the easy case. Apps are where the memory is (ChatGPT, Chrome and
//! Claude are most of this machine), and on macOS the only thing that frees an
//! app's memory is the app quitting. So the question Jev answers here is not
//! "freeze or not" but "would the user mind if this went away now".
//!
//! # Who decides what
//!
//! 1. **The floor is deterministic and never asks Jev.** An app not on the
//!    `[quit] allow` list, one the user is looking at, one playing audio, one
//!    with a working child, anything structurally protected (terminals, agent
//!    sessions, system services, builds), and Tailscale are refused before any
//!    model is consulted. A wrong model answer cannot reach them.
//! 2. **Jev must say yes.** With no answer cached the app stays: this tier is
//!    "Jev-vetted", not a rules-only quit.
//! 3. **Jev can say no for reasons the rules cannot see.** "Needed right now"
//!    covers servers, long sessions and anything a person leaves running on
//!    purpose; a high answer vetoes. "Unsaved work" vetoes the same way.
//! 4. The remaining yes is blended with a rules estimate (how long the app has
//!    been quiet, whether it has a window) on the log-odds scale, the same
//!    fusion the tab tier uses.
//!
//! What leaves the machine is the app's display name and coarse features (idle
//! bucket, size bucket, number of windows and ports, which parts it has), never
//! window titles, documents or URLs.

use std::time::{Duration, Instant};

use ramwarden_ai::jev::Question;
use ramwarden_core::config;
use ramwarden_core::roles::Role;
use ramwarden_core::signals::{CPU_BUSY_SECONDS, Protection, Signals};

use crate::tabintel::{TabIntel, blend};

pub const Q_NEEDED: &str = "needed_now";
pub const Q_QUIT: &str = "safe_quit";
pub const Q_RELAUNCH: &str = "relaunch_24h";
pub const Q_UNSAVED: &str = "unsaved";

pub fn questions() -> Vec<Question> {
    vec![
        Question::new(Q_NEEDED, "Is this a server, terminal session, build, sync, or other process that the user needs to leave running right now, even though they are not looking at it?"),
        Question::new(Q_QUIT, "Would the user be fine with this application being quit now, because it is idle and they are unlikely to notice or mind?"),
        Question::new(Q_RELAUNCH, "Will the user open this application again within the next 24 hours?"),
        Question::new(Q_UNSAVED, "Could this application hold unsaved work, a draft, or an in-progress task that would be lost if it quit?"),
    ]
}

#[derive(Clone, Debug, PartialEq)]
pub struct AppJudgement {
    pub needed: f64,
    pub safe_quit: f64,
    pub relaunch: f64,
    pub unsaved: f64,
    pub model: String,
}

/// Everything about one app that may be described to a model.
#[derive(Clone, Debug, PartialEq)]
pub struct AppFeatures {
    pub app: String,
    pub quiet_minutes: f64,
    pub rss_mb: f64,
    pub has_window: bool,
    pub ports: usize,
    pub has_tty: bool,
    pub established: u32,
    /// The two biggest parts, e.g. "page renderer x6".
    pub parts: Vec<String>,
}

fn quiet_bucket(m: f64) -> &'static str {
    match m {
        m if m < 10.0 => "under 10 minutes",
        m if m < 30.0 => "10 to 30 minutes",
        m if m < 120.0 => "30 minutes to 2 hours",
        m if m < 480.0 => "2 to 8 hours",
        _ => "over 8 hours",
    }
}

fn size_bucket(mb: f64) -> &'static str {
    match mb {
        m if m < 150.0 => "small (under 150 MB)",
        m if m < 500.0 => "medium (150 to 500 MB)",
        m if m < 1500.0 => "large (0.5 to 1.5 GB)",
        _ => "very large (over 1.5 GB)",
    }
}

pub fn describe(f: &AppFeatures) -> String {
    format!(
        "The desktop application \"{}\" on a Mac. It has shown no activity for {}. Memory: {}. {} {} {} Made of: {}.",
        f.app,
        quiet_bucket(f.quiet_minutes),
        size_bucket(f.rss_mb),
        if f.has_window { "It has an open window." } else { "It has no open window." },
        match f.ports {
            0 => "It is not listening on any network port.".to_string(),
            n => format!("It is listening on {n} network port(s), so something may connect to it."),
        },
        if f.has_tty { "It is attached to a terminal." } else if f.established > 0 { "It holds open network connections." } else { "It has no open connections." },
        if f.parts.is_empty() { "one process".to_string() } else { f.parts.join(", ") },
    )
}

/// Pure: build the features of one app from its processes.
pub fn features(app: &str, procs: &[&Signals], quiet_minutes: f64) -> AppFeatures {
    let mut by_part: std::collections::BTreeMap<&str, (usize, f64)> = Default::default();
    for s in procs {
        let e = by_part.entry(s.part).or_default();
        e.0 += 1;
        e.1 += s.rss as f64;
    }
    let mut parts: Vec<(&str, usize, f64)> = by_part.into_iter().map(|(p, (n, b))| (p, n, b)).collect();
    parts.sort_by(|a, b| b.2.total_cmp(&a.2));
    AppFeatures {
        app: app.to_string(),
        quiet_minutes,
        rss_mb: procs.iter().map(|s| s.rss as f64).sum::<f64>() / (1024.0 * 1024.0),
        has_window: procs.iter().any(|s| s.has_window),
        ports: procs.iter().map(|s| s.listening_ports.len()).sum(),
        has_tty: procs.iter().any(|s| s.has_tty),
        established: procs.iter().map(|s| s.established).sum(),
        parts: parts.iter().take(2).map(|(p, n, _)| format!("{p} x{n}")).collect(),
    }
}

/// Rules-only estimate that the app is safe to quit: rises with quiet time and
/// falls when a window is open. Deliberately modest, since Jev must still agree.
pub fn rules_p(f: &AppFeatures) -> f64 {
    let quiet = 1.0 / (1.0 + (-(f.quiet_minutes - 60.0) / 30.0).exp());
    let window = if f.has_window { 0.7 } else { 1.0 };
    (quiet * window).clamp(0.02, 0.98)
}

/// Why this app must stay, decided without any model; `None` means it may go on
/// to Jev. The order is the order of the user's trust: first the explicit list,
/// then things a person is plainly using, then what the app *is*.
pub fn floor(app: &str, procs: &[&Signals], quiet_minutes: f64, cfg: &config::Quit) -> Option<String> {
    if !cfg.allow.iter().any(|a| a.eq_ignore_ascii_case(app)) {
        return Some("not on the quit list".into());
    }
    if app.to_lowercase().contains("tailscale") {
        return Some("Tailscale is never touched".into());
    }
    if procs.iter().any(|s| s.is_focused) {
        return Some("you are using it now".into());
    }
    if procs.iter().any(|s| s.playing_audio) {
        return Some("playing or recording audio".into());
    }
    if let Some(d) = procs.iter().find_map(|s| s.active_descendant.as_ref()) {
        return Some(format!("a child process is working ({d})"));
    }
    if procs.iter().any(|s| s.cpu_seconds_recent >= CPU_BUSY_SECONDS) {
        return Some("working right now".into());
    }
    if let Some(s) = procs.iter().find(|s| s.protection == Protection::Structural || s.role.is_structural() || s.role == Role::Compositor) {
        return Some(s.role.protection_reason().to_string());
    }
    if quiet_minutes < cfg.min_quiet_minutes {
        return Some(format!("only quiet for {quiet_minutes:.0} min"));
    }
    None
}

#[derive(Clone, Debug, PartialEq)]
pub enum Decision {
    /// Quit it, with the blended probability that this is safe.
    Quit { p: f64 },
    /// Leave it, and why. Shown to the user, so phrased for them.
    Keep(String),
}

/// Pure: the Jev half of the decision, after the floor has passed.
pub fn judge(f: &AppFeatures, j: Option<&AppJudgement>, quit: &config::Quit, jev: &config::Jev) -> Decision {
    let Some(j) = j else { return Decision::Keep("waiting for Jev".into()) };
    if j.needed >= quit.needed_veto {
        return Decision::Keep(format!("Jev says it is needed right now ({:.0}%)", j.needed * 100.0));
    }
    if j.unsaved >= jev.unsaved_veto {
        return Decision::Keep(format!("Jev says it may hold unsaved work ({:.0}%)", j.unsaved * 100.0));
    }
    let p = blend(rules_p(f), j.safe_quit, jev.weight);
    if p >= quit.threshold { Decision::Quit { p } } else { Decision::Keep(format!("Jev is not convinced ({:.0}%)", p * 100.0)) }
}

impl TabIntel {
    /// A fresh app answer, if cached. Never blocks.
    pub fn lookup_app(&self, state: &str, now: Instant) -> Option<AppJudgement> {
        let ttl = Duration::from_secs(self.config().cache_hours * 3600);
        let c = self.app_cache.lock().unwrap();
        c.get(state).filter(|(_, at)| now.duration_since(*at) < ttl).map(|(j, _)| j.clone())
    }

    /// Ask about each uncached app description and cache the answers, within the
    /// shared hourly budget. Returns how many were answered.
    pub async fn refresh_apps(&self, states: &[String], now: Instant) -> usize {
        let Some(asker) = &self.asker else { return 0 };
        let qs = questions();
        let mut seen = std::collections::HashSet::new();
        let todo: Vec<&String> = states
            .iter()
            .filter(|s| seen.insert(s.as_str()) && self.lookup_app(s, now).is_none())
            .take(self.call_room(now))
            .collect();
        let mut answered = 0;
        for state in todo {
            self.note_call(now);
            match asker.ask(state, &qs).await {
                Ok(a) => {
                    self.note_ok(&a);
                    let g = |k: &str| a.noul.get(k).copied();
                    let (Some(needed), Some(safe_quit), Some(relaunch), Some(unsaved)) =
                        (g(Q_NEEDED), g(Q_QUIT), g(Q_RELAUNCH), g(Q_UNSAVED))
                    else {
                        tracing::warn!("jev app reply was missing an answer; ignoring it");
                        continue;
                    };
                    self.app_cache
                        .lock()
                        .unwrap()
                        .insert(state.clone(), (AppJudgement { needed, safe_quit, relaunch, unsaved, model: a.model }, now));
                    answered += 1;
                }
                Err(e) => {
                    self.record_failure(&e, now);
                    if self.breaker_open(now) {
                        break;
                    }
                }
            }
        }
        answered
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ramwarden_core::roles::classify;

    fn quit_cfg() -> config::Quit {
        config::Quit { mode: "live".into(), allow: vec!["zoom.us".into(), "Music".into(), "Terminal".into(), "Tailscale".into()], ..config::Quit::default() }
    }

    fn proc(app: &str) -> Signals {
        let mut s = Signals::new(1, app, classify(app, "", None));
        s.app = app.into();
        s.rss = 280 * 1024 * 1024;
        s.score(true);
        s
    }

    fn judgement(needed: f64, quit: f64, unsaved: f64) -> AppJudgement {
        AppJudgement { needed, safe_quit: quit, relaunch: 0.2, unsaved, model: "jev-test".into() }
    }

    #[test]
    fn the_description_has_the_app_and_coarse_features_but_no_titles_or_numbers_that_identify() {
        let s = proc("zoom.us");
        let d = describe(&features("zoom.us", &[&s], 95.0));
        assert!(d.contains("\"zoom.us\"") && d.contains("30 minutes to 2 hours") && d.contains("medium (150 to 500 MB)"));
        assert!(!d.contains("280"), "exact sizes stay on the machine");
    }

    #[test]
    fn an_app_off_the_list_is_never_a_candidate() {
        let s = proc("Safari");
        assert_eq!(floor("Safari", &[&s], 999.0, &quit_cfg()).as_deref(), Some("not on the quit list"));
    }

    #[test]
    fn the_floor_refuses_what_a_person_is_using_whatever_jev_would_say() {
        let cfg = quit_cfg();
        let mut s = proc("zoom.us");
        s.is_focused = true;
        assert!(floor("zoom.us", &[&s], 999.0, &cfg).unwrap().contains("using it now"));
        let mut a = proc("Music");
        a.playing_audio = true;
        assert!(floor("Music", &[&a], 999.0, &cfg).unwrap().contains("audio"));
        let mut w = proc("zoom.us");
        w.active_descendant = Some("ffmpeg".into());
        assert!(floor("zoom.us", &[&w], 999.0, &cfg).unwrap().contains("ffmpeg"));
        let mut b = proc("zoom.us");
        b.cpu_seconds_recent = 5.0;
        assert!(floor("zoom.us", &[&b], 999.0, &cfg).unwrap().contains("working"));
    }

    #[test]
    fn terminals_and_tailscale_stay_even_when_listed() {
        let cfg = quit_cfg();
        let t = proc("Terminal");
        assert!(floor("Terminal", &[&t], 999.0, &cfg).is_some(), "structural role holds");
        let ts = proc("Tailscale");
        assert_eq!(floor("Tailscale", &[&ts], 999.0, &cfg).as_deref(), Some("Tailscale is never touched"));
    }

    #[test]
    fn an_app_must_have_been_quiet_long_enough() {
        let s = proc("zoom.us");
        assert!(floor("zoom.us", &[&s], 5.0, &quit_cfg()).unwrap().contains("only quiet for 5 min"));
        assert_eq!(floor("zoom.us", &[&s], 45.0, &quit_cfg()), None);
    }

    #[test]
    fn without_a_jev_answer_the_app_stays() {
        let f = features("zoom.us", &[&proc("zoom.us")], 300.0);
        assert_eq!(judge(&f, None, &quit_cfg(), &config::Jev::default()), Decision::Keep("waiting for Jev".into()));
    }

    #[test]
    fn jev_can_veto_for_need_or_unsaved_work_and_can_approve() {
        let f = features("zoom.us", &[&proc("zoom.us")], 300.0);
        let (q, jev) = (quit_cfg(), config::Jev { weight: 0.7, ..config::Jev::default() });
        assert!(matches!(judge(&f, Some(&judgement(0.9, 0.99, 0.0)), &q, &jev), Decision::Keep(r) if r.contains("needed")));
        assert!(matches!(judge(&f, Some(&judgement(0.1, 0.99, 0.9)), &q, &jev), Decision::Keep(r) if r.contains("unsaved")));
        assert!(matches!(judge(&f, Some(&judgement(0.05, 0.95, 0.05)), &q, &jev), Decision::Quit { p } if p > 0.7));
        assert!(matches!(judge(&f, Some(&judgement(0.05, 0.3, 0.05)), &q, &jev), Decision::Keep(r) if r.contains("not convinced")));
    }

    #[test]
    fn longer_quiet_and_no_window_raise_the_rules_estimate() {
        let a = features("x", &[&proc("x")], 20.0);
        let b = features("x", &[&proc("x")], 240.0);
        assert!(rules_p(&b) > rules_p(&a));
        let mut windowed = proc("x");
        windowed.has_window = true;
        assert!(rules_p(&features("x", &[&windowed], 240.0)) < rules_p(&b));
    }
}
