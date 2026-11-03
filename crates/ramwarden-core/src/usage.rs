//! Numeric, opt-in product diagnostics. No subjects, titles, URLs or paths.
use crate::history::{History, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Metric {
    TabCloseRequests,
    TabCloseConfirmations,
    TabCloseRefusals,
    ManualTabCloseEvents,
    TabReopenEvents,
    TabKeepEvents,
    TabUnloadsConfirmed,
    TabUnloadsRefused,
    CloseDecisionsConfirmed,
    CloseDecisionsRefused,
    CloseDecisionsReopened,
    AppsPaused,
    AppsResumed,
    AppQuitConfirmations,
    AppQuitRefusals,
    AppQuitDryRuns,
    ReclaimConfirmations,
    ReclaimRefusals,
}
impl Metric {
    pub fn key(self) -> String {
        serde_json::to_value(self)
            .expect("metric")
            .as_str()
            .expect("string")
            .to_owned()
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageDay {
    pub day: u64,
    pub since: u64,
    #[serde(default)]
    pub counters: BTreeMap<Metric, u64>,
    pub bytes_recovered: u64,
}
impl UsageDay {
    pub fn valid(&self, now: u64, consent_since: u64) -> bool {
        self.day.is_multiple_of(86400)
            && self.day <= now / 86400 * 86400
            && self.day >= now.saturating_sub(30 * 86400) / 86400 * 86400
            && self.since == consent_since
            && self.since <= now + 300
            && self.day.saturating_add(86400) > self.since
            && self.counters.values().all(|n| *n <= 1_000_000_000)
            && self.bytes_recovered <= 1 << 50
    }
    fn add(&mut self, metric: Metric, n: u64) {
        *self.counters.entry(metric).or_default() += n;
    }
}
impl History {
    /// Aggregate only rows since consent. Confirmation notifications are
    /// deliberately named events, not unique tabs: older workers can emit
    /// both an extension confirmation and the daemon's acknowledgement.
    pub fn usage_day(&self, day: u64, since: u64) -> Result<UsageDay> {
        self.tab_safety_schema()?;
        let mut result = UsageDay {
            day,
            since,
            ..Default::default()
        };
        let from = day.max(since) as i64;
        let until = day.saturating_add(86400) as i64;
        let mut statement=self.conn().prepare("SELECT action,succeeded,COUNT(*),COALESCE(SUM(CASE WHEN succeeded=1 AND bytes_freed>0 THEN bytes_freed ELSE 0 END),0) FROM actions WHERE at>=replace(datetime(?1,'unixepoch'),' ','T') AND at<replace(datetime(?2,'unixepoch'),' ','T') GROUP BY action,succeeded")?;
        let rows = statement.query_map([from, until], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, bool>(1)?,
                r.get::<_, u64>(2)?,
                r.get::<_, u64>(3)?,
            ))
        })?;
        for row in rows {
            let (action, ok, count, bytes) = row?;
            result.bytes_recovered = result.bytes_recovered.saturating_add(bytes);
            let metric = match (action.as_str(), ok) {
                ("suspend", true) => Some(Metric::AppsPaused),
                ("resume", true) => Some(Metric::AppsResumed),
                ("quit_app" | "quit_app_live" | "kill", true) => Some(Metric::AppQuitConfirmations),
                ("quit_app" | "quit_app_live" | "kill", false) => Some(Metric::AppQuitRefusals),
                ("quit_app_dry_run", _) => Some(Metric::AppQuitDryRuns),
                ("reclaim" | "pageout", true) => Some(Metric::ReclaimConfirmations),
                ("reclaim" | "pageout", false) => Some(Metric::ReclaimRefusals),
                _ => None,
            };
            if let Some(metric) = metric {
                result.add(metric, count);
            }
        }
        let mut statement = self.conn().prepare(
            "SELECT outcome,COUNT(*) FROM tab_close_events WHERE at>=?1 AND at<?2 GROUP BY outcome",
        )?;
        for row in statement.query_map([from, until], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, u64>(1)?))
        })? {
            let (kind, count) = row?;
            let metric = match kind.as_str() {
                "requested" => Some(Metric::TabCloseRequests),
                "confirmed" => Some(Metric::TabCloseConfirmations),
                "refused" => Some(Metric::TabCloseRefusals),
                "manual_close" => Some(Metric::ManualTabCloseEvents),
                "reopened" => Some(Metric::TabReopenEvents),
                "keep" => Some(Metric::TabKeepEvents),
                _ => None,
            };
            if let Some(metric) = metric {
                result.add(metric, count);
            }
        }
        let mut statement=self.conn().prepare("SELECT action,result,outcome,COUNT(*) FROM decisions WHERE kind='tab' AND at>=?1 AND at<?2 GROUP BY action,result,outcome")?;
        for row in statement.query_map([from, until], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, u64>(3)?,
            ))
        })? {
            let (action, status, outcome, count) = row?;
            let metric = match (action.as_str(), status.as_str()) {
                ("unload", "confirmed") => Some(Metric::TabUnloadsConfirmed),
                ("unload", "refused") => Some(Metric::TabUnloadsRefused),
                ("close", "confirmed") => Some(Metric::CloseDecisionsConfirmed),
                ("close", "refused") => Some(Metric::CloseDecisionsRefused),
                _ => None,
            };
            if let Some(metric) = metric {
                result.add(metric, count);
            }
            if action == "close" && status == "confirmed" && outcome.as_deref() == Some("reopened")
            {
                result.add(Metric::CloseDecisionsReopened, count);
            }
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn summaries_exclude_preconsent_rows_and_all_private_text() {
        let h = History::in_memory().unwrap();
        let day = 1_793_000_000 / 86400 * 86400;
        let since = day + 100;
        for at in [since - 1, since + 1] {
            h.conn().execute("INSERT INTO actions(at,rung,trigger,action,target,bytes_freed,succeeded,notes) VALUES (replace(datetime(?1,'unixepoch'),' ','T'),3,'manual','suspend','SecretApp',2048,1,'/Users/private-document')",[at as i64]).unwrap();
        }
        let d = crate::decisions::NewDecision {
            kind: "tab".into(),
            action: "close".into(),
            result: "confirmed".into(),
            subject: "private.example".into(),
            title: "sensitive-title".into(),
            url: "https://private.example/secret".into(),
            ..Default::default()
        };
        let id = h.record_decision(&d, (since + 2) as i64).unwrap();
        h.mark_decision_outcome(id, "reopened", (since + 3) as i64)
            .unwrap();
        h.tab_feedback(
            "https://private.example/secret",
            "manual_close",
            (since + 3) as i64,
        )
        .unwrap();
        let summary = h.usage_day(day, since).unwrap();
        assert_eq!(summary.counters[&Metric::AppsPaused], 1);
        assert_eq!(summary.counters[&Metric::CloseDecisionsReopened], 1);
        assert_eq!(summary.counters[&Metric::ManualTabCloseEvents], 1);
        assert_eq!(summary.bytes_recovered, 2048);
        let text = serde_json::to_string(&summary).unwrap();
        for secret in ["SecretApp", "private.example", "sensitive-title", "/Users/"] {
            assert!(!text.contains(secret));
        }
        assert!(summary.valid(since + 10, since));
    }
    #[test]
    fn payloads_reject_text_fields_and_invalid_consent_windows() {
        assert!(serde_json::from_value::<UsageDay>(serde_json::json!({"day":0,"since":0,"counters":{},"bytes_recovered":0,"title":"private"})).is_err());
        let mut day = UsageDay {
            day: 86400,
            since: 86500,
            ..Default::default()
        };
        assert!(day.valid(87000, 86500));
        assert!(!day.valid(87000, 86600));
        day.day = 86401;
        assert!(!day.valid(87000, 86500));
    }
}
