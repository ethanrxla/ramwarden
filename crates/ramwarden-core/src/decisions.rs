//! The decision log: one row per thing RamWarden chose to do, or chose not to.
//!
//! # Why a second log next to `actions`
//!
//! `actions` answers "what did the ladder achieve?" in aggregate: one row per
//! rung, "3 unloaded, 1 closed". That is the right shape for totals and the
//! wrong one for debugging a single bad close. When the user asks "why did it
//! close the tab I was reading?", the answer needs that tab's own numbers at the
//! moment of the decision: how long it had been idle, the closed-form return
//! chance, what Jev said and whether it vetoed, which rule picked it (older
//! copy, over the tab budget, or long idle), and whether the close was live or
//! dry-run. This table keeps exactly that, one row per tab or app.
//!
//! # Outcomes close the loop
//!
//! A decision is only judged right or wrong afterwards. An unload is wrong if
//! the user comes back within the horizon (`returned`); a close is wrong if the
//! user opens the same page again (`reopened`). Both are written back to the
//! decision row by [`History::mark_decision_outcome`], so the log is also the
//! labelled data that [`History::reopen_stats`] reads to stop repeating a close
//! the user keeps undoing.
//!
//! # Syncing
//!
//! Every insert and every outcome bumps `rev`, a counter over the whole table.
//! `ramwarden-cloud-db decisions sync` reads `rev > last_synced` and upserts
//! those rows into MongoDB, so an outcome that arrives a day later updates the
//! document it belongs to. The full URL is kept here, on this machine, because
//! reopen detection needs it; the sync sends only the host and title (see
//! `docs/specs/decision-log.md`).
//!
//! # The vector
//!
//! [`TabVector`] is a fixed-length numeric description of a tab decision. It is
//! stored with the row so that "decisions like this one" is a nearest-neighbour
//! query (Atlas Vector Search once synced), and so the definition lives in one
//! place: the sync tool copies it, it never recomputes it.

use rusqlite::{Connection, OptionalExtension, params};


use crate::history::{History, Result};

/// How long a closed tab stays a candidate for "the user reopened it".
pub const REOPEN_WINDOW_SECS: i64 = 7 * 86_400;

/// A decision to record, before it has an id.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NewDecision {
    /// `tab` or `app`.
    pub kind: String,
    /// `unload`, `close`, `quit`, `keep`.
    pub action: String,
    /// What actually happened: `confirmed`, `queued`, `refused`, `dry_run`,
    /// `kept`. Never "attempted".
    pub result: String,
    /// The host for a tab, the app name for an app.
    pub subject: String,
    pub title: String,
    /// Full URL for a tab. Kept locally for reopen matching, never synced.
    pub url: String,
    pub browser: String,
    pub tab_id: Option<i64>,
    /// `pressure`, `sweep`, `manual`, or the memory figure that triggered a quit.
    pub trigger: String,
    /// One sentence a person can read: which rule, and the numbers behind it.
    pub reason: String,
    /// Every number the decision was made from, as JSON.
    pub inputs: serde_json::Value,
    /// What Jev was asked and answered, when it was consulted.
    pub jev: Option<serde_json::Value>,
    /// [`TabVector`] for tabs; empty for apps.
    pub vector: Vec<f32>,
    /// Bytes measurably returned, when known. 0 otherwise.
    pub bytes: i64,
}

/// One recorded decision.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct Decision {
    pub id: i64,
    /// Unix seconds.
    pub at: i64,
    pub rev: i64,
    pub kind: String,
    pub action: String,
    pub result: String,
    pub subject: String,
    pub title: String,
    #[serde(skip)]
    pub url: String,
    pub browser: String,
    pub tab_id: Option<i64>,
    pub trigger: String,
    pub reason: String,
    pub inputs: serde_json::Value,
    pub jev: Option<serde_json::Value>,
    pub vector: Vec<f32>,
    pub bytes: i64,
    /// `returned` (an unload the user came back to) or `reopened` (a close the
    /// user undid). `None` while unknown.
    pub outcome: Option<String>,
    pub outcome_at: Option<i64>,
}

/// Filter for [`History::recent_decisions`].
#[derive(Clone, Debug, Default)]
pub struct DecisionQuery {
    pub kind: Option<String>,
    pub action: Option<String>,
    /// Substring of the subject (host or app name) or title.
    pub text: Option<String>,
    pub limit: u32,
}

