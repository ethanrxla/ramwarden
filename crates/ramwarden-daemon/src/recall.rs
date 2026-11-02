//! Reopen recall: "tabs like this one, you opened again after RamWarden closed
//! them", read from a file the sync tool writes.
//!
//! # Why a file and not a query
//!
//! The question needs the whole decision history across machines and a
//! similarity search over tab titles, which is what MongoDB Atlas Vector Search
//! is for. The daemon does not link a MongoDB driver (its footprint is the
//! product; see CLAUDE.md), and a close must never wait on a network. So
//! `ramwarden-cloud-db decisions sync` does the retrieval: it embeds the titles
//! of the tabs open now, finds the nearest past close decisions in Atlas, and
//! writes how many of those were reopened to `recall.json` next to the history
//! database. The sweep reads that file, which costs one small read.
//!
//! # What it may do
//!
//! Only hold a close back, never cause one. A tab whose neighbours were mostly
//! reopened is kept open this sweep and the keep is logged as its own decision,
//! so the log shows both the rule that wanted to close it and the memory that
//! stopped it. A missing, unreadable or stale file holds nothing back: the
//! sweep is then exactly the rule-based one ("degrade, don't fail").

use std::collections::HashMap;
use std::path::Path;

use ramwarden_core::history::{History, unix_now};
use serde::Deserialize;

use crate::browser::{Row, Target};

/// Older than this, the open tabs it describes have moved on.
pub const MAX_AGE_SECS: u64 = 30 * 60;
/// At least this many similar closes must have been reopened...
pub const MIN_REOPENED: u32 = 2;
/// ...and at least this share of them.
pub const MIN_SHARE: f64 = 0.5;

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Hint {
    pub browser: String,
    pub tab_id: i64,
    pub url: String,
    /// Past close decisions judged similar (by title embedding, then by the
    /// decision vector when there is no title model).
    pub similar: u32,
    pub reopened: u32,
    /// Decision ids of the reopened neighbours, for `GET /decisions/{id}`.
    #[serde(default)]
    pub examples: Vec<String>,
}

impl Hint {
    pub fn holds_back(&self) -> bool {
        self.reopened >= MIN_REOPENED && self.reopened as f64 >= MIN_SHARE * self.similar.max(1) as f64
    }

    pub fn reason(&self) -> String {
        format!(
            "kept open: {} of {} similar tabs RamWarden closed before were reopened (Atlas vector recall)",
            self.reopened, self.similar
        )
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct Recall {
    pub generated_at: u64,
    #[serde(default)]
    pub tabs: Vec<Hint>,
}

impl Recall {
    pub fn path(db_path: &Path) -> std::path::PathBuf {
        db_path.with_file_name("recall.json")
    }

    /// The current recall, or an empty one when there is none worth trusting.
    pub fn load(db_path: &Path) -> Recall {
        let Ok(text) = std::fs::read_to_string(Self::path(db_path)) else { return Recall::default() };
        match serde_json::from_str::<Recall>(&text) {
            Ok(r) if unix_now().saturating_sub(r.generated_at) <= MAX_AGE_SECS => r,
            Ok(_) => Recall::default(),
            Err(e) => {
                tracing::debug!("recall.json unreadable, ignored: {e}");
                Recall::default()
            }
        }
    }

    /// Split `to_close` into what may still close and what recall holds back.
    pub fn hold_back(&self, to_close: Vec<Target>) -> (Vec<Target>, Vec<(Target, Hint)>) {
        if self.tabs.is_empty() {
            return (to_close, Vec::new());
        }
        let by_tab: HashMap<(&str, i64, &str), &Hint> =
            self.tabs.iter().map(|h| ((h.browser.as_str(), h.tab_id, h.url.as_str()), h)).collect();
        let mut close = Vec::with_capacity(to_close.len());
        let mut held = Vec::new();
        for t in to_close {
            let hint = by_tab.get(&(t.browser.as_str(), t.id, t.url.as_str())).filter(|h| h.holds_back()).map(|h| (*h).clone());
            match hint {
                Some(h) => held.push((t, h)),
                None => close.push(t),
            }
        }
        (close, held)
    }
}

/// Log each held-back close as a `keep` decision with the neighbours that caused it.
pub fn record_kept(h: &History, rows: &HashMap<Target, Row>, held: &[(Target, Hint)], trigger: &str) {
    let now = unix_now() as i64;
    for (t, hint) in held {
        let Some(r) = rows.get(t) else { continue };
        let mut d = r.decision("keep", "kept", trigger, hint.reason(), now);
        d.inputs["recall"] = serde_json::json!({
            "similar": hint.similar, "reopened": hint.reopened, "examples": hint.examples,
        });
        tracing::info!("tabs: {} {}", hint.reason(), t.url);
        let _ = h.record_decision(&d, now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(id: i64) -> Target {
        Target { browser: "chrome".into(), id, url: format!("https://x.test/{id}") }
    }

    fn hint(id: i64, similar: u32, reopened: u32) -> Hint {
        Hint { browser: "chrome".into(), tab_id: id, url: format!("https://x.test/{id}"), similar, reopened, examples: vec![] }
    }

    #[test]
    fn only_a_mostly_reopened_neighbourhood_holds_a_close_back() {
        assert!(hint(1, 4, 2).holds_back());
        assert!(!hint(1, 5, 2).holds_back(), "2 of 5 is under half");
        assert!(!hint(1, 1, 1).holds_back(), "one reopen is not a pattern");
    }

    #[test]
    fn hold_back_matches_browser_tab_and_url() {
        let r = Recall { generated_at: unix_now(), tabs: vec![hint(1, 3, 3), hint(2, 3, 0)] };
        let mut moved = target(1);
        moved.url = "https://x.test/other".into();
        let (close, held) = r.hold_back(vec![target(1), target(2), moved.clone()]);
        assert_eq!(held.len(), 1);
        assert_eq!(held[0].0, target(1));
        assert_eq!(close, vec![target(2), moved]);
    }

    #[test]
    fn a_missing_or_stale_file_holds_nothing_back() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("history.db");
        assert!(Recall::load(&db).tabs.is_empty());
        let stale = serde_json::json!({"generated_at": unix_now() - MAX_AGE_SECS - 5,
            "tabs": [{"browser":"chrome","tab_id":1,"url":"https://x.test/1","similar":3,"reopened":3}]});
        std::fs::write(Recall::path(&db), stale.to_string()).unwrap();
        assert!(Recall::load(&db).tabs.is_empty());
        let fresh = serde_json::json!({"generated_at": unix_now(),
            "tabs": [{"browser":"chrome","tab_id":1,"url":"https://x.test/1","similar":3,"reopened":3}]});
        std::fs::write(Recall::path(&db), fresh.to_string()).unwrap();
        assert_eq!(Recall::load(&db).tabs.len(), 1);
    }
}
