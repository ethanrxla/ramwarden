//! Acting on app judgements: ask idle apps to quit, gently, while memory is high.
//!
//! The decisions live in [`crate::appintel`]; this is the loop that feeds them
//! and carries them out. It runs from the monitor, at most once a minute, and
//! only while memory used is above `[quit] target_used_percent`.
//!
//! * `mode = "dry-run"` logs and records what it *would* quit, so the first
//!   hours can be read before anything is closed.
//! * `mode = "live"` sends the app its own quit request (`tell application ... to
//!   quit`), never a signal. An app with unsaved work shows its save prompt and
//!   stays; if it has not quit when the request times out we leave it.
//! * One quit per pass, biggest first, so memory is re-read before the next.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use ramwarden_core::history::NewAction;

use crate::appintel::{self, Decision};
use crate::hub::AppState;

pub struct Quitter {
    /// The last reason each allow-listed app was kept, so a refusal is logged
    /// when it changes rather than every minute.
    kept: std::collections::HashMap<String, String>,
    /// When a dry-run "would quit" was last announced per app: nothing is acted
    /// on in dry-run, so the same line every minute is noise.
    announced: std::collections::HashMap<String, Instant>,
    last: Instant,
    quits: VecDeque<Instant>,
    in_flight: Arc<AtomicBool>,
}

impl Quitter {
    pub fn new() -> Self {
        Quitter { kept: Default::default(), announced: Default::default(), last: Instant::now() - Duration::from_secs(3600), quits: VecDeque::new(), in_flight: Arc::new(AtomicBool::new(false)) }
    }
}

impl Default for Quitter {
    fn default() -> Self {
        Quitter::new()
    }
}

/// Log why an allow-listed app was kept, once per change of reason.
fn note_kept(kept: &mut std::collections::HashMap<String, String>, app: &str, why: &str) {
    if kept.get(app).map(String::as_str) != Some(why) {
        tracing::info!("quit: kept {app}: {why}");
        kept.insert(app.to_string(), why.to_string());
    }
}

/// AppleScript string literal for an app name: quotes and backslashes escaped.
fn script_quote(name: &str) -> String {
    format!("\"{}\"", name.replace('\\', "\\\\").replace('"', "\\\""))
}

pub fn consider(state: &AppState, handle: &tokio::runtime::Handle, q: &mut Quitter) {
    let cfg = &state.cfg.quit;
    if cfg.mode != "dry-run" && cfg.mode != "live" {
        return;
    }
    let Some(intel) = crate::tabintel::global().filter(|i| i.active()) else { return };
    if q.last.elapsed() < Duration::from_secs(60) || state.on_hold() {
        return;
    }
    q.last = Instant::now();

    let mem = state.memory();
    if mem.total == 0 {
        return;
    }
    let used_pct = mem.used() as f64 / mem.total as f64 * 100.0;
    if used_pct < cfg.target_used_percent {
        return;
    }

    // Candidates: allow-listed apps that pass the deterministic floor.
    let now = Instant::now();
    let mut cands: Vec<(String, appintel::AppFeatures, String)> = Vec::new();
    {
        let det = state.det.read().unwrap();
        let mut by_app: std::collections::HashMap<&str, Vec<&ramwarden_core::signals::Signals>> = Default::default();
        for s in det.snapshot().values() {
            by_app.entry(s.app.as_str()).or_default().push(s);
        }
        for (app, procs) in by_app {
            let quiet = det.quiet_minutes(app, now).unwrap_or(0.0);
            let runaway = det.runaway_minutes(app, now);
            if let Some(why) = appintel::floor(app, &procs, quiet, runaway, cfg) {
                // Only apps the user listed are worth a line; the rest are not candidates.
                if cfg.allow.iter().any(|a| a.eq_ignore_ascii_case(app)) {
                    note_kept(&mut q.kept, app, &why);
                }
                continue;
            }
            q.kept.remove(app);
            let f = appintel::features(app, &procs, quiet, runaway);
            let text = appintel::describe(&f);
            cands.push((app.to_string(), f, text));
        }
    }
    cands.sort_by(|a, b| b.1.rss_mb.total_cmp(&a.1.rss_mb));

    // Ask Jev about anything new, in the background; its answers land next pass.
    let states: Vec<String> = cands.iter().map(|c| c.2.clone()).collect();
    if !states.is_empty() && !q.in_flight.swap(true, Ordering::AcqRel) {
        let intel = intel.clone();
        let flag = q.in_flight.clone();
        handle.spawn(async move {
            let n = intel.refresh_apps(&states, Instant::now()).await;
            if n > 0 {
                tracing::info!("jev judged {n} app(s)");
            }
            flag.store(false, Ordering::Release);
        });
    }

    while q.quits.front().is_some_and(|t| t.elapsed() >= Duration::from_secs(3600)) {
        q.quits.pop_front();
    }
    if q.quits.len() as u32 >= cfg.max_per_hour {
        return;
    }

    let jev = intel.config();
    for (app, f, text) in &cands {
        let j = intel.lookup_app(text, now);
        match appintel::judge(f, j.as_ref(), cfg, jev) {
            Decision::Keep(why) => tracing::info!("quit: keep {app}: {why}"),
            Decision::Quit { p } => {
                let live = cfg.mode == "live";
                if !live && q.announced.get(app).is_some_and(|t| t.elapsed() < Duration::from_secs(30 * 60)) {
                    break;
                }
                if !live {
                    q.announced.insert(app.clone(), Instant::now());
                }
                let ok = live && ramwarden_core::exec::run("osascript", &["-e", &format!("tell application {} to quit", script_quote(app))]).is_some();
                tracing::info!(
                    "{} {app} ({:.0} MB, quiet {:.0} min, p={:.2}, memory {:.0}% used)",
                    if !live { "would quit" } else if ok { "asked to quit" } else { "quit request failed for" },
                    f.rss_mb, f.quiet_minutes, p, used_pct
                );
                if let Ok(h) = state.history.lock() {
                    let _ = h.log(&NewAction {
                        rung: -1,
                        trigger: format!("memory {used_pct:.0}% used"),
                        psi_some: 0.0,
                        psi_full: 0.0,
                        action: if live { "quit_app".into() } else { "quit_app_dry_run".into() },
                        target: app.clone(),
                        bytes_freed: if ok { (f.rss_mb * 1048576.0) as i64 } else { 0 },
                        succeeded: ok,
                        notes: format!("p={p:.2}; asked to quit, app may still hold a save prompt"),
                    });
                }
                if live && ok {
                    q.quits.push_back(Instant::now());
                }
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_is_remembered_so_it_is_logged_once_per_change() {
        let mut kept = Default::default();
        note_kept(&mut kept, "Console", "working right now");
        note_kept(&mut kept, "Console", "working right now");
        assert_eq!(kept.get("Console").map(String::as_str), Some("working right now"));
        note_kept(&mut kept, "Console", "only quiet for 3 min");
        assert_eq!(kept.get("Console").map(String::as_str), Some("only quiet for 3 min"));
    }

    #[test]
    fn app_names_are_quoted_for_applescript() {
        assert_eq!(script_quote("zoom.us"), "\"zoom.us\"");
        assert_eq!(script_quote("A\"B\\C"), "\"A\\\"B\\\\C\"");
    }
}
