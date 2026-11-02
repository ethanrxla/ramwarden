//! Writes the decision log (`ramwarden_core::decisions`) from the daemon's
//! own types, and serves it at `GET /decisions` for local debugging.
//!
//! Each tab or app RamWarden acts on gets one row with the numbers behind the
//! choice and Jev's answer as it stood at that moment. Rows are written after
//! the browser or app has answered, so `result` says what happened (confirmed,
//! refused, dry-run), not what was attempted.
//!
//! Nothing here talks to a network. `ramwarden-cloud-db decisions sync` copies
//! the table to MongoDB on its own schedule; with no sync configured, the log
//! is still complete on this machine.

use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use ramwarden_core::config;
use ramwarden_core::decisions::{DecisionQuery, NewDecision, TabVector};
use ramwarden_core::history::{History, unix_now};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::browser::{Row, Target};
use crate::hub::{AppState, DiscardResult};
use crate::tabintel;

/// Local decisions older than this are deleted. Synced copies are kept by Atlas.
pub const KEEP_DAYS: i64 = 90;

/// The full host, lowercased, `www.` dropped: `docs.google.com`, not the
/// registrable `google.com`, because "which Google tab?" is the question.
pub fn host_of(url: &str) -> String {
    let host = url::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_ascii_lowercase)).unwrap_or_default();
    host.strip_prefix("www.").map(str::to_string).unwrap_or(host)
}

fn utc_hour(now: i64) -> u32 {
    (now.rem_euclid(86_400) / 3600) as u32
}

impl Row {
    /// What Jev said about this tab, if it was in the band Jev is asked about.
    /// `None` when the model tier is off: the closed form decided alone.
    pub fn jev_record(&self) -> Option<Value> {
        let intel = tabintel::global().filter(|i| i.active())?;
        let cfg = intel.config();
        let features = tabintel::Features {
            class: self.class,
            idle_minutes: self.inactive_minutes,
            frecency: self.frecency,
            related: self.related,
            duplicate: self.duplicate,
        };
        if self.p_rules < cfg.band_low || self.p_rules > cfg.band_high {
            return Some(json!({
                "consulted": false,
                "why": format!("rules were sure ({:.2} outside {:.2}-{:.2})", self.p_rules, cfg.band_low, cfg.band_high),
            }));
        }
        let state = tabintel::describe(&features);
        Some(match intel.lookup(&state, std::time::Instant::now()) {
            Some(j) => json!({
                "consulted": true,
                "model": j.model,
                "asked": state,
                "return_24h": j.return_24h,
                "unsaved": j.unsaved,
                "weight": cfg.weight,
                "veto": j.unsaved >= cfg.unsaved_veto,
                "blended": self.p_return,
            }),
            None => json!({ "consulted": false, "why": "borderline, no answer cached yet", "asked": state }),
        })
    }

    /// The row as a decision. `reason` is the sentence a person reads first.
    pub fn decision(&self, action: &str, result: &str, trigger: &str, reason: String, now: i64) -> NewDecision {
        let jev = self.jev_record();
        let unsaved = jev.as_ref().and_then(|j| j["unsaved"].as_f64());
        NewDecision {
            kind: "tab".into(),
            action: action.into(),
            result: result.into(),
            subject: host_of(&self.target.url),
            title: self.title.clone(),
            url: self.target.url.clone(),
            browser: self.target.browser.clone(),
            tab_id: Some(self.target.id),
            trigger: trigger.into(),
            reason,
            inputs: json!({
                "idle_minutes": self.inactive_minutes,
                "status": self.status,
                "status_reason": self.reason,
                "p_return": self.p_return,
                "p_rules": self.p_rules,
                "p_model": self.p_model,
                "est_mb": self.est_mb,
                "frecency": self.frecency,
                "class": self.class,
                "related": self.related,
                "duplicate": self.duplicate,
                "priority": self.priority,
                "site": crate::tabrank::site_key(&self.target.url),
            }),
            vector: TabVector {
                class: self.class,
                idle_minutes: self.inactive_minutes,
                p_rules: self.p_rules,
                p_model: self.p_model,
                p_return: self.p_return,
                est_mb: self.est_mb,
                frecency: self.frecency,
                related: self.related,
                duplicate: self.duplicate,
                unsaved,
                hour: utc_hour(now),
            }
            .encode(),
            jev,
            bytes: 0,
        }
    }

    /// Why the sweep closes this tab: the rule in `browser::close_candidates`,
    /// with this tab's numbers.
    pub fn close_reason(&self, cfg: &config::Tabs) -> String {
        format!(
            "idle {:.1} h (floor {:.0} h), return chance {:.2} below {:.2}{}",
            self.inactive_minutes as f64 / 60.0,
            cfg.close_after_hours.max(24.0),
            self.p_return,
            cfg.close_below_return.min(0.3),
            if self.duplicate { "; older copy of an open page" } else { "" },
        )
    }

