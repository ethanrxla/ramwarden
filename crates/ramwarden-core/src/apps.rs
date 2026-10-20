//! Per-application view of the process table.
//!
//! The detector scores processes one by one, but nobody thinks in processes:
//! "ChatGPT is using 2.3 GB" is the useful fact, and so is *how much of it is
//! doing anything*. This groups the scored processes by owning app and splits
//! each app's memory into in-use, idle and protected, with a labelled breakdown
//! of its parts, so the biggest quiet consumers are obvious.

use std::collections::{BTreeMap, HashMap};
use std::time::Instant;

use crate::detector::Detector;
use crate::signals::{Signals, Verdict};

const MB: f64 = 1024.0 * 1024.0;

/// What one part of an app (all its page renderers, say) is costing.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct PartStat {
    pub part: &'static str,
    pub procs: usize,
    pub mb: f64,
    pub idle_mb: f64,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct AppStat {
    pub app: String,
    pub procs: usize,
    /// Resident size, the figure Activity Monitor-style tools show.
    pub rss_mb: f64,
    /// Resident memory in processes with each verdict.
    pub in_use_mb: f64,
    pub idle_mb: f64,
    pub protected_mb: f64,
    pub focused: bool,
    pub has_window: bool,
    pub listening_ports: Vec<u16>,
    /// Minutes since any part of the app did anything, if known.
    pub quiet_minutes: Option<f64>,
    /// Distinct reasons processes of this app are protected or in use.
    pub reasons: Vec<String>,
    pub parts: Vec<PartStat>,
}

/// Resident size. On macOS the proportional figure is an estimate that overstates
/// shared-heavy apps (Chrome read 2x its resident size), so resident is used
/// unless it is unavailable.
fn size(s: &Signals) -> f64 {
    (if s.rss > 0 { s.rss } else { s.pss }) as f64 / MB
}

/// Group `sigs` by app, biggest first. `quiet` looks up per-app idle time.
pub fn summarise<'a>(
    sigs: impl IntoIterator<Item = &'a Signals>,
    quiet: impl Fn(&str) -> Option<f64>,
) -> Vec<AppStat> {
    let mut apps: HashMap<String, AppStat> = HashMap::new();
    let mut parts: HashMap<(String, &'static str), PartStat> = HashMap::new();
    for s in sigs {
        let a = apps.entry(s.app.clone()).or_insert_with(|| AppStat {
            app: s.app.clone(),
            procs: 0,
            rss_mb: 0.0,
            in_use_mb: 0.0,
            idle_mb: 0.0,
            protected_mb: 0.0,
            focused: false,
            has_window: false,
            listening_ports: Vec::new(),
            quiet_minutes: quiet(&s.app),
            reasons: Vec::new(),
            parts: Vec::new(),
        });
        let mb = size(s);
        a.procs += 1;
        a.rss_mb += s.rss as f64 / MB;
        match s.verdict {
            Verdict::InUse => a.in_use_mb += mb,
            Verdict::Idle => a.idle_mb += mb,
            Verdict::Protected => a.protected_mb += mb,
        }
        a.focused |= s.is_focused;
        a.has_window |= s.has_window;
        for p in &s.listening_ports {
            if !a.listening_ports.contains(p) {
                a.listening_ports.push(*p);
            }
        }
        if s.verdict != Verdict::Idle
            && let Some(r) = s.reasons.first()
            && !a.reasons.contains(r)
            && a.reasons.len() < 4
        {
            a.reasons.push(r.clone());
        }
        let p = parts.entry((s.app.clone(), s.part)).or_insert(PartStat { part: s.part, procs: 0, mb: 0.0, idle_mb: 0.0 });
        p.procs += 1;
        p.mb += mb;
        if s.verdict == Verdict::Idle {
            p.idle_mb += mb;
        }
    }
    let mut by_app: BTreeMap<String, Vec<PartStat>> = BTreeMap::new();
    for ((app, _), p) in parts {
        by_app.entry(app).or_default().push(p);
    }
    let mut out: Vec<AppStat> = apps.into_values().collect();
    for a in &mut out {
        let mut ps = by_app.remove(&a.app).unwrap_or_default();
        ps.sort_by(|x, y| y.mb.total_cmp(&x.mb));
        a.parts = ps;
    }
    out.sort_by(|x, y| (y.rss_mb).total_cmp(&x.rss_mb));
    out
}

/// The detector's current table, grouped.
pub fn from_detector(det: &Detector, now: Instant) -> Vec<AppStat> {
    summarise(det.snapshot().values(), |app| det.quiet_minutes(app, now))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::roles::classify;

    fn sig(pid: i32, app: &str, part: &'static str, mb: u64, verdict: Verdict) -> Signals {
        let mut s = Signals::new(pid, app, classify("x", "", None));
        s.app = app.into();
        s.part = part;
        s.rss = mb * 1024 * 1024;
        s.verdict = verdict;
        s
    }

    #[test]
    fn memory_is_split_by_verdict_and_apps_sorted_by_size() {
        let sigs = vec![
            sig(1, "ChatGPT", "main process", 300, Verdict::InUse),
            sig(2, "ChatGPT", "page renderer", 700, Verdict::Idle),
            sig(3, "ChatGPT", "page renderer", 500, Verdict::Idle),
            sig(4, "Notes", "main process", 90, Verdict::Idle),
        ];
        let apps = summarise(&sigs, |a| (a == "ChatGPT").then_some(42.0));
        assert_eq!(apps[0].app, "ChatGPT");
        assert_eq!(apps[0].procs, 3);
        assert_eq!((apps[0].in_use_mb, apps[0].idle_mb), (300.0, 1200.0));
        assert_eq!(apps[0].quiet_minutes, Some(42.0));
        assert_eq!(apps[1].app, "Notes");
        assert_eq!(apps[1].quiet_minutes, None);
    }

    #[test]
    fn parts_are_ranked_and_report_how_much_of_each_is_idle() {
        let sigs = vec![
            sig(1, "Claude", "main process", 100, Verdict::InUse),
            sig(2, "Claude", "page renderer", 700, Verdict::Idle),
            sig(3, "Claude", "page renderer", 300, Verdict::InUse),
        ];
        let p = &summarise(&sigs, |_| None)[0].parts;
        assert_eq!((p[0].part, p[0].procs, p[0].mb, p[0].idle_mb), ("page renderer", 2, 1000.0, 700.0));
        assert_eq!(p[1].part, "main process");
    }

    #[test]
    fn why_an_app_is_protected_is_kept_once_and_capped() {
        let mut a = sig(1, "Discord", "helper", 380, Verdict::Protected);
        a.reasons = vec!["serving on port 6463 — clients would hang".into()];
        a.listening_ports = vec![6463];
        let mut b = a.clone();
        b.pid = 2;
        let apps = summarise(&[a, b], |_| None);
        assert_eq!(apps[0].reasons.len(), 1);
        assert_eq!(apps[0].listening_ports, vec![6463]);
        assert_eq!(apps[0].protected_mb, 760.0);
    }
}