/// The numbers of a tab decision as a fixed-length vector, every component in
/// about `[0, 1]` so that no single feature dominates a cosine distance.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TabVector {
    pub class: &'static str,
    pub idle_minutes: i64,
    pub p_rules: f64,
    pub p_model: Option<f64>,
    pub p_return: f64,
    pub est_mb: u32,
    pub frecency: f64,
    pub related: bool,
    pub duplicate: bool,
    pub unsaved: Option<f64>,
    /// Local hour of the decision, 0-23. Reopen behaviour is daily.
    pub hour: u32,
}

impl TabVector {
    /// Length of [`TabVector::encode`]'s output. The Atlas index is built for
    /// this many dimensions; changing it means rebuilding the index.
    pub const DIMS: usize = 16;

    pub fn encode(&self) -> Vec<f32> {
        let class = |c: &str| if self.class == c { 1.0 } else { 0.0 };
        // A week of idle time spans the whole range; log scale, because the
        // difference between 10 and 70 minutes matters more than 3 vs 4 days.
        let idle = ((1.0 + self.idle_minutes.max(0) as f64).ln() / (1.0 + 10_080.0f64).ln()).min(1.0);
        let angle = self.hour as f64 / 24.0 * std::f64::consts::TAU;
        let v = [
            class("feed"),
            class("app"),
            class("reference"),
            class("page"),
            idle,
            self.p_rules,
            self.p_model.unwrap_or(self.p_rules),
            self.p_return,
            (self.est_mb as f64 / 1000.0).min(1.0),
            self.frecency.max(0.0) / (1.0 + self.frecency.max(0.0)),
            self.related as u8 as f64,
            self.duplicate as u8 as f64,
            self.unsaved.unwrap_or(0.0),
            self.p_model.is_some() as u8 as f64,
            // Hour on a circle, shifted into [0, 1], so 23:00 is next to 00:00.
            0.5 + 0.5 * angle.sin(),
            0.5 + 0.5 * angle.cos(),
        ];
        debug_assert_eq!(v.len(), Self::DIMS);
        v.iter().map(|x| x.clamp(0.0, 1.0) as f32).collect()
    }
}

/// A URL without its fragment: `#section` changes do not make a different page.
pub fn canonical_url(url: &str) -> &str {
    url.split('#').next().unwrap_or(url)
}

pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS decisions (
             id         INTEGER PRIMARY KEY AUTOINCREMENT,
             at         INTEGER NOT NULL,
             rev        INTEGER NOT NULL,
             kind       TEXT    NOT NULL,
             action     TEXT    NOT NULL,
             result     TEXT    NOT NULL,
             subject    TEXT    NOT NULL DEFAULT '',
             title      TEXT    NOT NULL DEFAULT '',
             url        TEXT    NOT NULL DEFAULT '',
             browser    TEXT    NOT NULL DEFAULT '',
             tab_id     INTEGER,
             trigger    TEXT    NOT NULL DEFAULT '',
             reason     TEXT    NOT NULL DEFAULT '',
             inputs     TEXT    NOT NULL DEFAULT '{}',
             jev        TEXT,
             vector     TEXT    NOT NULL DEFAULT '[]',
             bytes      INTEGER NOT NULL DEFAULT 0,
             outcome    TEXT,
             outcome_at INTEGER
         );
         CREATE INDEX IF NOT EXISTS decisions_rev ON decisions(rev);
         CREATE INDEX IF NOT EXISTS decisions_at ON decisions(at DESC);
         CREATE INDEX IF NOT EXISTS decisions_pending
             ON decisions(browser, action, outcome);",
    )
}

const COLUMNS: &str = "id, at, rev, kind, action, result, subject, title, url, browser, tab_id,
                       trigger, reason, inputs, jev, vector, bytes, outcome, outcome_at";

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Decision> {
    let json = |i: usize| -> rusqlite::Result<serde_json::Value> {
        let s: String = r.get(i)?;
        Ok(serde_json::from_str(&s).unwrap_or(serde_json::Value::Null))
    };
    let jev: Option<String> = r.get(14)?;
    let vector: String = r.get(15)?;
    Ok(Decision {
        id: r.get(0)?,
        at: r.get(1)?,
        rev: r.get(2)?,
        kind: r.get(3)?,
        action: r.get(4)?,
        result: r.get(5)?,
        subject: r.get(6)?,
        title: r.get(7)?,
        url: r.get(8)?,
        browser: r.get(9)?,
        tab_id: r.get(10)?,
        trigger: r.get(11)?,
        reason: r.get(12)?,
        inputs: json(13)?,
        jev: jev.and_then(|s| serde_json::from_str(&s).ok()),
        vector: serde_json::from_str(&vector).unwrap_or_default(),
        bytes: r.get(16)?,
        outcome: r.get(17)?,
        outcome_at: r.get(18)?,
    })
}