    pub fn unload_reason(&self) -> String {
        format!(
            "idle {} min, return chance {:.2}, about {} MB; unloaded, tab stays in the strip",
            self.inactive_minutes, self.p_return, self.est_mb
        )
    }
}

/// Which of `result`'s targets went which way, as decision `result` values.
fn outcomes(result: &DiscardResult) -> impl Iterator<Item = (&Target, &'static str)> {
    result
        .confirmed
        .iter()
        .map(|t| (t, "confirmed"))
        .chain(result.queued.iter().map(|t| (t, "queued")))
        .chain(result.refused.iter().map(|t| (t, "refused")))
}

/// Record one batch: every target in `result`, described by its row when the
/// snapshot has it. Returns how many rows were written.
pub fn record_batch(
    h: &History,
    rows: &HashMap<Target, Row>,
    result: &DiscardResult,
    action: &str,
    trigger: &str,
    reason: impl Fn(&Row) -> String,
) -> usize {
    let now = unix_now() as i64;
    outcomes(result)
        .filter_map(|(t, res)| {
            let d = match rows.get(t) {
                Some(r) => r.decision(action, res, trigger, reason(r), now),
                // No row: the tab left the report between choosing and acting.
                None => NewDecision {
                    kind: "tab".into(),
                    action: action.into(),
                    result: res.into(),
                    subject: host_of(&t.url),
                    url: t.url.clone(),
                    browser: t.browser.clone(),
                    tab_id: Some(t.id),
                    trigger: trigger.into(),
                    reason: "tab was not in the latest report".into(),
                    inputs: json!({}),
                    ..Default::default()
                },
            };
            h.record_decision(&d, now).ok()
        })
        .count()
}

/// Dry-run closes: what a live sweep would have closed, logged the same way.
pub fn record_dry_run(h: &History, rows: &HashMap<Target, Row>, targets: &[Target], trigger: &str, cfg: &config::Tabs) -> usize {
    let now = unix_now() as i64;
    targets
        .iter()
        .filter_map(|t| rows.get(t))
        .filter_map(|r| h.record_decision(&r.decision("close", "dry_run", trigger, r.close_reason(cfg), now), now).ok())
        .count()
}

/// The user reported reopening `url`: attribute it to the close it undoes.
pub fn note_reopened(h: &History, url: &str) {
    let subject = host_of(url);
    match h.mark_close_reopened(url, &subject, unix_now() as i64) {
        Ok(Some(id)) => tracing::info!("decision {id}: the user reopened a tab RamWarden closed ({subject})"),
        Ok(None) => {}
        Err(e) => tracing::debug!("could not mark reopen: {e}"),
    }
}

/// The current report's rows by target, taken before a manual action so the
/// log has the tab's numbers even after the tab is gone.
pub fn rows_by_target(st: &AppState) -> HashMap<Target, Row> {
    crate::tabreview::snapshot(st).into_iter().map(|r| (r.target.clone(), r)).collect()
}

/// A manual unload or close from the popup, the app or the API.
pub fn record_manual(st: &AppState, rows: &HashMap<Target, Row>, result: &DiscardResult, action: &str) {
    if let Ok(h) = st.history.lock() {
        record_batch(&h, rows, result, action, "manual", |_| format!("you chose to {action} it"));
    }
}

/// Tabs the user ticked "keep" in the close review: the strongest label there is.
pub fn record_user_keep(st: &AppState, offered: &[Row], kept: &[Target]) {
    let Ok(h) = st.history.lock() else { return };
    let now = unix_now() as i64;
    for r in offered.iter().filter(|r| kept.contains(&r.target)) {
        let _ = h.record_decision(&r.decision("keep", "kept", "user", "you chose to keep it in the close review".into(), now), now);
    }
}

// ── Apps ───────────────────────────────────────────────────────────────────

/// An app quit (or a decision to keep a Jev-judged app), with Jev's answers.
pub fn app_decision(
    f: &crate::appintel::AppFeatures,
    j: Option<&crate::appintel::AppJudgement>,
    action: &str,
    result: &str,
    trigger: &str,
    reason: String,
) -> NewDecision {
    NewDecision {
        kind: "app".into(),
        action: action.into(),
        result: result.into(),
        subject: f.app.clone(),
        trigger: trigger.into(),
        reason,
        inputs: json!({
            "quiet_minutes": f.quiet_minutes,
            "rss_mb": f.rss_mb,
            "has_window": f.has_window,
            "ports": f.ports,
            "has_tty": f.has_tty,
            "established": f.established,
            "runaway_minutes": f.runaway_minutes,
            "parts": f.parts,
            "p_rules": crate::appintel::rules_p(f),
        }),
        jev: j.map(|j| {
            json!({
                "consulted": true,
                "model": j.model,
                "asked": crate::appintel::describe(f),
                "needed": j.needed,
                "safe_quit": j.safe_quit,
                "relaunch": j.relaunch,
                "unsaved": j.unsaved,
            })
        }),
        bytes: 0,
        ..Default::default()
    }
}

