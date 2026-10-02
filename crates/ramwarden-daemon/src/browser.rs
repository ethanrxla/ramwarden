//! Browser-scoped, model-independent memory policy.
use crate::{tabpolicy, tabs::Tab};
use serde::{Deserialize, Serialize};

pub const BATCH_LIMIT: usize = 5;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
pub struct Target {
    pub browser: String,
    pub id: i64,
    pub url: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Row {
    #[serde(flatten)]
    pub target: Target,
    pub title: String,
    pub inactive_minutes: i64,
    pub eligible: bool,
    pub closeable: bool,
    pub status: &'static str,
    pub reason: String,
    pub priority: i64,
}

pub fn rank(browser: &str, tabs: &[Tab], threshold: i64) -> Vec<Row> {
    let threshold = threshold.max(5);
    let canonical = |t: &Tab| t.url.split('#').next().unwrap_or("").to_string();
    let mut copies: std::collections::HashMap<String, (usize, (i64, i64))> =
        std::collections::HashMap::new();
    for t in tabs {
        let entry = copies
            .entry(canonical(t))
            .or_insert((0, (t.inactive_minutes, t.id)));
        entry.0 += 1;
        entry.1 = entry.1.min((t.inactive_minutes, t.id));
    }
    let mut rows: Vec<_> = tabs
        .iter()
        .map(|t| {
            let (count, newest) = copies[&canonical(t)];
            let duplicate = count > 1 && newest != (t.inactive_minutes, t.id);
            let keeper = count > 1 && !duplicate;
            let required = if duplicate || tabpolicy::is_stale_domain(&t.url) {
                threshold
            } else {
                threshold.saturating_mul(2)
            };
            let (status, reason) = if t.discarded == Some(true) {
                ("unloaded", "already unloaded")
            } else if t.active == Some(true) {
                ("protected", "active tab")
            } else if t.pinned == Some(true) {
                ("protected", "pinned tab")
            } else if t.audible == Some(true) {
                ("protected", "playing audio")
            } else if t.incognito {
                ("protected", "private tab")
            } else if tabpolicy::is_protected(&t.url) {
                ("protected", "editor, internal page or protected URL")
            } else if t.discard_supported != Some(true)
                || t.active.is_none()
                || t.pinned.is_none()
                || t.audible.is_none()
                || t.discarded.is_none()
                || t.auto_discardable.is_none()
                || t.status.is_none()
            {
                (
                    "update needed",
                    "manual close available; update extension for safe unloading",
                )
            } else if t.auto_discardable != Some(true) {
                ("protected", "browser opted out of discarding")
            } else if t.status.as_deref() != Some("complete") {
                ("protected", "page is loading")
            } else if keeper {
                ("retained", "most recently used duplicate copy")
            } else if t.inactive_minutes < required {
                ("recent", "used recently; retain for reuse")
            } else if duplicate {
                (
                    "candidate",
                    "older duplicate; retain most recently used copy",
                )
            } else {
                ("candidate", "idle background tab; reloads when selected")
            };
            let eligible = status == "candidate";
            Row {
                target: Target {
                    browser: browser.into(),
                    id: t.id,
                    url: t.url.clone(),
                },
                title: t.title.clone(),
                inactive_minutes: t.inactive_minutes.max(0),
                eligible,
                closeable: can_close(t),
                status,
                reason: reason.into(),
                priority: if eligible {
                    t.inactive_minutes.clamp(0, 10080) + if duplicate { 10080 } else { 0 }
                } else {
                    0
                },
            }
        })
        .collect();
    rows.sort_by(|a, b| {
        b.priority
            .cmp(&a.priority)
            .then(a.target.id.cmp(&b.target.id))
    });
    rows
}

pub fn snapshot(reg: &crate::tabs::Registry, threshold: i64) -> Vec<Row> {
    let mut rows = Vec::new();
    for browser in reg.socket_ids().into_iter().chain(reg.poll_ids()) {
        let mut ranked = rank(&browser, reg.tabs_for(&browser), threshold);
        if !reg.fresh(&browser) {
            for row in &mut ranked {
                row.eligible = false;
                row.closeable = false;
                row.status = "stale";
                row.reason = "waiting for a fresh browser report".into();
                row.priority = 0;
            }
        }
        rows.extend(ranked);
    }
    rows.sort_by(|a, b| {
        b.priority
            .cmp(&a.priority)
            .then(a.target.browser.cmp(&b.target.browser))
            .then(a.target.id.cmp(&b.target.id))
    });
    rows
}

/// Recheck the current report; never apply a decision to a navigated/reused ID.
pub fn select(reg: &crate::tabs::Registry, threshold: i64, requested: &[Target]) -> Vec<Target> {
    snapshot(reg, threshold)
        .into_iter()
        .filter(|r| r.eligible && requested.contains(&r.target))
        .take(BATCH_LIMIT)
        .map(|r| r.target)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tab(id: i64, age: i64) -> Tab {
        serde_json::from_value(
            serde_json::json!({"id":id,"url":"https://example.org/article","inactiveMinutes":age,
            "active":false,"pinned":false,"audible":false,"discarded":false,"autoDiscardable":true,
            "status":"complete","discardSupported":true}),
        )
        .unwrap()
    }
    #[test]
    fn ordinary_pages_require_twice_the_threshold_and_are_ranked() {
        let mut recent = tab(1, 89);
        recent.url.push_str("/other");
        let rows = rank("b", &[recent, tab(2, 120)], 45);
        assert!(!rows.iter().find(|r| r.target.id == 1).unwrap().eligible);
        assert!(rows.iter().find(|r| r.target.id == 2).unwrap().eligible);
    }
    #[test]
    fn safety_flags_and_unknown_metadata_block_unloading() {
        for field in ["active", "pinned", "audible", "discarded", "incognito"] {
            let mut v = serde_json::to_value(tab(1, 999)).unwrap();
            v[field] = true.into();
            assert!(
                !rank("b", &[serde_json::from_value(v).unwrap()], 45)[0].eligible,
                "{field}"
            );
        }
        for field in [
            "active",
            "pinned",
            "audible",
            "discarded",
            "autoDiscardable",
            "status",
            "discardSupported",
        ] {
            let mut v = serde_json::to_value(tab(1, 999)).unwrap();
            v.as_object_mut().unwrap().remove(field);
            assert!(
                !rank("b", &[serde_json::from_value(v).unwrap()], 45)[0].eligible,
                "missing {field}"
            );
        }
        let mut t = tab(1, 999);
        t.url = "https://docs.google.com/document/d/a/edit".into();
        assert!(!rank("b", &[t], 45)[0].eligible);
    }
    #[test]
    fn duplicate_lru_retains_newest_copy_and_never_merges_query_strings() {
        let mut a = tab(1, 50);
        a.url.push_str("?q=1#one");
        let mut b = tab(2, 60);
        b.url.push_str("?q=1#two");
        let mut c = tab(3, 60);
        c.url.push_str("?q=2");
        let rows = rank("b", &[a, b, c], 45);
        assert_eq!(
            rows.iter()
                .filter(|r| r.eligible)
                .map(|r| r.target.id)
                .collect::<Vec<_>>(),
            vec![2]
        );
        assert!(rows[0].reason.contains("duplicate"));
    }
    #[test]
    fn a_duplicate_group_always_retains_one_copy_even_when_all_are_old() {
        let rows = rank("b", &[tab(1, 900), tab(2, 1000)], 45);
        assert_eq!(rows.iter().filter(|r| r.eligible).count(), 1);
        assert!(!rows.iter().find(|r| r.target.id == 1).unwrap().eligible);
    }
    #[test]
    fn zero_threshold_still_has_a_cooling_period() {
        assert!(!rank("b", &[tab(1, 0)], 0)[0].eligible);
    }
}

#[cfg(test)]
mod selection_tests {
    use super::*;
    use crate::tabs::{Registry, Transport};
    #[test]
    fn selections_are_bounded_browser_scoped_and_navigation_safe() {
        let mut reg = Registry::new();
        let tabs: Vec<Tab>=(1..=8).map(|id| serde_json::from_value(serde_json::json!({"id":id,
            "url":format!("https://example.org/{id}"),"inactiveMinutes":999,"active":false,"pinned":false,
            "audible":false,"discarded":false,"autoDiscardable":true,"status":"complete","discardSupported":true})).unwrap()).collect();
        reg.report("a", Transport::Socket, tabs.clone());
        reg.report("b", Transport::Socket, tabs);
        let targets: Vec<_> = snapshot(&reg, 45)
            .into_iter()
            .filter(|r| r.target.browser == "a")
            .map(|r| r.target)
            .collect();
        let selected = select(&reg, 45, &targets);
        assert_eq!(selected.len(), BATCH_LIMIT);
        assert!(selected.iter().all(|t| t.browser == "a"));
        let mut changed = targets[0].clone();
        changed.url.push_str("/navigated");
        assert!(select(&reg, 45, &[changed]).is_empty());
        reg.mark_discarded("a", &[1]);
        assert!(select(&reg, 45, &targets[..1]).is_empty());
        assert_eq!(
            snapshot(&reg, 45)
                .iter()
                .filter(|r| r.status == "unloaded")
                .count(),
            1
        );
    }
    #[test]
    fn domain_matching_does_not_trust_query_strings_and_private_networks_are_protected() {
        assert!(!tabpolicy::is_stale_domain(
            "https://example.org/?next=youtube.com"
        ));
        assert!(!tabpolicy::is_stale_domain("https://notyoutube.com/"));
        for url in [
            "http://172.16.1.1/",
            "http://[::1]/",
            "http://[fd00::1]/",
            "file:///tmp/x",
            "https://example.org/oauth/start",
        ] {
            assert!(tabpolicy::is_protected(url), "{url}");
        }
    }
}

/// Manual close does not require automatic eviction eligibility.
pub fn can_close(tab: &Tab) -> bool {
    !tabpolicy::is_protected(&tab.url)
        && !tab.incognito
        && tab.active != Some(true)
        && tab.pinned != Some(true)
        && tab.audible != Some(true)
        && tab.status.as_deref() != Some("loading")
}

#[cfg(test)]
mod manual_close_tests {
    use super::*;
    #[test]
    fn legacy_and_recent_tabs_can_be_selected_for_explicit_closing() {
        let mut tab: Tab = serde_json::from_value(
            serde_json::json!({"id":1,"url":"https://example.org/article","inactiveMinutes":0}),
        )
        .unwrap();
        assert!(can_close(&tab));
        tab.active = Some(true);
        assert!(!can_close(&tab));
        tab.active = None;
        tab.pinned = Some(true);
        assert!(!can_close(&tab));
        tab.pinned = None;
        tab.audible = Some(true);
        assert!(!can_close(&tab));
        tab.audible = None;
        tab.url = "https://docs.google.com/document/d/a/edit".into();
        assert!(!can_close(&tab));
    }
}