const NEXT_REV: &str = "(SELECT COALESCE(MAX(rev), 0) + 1 FROM decisions)";

impl History {
    /// Record one decision. Returns its id.
    pub fn record_decision(&self, d: &NewDecision, now: i64) -> Result<i64> {
        let conn = self.conn();
        conn.execute(
            &format!(
                "INSERT INTO decisions
                   (at, rev, kind, action, result, subject, title, url, browser, tab_id,
                    trigger, reason, inputs, jev, vector, bytes)
                 VALUES (?1, {NEXT_REV}, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)"
            ),
            params![
                now,
                d.kind,
                d.action,
                d.result,
                d.subject,
                d.title,
                d.url,
                d.browser,
                d.tab_id,
                d.trigger,
                d.reason,
                d.inputs.to_string(),
                d.jev.as_ref().map(|j| j.to_string()),
                serde_json::to_string(&d.vector).unwrap_or_else(|_| "[]".into()),
                d.bytes,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Newest first, filtered.
    pub fn recent_decisions(&self, q: &DecisionQuery) -> Result<Vec<Decision>> {
        let like = q.text.as_ref().map(|t| format!("%{}%", t.replace(['%', '_'], "")));
        let mut stmt = self.conn().prepare(&format!(
            "SELECT {COLUMNS} FROM decisions
              WHERE (?1 IS NULL OR kind = ?1)
                AND (?2 IS NULL OR action = ?2)
                AND (?3 IS NULL OR subject LIKE ?3 OR title LIKE ?3)
              ORDER BY id DESC LIMIT ?4"
        ))?;
        let rows = stmt.query_map(params![q.kind, q.action, like, q.limit.clamp(1, 1000)], row)?;
        rows.collect()
    }

    pub fn decision(&self, id: i64) -> Result<Option<Decision>> {
        self.conn()
            .query_row(&format!("SELECT {COLUMNS} FROM decisions WHERE id = ?1"), [id], row)
            .optional()
    }

    /// Rows changed after `rev`, oldest change first: what a sync has not sent yet.
    pub fn decisions_since(&self, rev: i64, limit: u32) -> Result<Vec<Decision>> {
        let mut stmt = self.conn().prepare(&format!(
            "SELECT {COLUMNS} FROM decisions WHERE rev > ?1 ORDER BY rev LIMIT ?2"
        ))?;
        let rows = stmt.query_map(params![rev, limit], row)?;
        rows.collect()
    }

    /// Write what became of a decision. Only the first outcome counts: a tab
    /// reopened twice is one wrong close, not two.
    pub fn mark_decision_outcome(&self, id: i64, outcome: &str, now: i64) -> Result<usize> {
        self.conn().execute(
            &format!(
                "UPDATE decisions SET outcome = ?2, outcome_at = ?3, rev = {NEXT_REV}
                  WHERE id = ?1 AND outcome IS NULL"
            ),
            params![id, outcome, now],
        )
    }

    /// The newest confirmed unload of this tab that has no outcome yet, marked
    /// `returned`. Called alongside [`History::mark_returned`].
    pub fn mark_unload_returned(&self, browser: &str, tab_id: i64, now: i64) -> Result<usize> {
        let id: Option<i64> = self
            .conn()
            .query_row(
                "SELECT id FROM decisions
                  WHERE browser = ?1 AND tab_id = ?2 AND action = 'unload'
                    AND result = 'confirmed' AND outcome IS NULL
                  ORDER BY id DESC LIMIT 1",
                params![browser, tab_id],
                |r| r.get(0),
            )
            .optional()?;
        match id {
            Some(id) => self.mark_decision_outcome(id, "returned", now),
            None => Ok(0),
        }
    }

    /// The user opened a page RamWarden closed: mark that close `reopened`.
    ///
    /// The extension reports reopens (`POST /browser/feedback`, see
    /// `docs/specs/tab-feedback.md`); this only attributes the report to a
    /// decision. The exact page wins; failing that, the newest close on the same
    /// site, because the extension matches reopens by site. Only live closes
    /// inside [`REOPEN_WINDOW_SECS`] without an outcome qualify. Returns the
    /// decision id, if one matched.
    pub fn mark_close_reopened(&self, url: &str, subject: &str, now: i64) -> Result<Option<i64>> {
        let page = canonical_url(url);
        let mut stmt = self.conn().prepare(
            "SELECT id, url, subject FROM decisions
              WHERE kind = 'tab' AND action = 'close' AND result IN ('confirmed', 'queued')
                AND outcome IS NULL AND at >= ?1
              ORDER BY id DESC LIMIT 200",
        )?;
        let pending: Vec<(i64, String, String)> = stmt
            .query_map([now - REOPEN_WINDOW_SECS], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<_>>()?;
        let hit = pending
            .iter()
            .find(|(_, u, _)| canonical_url(u) == page)
            .or_else(|| pending.iter().find(|(_, _, s)| !subject.is_empty() && s == subject))
            .map(|(id, _, _)| *id);
        if let Some(id) = hit {
            self.mark_decision_outcome(id, "reopened", now)?;
        }
        Ok(hit)
    }

    /// Live closes on `subject` since `since`, and how many of them the user reopened.
    pub fn reopen_stats(&self, subject: &str, since: i64) -> Result<(i64, i64)> {
        self.conn().query_row(
            "SELECT COUNT(*), COALESCE(SUM(outcome = 'reopened'), 0) FROM decisions
              WHERE kind = 'tab' AND action = 'close' AND result IN ('confirmed', 'queued')
                AND subject = ?1 AND at >= ?2",
            params![subject, since],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
    }

    pub fn count_decisions(&self) -> Result<i64> {
        self.conn().query_row("SELECT COUNT(*) FROM decisions", [], |r| r.get(0))
    }

    pub fn prune_decisions(&self, days: i64, now: i64) -> Result<usize> {
        self.conn().execute("DELETE FROM decisions WHERE at < ?1", [now - days * 86_400])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(url: &str, at_title: &str) -> NewDecision {
        NewDecision {
            kind: "tab".into(),
            action: "close".into(),
            result: "confirmed".into(),
            subject: "docs.example.com".into(),
            title: at_title.into(),
            url: url.into(),
            browser: "chrome".into(),
            tab_id: Some(7),
            trigger: "sweep".into(),
            reason: "idle 30 h, return chance 0.12 < 0.30".into(),
            inputs: serde_json::json!({"idle_minutes": 1800}),
            jev: Some(serde_json::json!({"return_24h": 0.1})),
            vector: TabVector { class: "page", idle_minutes: 1800, ..Default::default() }.encode(),
            bytes: 0,
        }
    }

    #[test]
    fn a_decision_round_trips_with_its_inputs_and_jev_answer() {
        let h = History::in_memory().unwrap();
        let id = h.record_decision(&close("https://docs.example.com/a#x", "Plan"), 100).unwrap();
        let d = h.decision(id).unwrap().unwrap();
        assert_eq!(d.action, "close");
        assert_eq!(d.inputs["idle_minutes"], 1800);
        assert_eq!(d.jev.unwrap()["return_24h"], 0.1);
        assert_eq!(d.vector.len(), TabVector::DIMS);
        assert_eq!(d.outcome, None);
    }

    #[test]
    fn every_change_moves_rev_forward_so_a_sync_sees_late_outcomes() {
        let h = History::in_memory().unwrap();
        let a = h.record_decision(&close("https://x.test/a", "A"), 100).unwrap();
        let _b = h.record_decision(&close("https://x.test/b", "B"), 101).unwrap();
        let synced = h.decisions_since(0, 100).unwrap();
        assert_eq!(synced.len(), 2);
        let last = synced.last().unwrap().rev;
        assert!(h.decisions_since(last, 100).unwrap().is_empty());

        h.mark_decision_outcome(a, "reopened", 200).unwrap();
        let changed = h.decisions_since(last, 100).unwrap();
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].id, a);
        assert_eq!(changed[0].outcome.as_deref(), Some("reopened"));
    }

    #[test]
    fn only_the_first_outcome_counts() {
        let h = History::in_memory().unwrap();
        let a = h.record_decision(&close("https://x.test/a", "A"), 100).unwrap();
        assert_eq!(h.mark_decision_outcome(a, "reopened", 200).unwrap(), 1);
        assert_eq!(h.mark_decision_outcome(a, "reopened", 300).unwrap(), 0);
        assert_eq!(h.decision(a).unwrap().unwrap().outcome_at, Some(200));
    }

    #[test]
    fn a_reopen_matches_the_exact_page_first_then_the_site() {
        let h = History::in_memory().unwrap();
        let now = 10 * 86_400;
        let a = h.record_decision(&close("https://docs.example.com/a", "A"), now - 120).unwrap();
        let b = h.record_decision(&close("https://docs.example.com/b", "B"), now - 60).unwrap();
        assert_eq!(h.mark_close_reopened("https://docs.example.com/a#top", "docs.example.com", now).unwrap(), Some(a));
        // No exact page left for /c: the newest open close on the site takes it.
        assert_eq!(h.mark_close_reopened("https://docs.example.com/c", "docs.example.com", now).unwrap(), Some(b));
        assert_eq!(h.mark_close_reopened("https://docs.example.com/c", "docs.example.com", now).unwrap(), None);
    }

    #[test]
    fn dry_runs_and_old_closes_are_never_marked_reopened() {
        let h = History::in_memory().unwrap();
        let now = 10 * 86_400;
        let mut dry = close("https://x.test/b", "B");
        dry.result = "dry_run".into();
        h.record_decision(&dry, now - 60).unwrap();
        h.record_decision(&close("https://x.test/old", "Old"), now - REOPEN_WINDOW_SECS - 1).unwrap();
        assert_eq!(h.mark_close_reopened("https://x.test/b", "", now).unwrap(), None);
        assert_eq!(h.mark_close_reopened("https://x.test/old", "", now).unwrap(), None);
    }

    #[test]
    fn reopen_stats_count_live_closes_and_their_reopens() {
        let h = History::in_memory().unwrap();
        let a = h.record_decision(&close("https://x.test/a", "A"), 100).unwrap();
        h.record_decision(&close("https://x.test/b", "B"), 100).unwrap();
        let mut dry = close("https://x.test/c", "C");
        dry.result = "dry_run".into();
        h.record_decision(&dry, 100).unwrap();
        h.mark_decision_outcome(a, "reopened", 150).unwrap();
        assert_eq!(h.reopen_stats("docs.example.com", 0).unwrap(), (2, 1));
        assert_eq!(h.reopen_stats("docs.example.com", 101).unwrap(), (0, 0));
    }

    #[test]
    fn an_unload_return_marks_the_newest_confirmed_unload() {
        let h = History::in_memory().unwrap();
        let mut u = close("https://x.test/a", "A");
        u.action = "unload".into();
        let first = h.record_decision(&u, 100).unwrap();
        let second = h.record_decision(&u, 200).unwrap();
        assert_eq!(h.mark_unload_returned("chrome", 7, 300).unwrap(), 1);
        assert_eq!(h.decision(second).unwrap().unwrap().outcome.as_deref(), Some("returned"));
        assert_eq!(h.decision(first).unwrap().unwrap().outcome, None);
    }

    #[test]
    fn the_text_filter_matches_host_or_title() {
        let h = History::in_memory().unwrap();
        h.record_decision(&close("https://x.test/a", "Quarterly plan"), 100).unwrap();
        let q = |t: &str| DecisionQuery { text: Some(t.into()), limit: 10, ..Default::default() };
        assert_eq!(h.recent_decisions(&q("quarterly")).unwrap().len(), 1);
        assert_eq!(h.recent_decisions(&q("docs.example")).unwrap().len(), 1);
        assert!(h.recent_decisions(&q("nothing")).unwrap().is_empty());
    }

    #[test]
    fn the_vector_has_fixed_length_and_bounded_components() {
        let v = TabVector {
            class: "feed",
            idle_minutes: 1_000_000,
            p_rules: 0.4,
            p_model: Some(0.2),
            p_return: 0.3,
            est_mb: 5000,
            frecency: 30.0,
            related: true,
            duplicate: false,
            unsaved: Some(0.9),
            hour: 23,
        }
        .encode();
        assert_eq!(v.len(), TabVector::DIMS);
        assert!(v.iter().all(|x| (0.0..=1.0).contains(x)));
        assert_eq!(v[0], 1.0);
        assert_eq!(v[4], 1.0, "a week or more of idle saturates");
    }
}