// ── HTTP ───────────────────────────────────────────────────────────────────

#[derive(Deserialize, Default)]
struct Filter {
    kind: Option<String>,
    action: Option<String>,
    /// Substring of host, app name or title.
    q: Option<String>,
    limit: Option<u32>,
}

pub fn routes() -> Router<AppState> {
    Router::new().route("/decisions", get(list)).route("/decisions/{id}", get(one))
}

async fn list(State(st): State<AppState>, Query(f): Query<Filter>) -> Json<Value> {
    let h = st.history.lock().unwrap();
    let q = DecisionQuery { kind: f.kind, action: f.action, text: f.q, limit: f.limit.unwrap_or(100) };
    let rows = h.recent_decisions(&q).unwrap_or_default();
    Json(json!({ "count": h.count_decisions().unwrap_or(0), "decisions": rows }))
}

async fn one(State(st): State<AppState>, Path(id): Path<i64>) -> Result<Json<Value>, StatusCode> {
    let h = st.history.lock().unwrap();
    match h.decision(id) {
        Ok(Some(d)) => Ok(Json(json!(d))),
        Ok(None) => Err(StatusCode::NOT_FOUND),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: i64, url: &str) -> Row {
        let tab: crate::tabs::Tab = serde_json::from_value(json!({
            "id": id, "url": url, "title": "Quarterly plan", "inactiveMinutes": 1800,
            "active": false, "pinned": false, "audible": false, "discarded": false,
            "autoDiscardable": true, "status": "complete", "discardSupported": true
        }))
        .unwrap();
        crate::browser::rank("chrome", &[tab], 30).remove(0)
    }

    #[test]
    fn a_tab_decision_carries_its_numbers_and_never_claims_success_it_did_not_get() {
        let h = History::in_memory().unwrap();
        let r = row(4, "https://docs.example.com/plan");
        let mut rows = HashMap::new();
        rows.insert(r.target.clone(), r.clone());
        let result = DiscardResult { refused: vec![r.target.clone()], ..Default::default() };
        assert_eq!(record_batch(&h, &rows, &result, "close", "sweep", |r| r.close_reason(&config::Tabs::default())), 1);

        let d = &h.recent_decisions(&DecisionQuery { limit: 5, ..Default::default() }).unwrap()[0];
        assert_eq!(d.result, "refused");
        assert_eq!(d.subject, "docs.example.com");
        assert_eq!(d.title, "Quarterly plan");
        assert_eq!(d.inputs["idle_minutes"], 1800);
        assert!(d.reason.contains("idle 30.0 h"), "{}", d.reason);
        assert_eq!(d.vector.len(), TabVector::DIMS);
    }

    #[test]
    fn a_target_missing_from_the_report_is_still_logged() {
        let h = History::in_memory().unwrap();
        let t = Target { browser: "chrome".into(), id: 9, url: "https://x.test/".into() };
        let result = DiscardResult { confirmed: vec![t], ..Default::default() };
        assert_eq!(record_batch(&h, &HashMap::new(), &result, "unload", "pressure", |r| r.unload_reason()), 1);
        let d = &h.recent_decisions(&DecisionQuery { limit: 5, ..Default::default() }).unwrap()[0];
        assert_eq!(d.reason, "tab was not in the latest report");
    }

    #[test]
    fn dry_runs_are_logged_and_a_reopen_finds_only_live_closes() {
        let h = History::in_memory().unwrap();
        let r = row(4, "https://docs.example.com/plan");
        let rows: HashMap<_, _> = [(r.target.clone(), r.clone())].into();
        assert_eq!(record_dry_run(&h, &rows, std::slice::from_ref(&r.target), "sweep", &config::Tabs::default()), 1);
        note_reopened(&h, "https://docs.example.com/plan");
        let d = &h.recent_decisions(&DecisionQuery { limit: 5, ..Default::default() }).unwrap()[0];
        assert_eq!(d.result, "dry_run");
        assert_eq!(d.outcome, None);

        let live = DiscardResult { confirmed: vec![r.target.clone()], ..Default::default() };
        record_batch(&h, &rows, &live, "close", "sweep", |r| r.close_reason(&config::Tabs::default()));
        note_reopened(&h, "https://docs.example.com/plan");
        let d = &h.recent_decisions(&DecisionQuery { action: Some("close".into()), limit: 1, ..Default::default() }).unwrap()[0];
        assert_eq!(d.outcome.as_deref(), Some("reopened"));
    }
}
