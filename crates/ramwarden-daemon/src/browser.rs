//! Browser-scoped, model-independent memory policy.
use crate::{tabpolicy, tabrank, tabs::Tab};
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
    /// Release order: higher goes first. `est_mb / (p_return + ε)`, see `tabrank`.
    pub priority: i64,
    /// Estimated chance the user returns to this tab (0-1).
    pub p_return: f64,
    /// Estimated RAM the tab holds while loaded. A prior by site type, not a measurement.
    pub est_mb: u32,
    /// Recency-weighted visit count from the extension's activation log.
    pub frecency: f64,
    /// On the same site as a tab the user is using right now.
    pub related: bool,
    /// Site class (`feed`, `app`, `reference`, `page`), for the feature description.
    pub class: &'static str,
    /// An older copy of a page that is open more recently elsewhere.
    pub duplicate: bool,
    /// The closed-form return chance, before any model adjustment.
    pub p_rules: f64,
    /// The model's own answer for this tab's features, when it has one.
    pub p_model: Option<f64>,
}

pub fn rank(browser: &str, tabs: &[Tab], threshold: i64) -> Vec<Row> {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as f64)
        .unwrap_or(0.0);
    rank_at(browser, tabs, threshold, now_ms)
}

/// Tabs this recently used count as "what the user is doing right now".
const ACTIVE_WINDOW_MINUTES: i64 = 15;

/// [`rank`] at an explicit time, so the frecency math is testable.
pub fn rank_at(browser: &str, tabs: &[Tab], threshold: i64, now_ms: f64) -> Vec<Row> {
    let threshold = threshold.max(5);
    // Sites of the tabs in use right now: a tab on one of them is probably part
    // of the same task. One pass to build, O(1) per tab to look up.
    let active_sites: std::collections::HashSet<String> = tabs
        .iter()
        .filter(|t| t.active == Some(true) || t.inactive_minutes <= ACTIVE_WINDOW_MINUTES)
        .filter_map(|t| tabrank::site_key(&t.url))
        .collect();
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
            } else if tabpolicy::is_protected_for(&t.url, t.inactive_minutes) {
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
            let class = tabrank::classify(&t.url);
            let mb = tabrank::est_mb(class);
            let frec = tabrank::frecency(&t.activations, now_ms);
            let related = tabrank::site_key(&t.url).is_some_and(|k| active_sites.contains(&k));
            let mult = tabrank::half_life_multiplier(frec, related);
            let p_rules = tabrank::p_return(t.inactive_minutes, class, duplicate, mult);
            let features = crate::tabintel::Features {
                class: tabrank::class_name(class),
                idle_minutes: t.inactive_minutes,
                frecency: frec,
                related,
                duplicate,
            };
            // Advisory model tier: a cached answer adjusts the estimate or vetoes the
            // unload. Absent (disabled, not asked yet, breaker open) means no change.
            let adj = if eligible { crate::tabintel::adjust(p_rules, &features) } else { crate::tabintel::Adjusted::none(p_rules) };
            let (status, reason, eligible) = if adj.veto {
                ("protected", "model says this tab may hold unsaved work", false)
            } else {
                (status, reason, eligible)
            };
            let p = adj.p;
            let reason = if eligible {
                format!("{reason} (return chance {:.0}%, ~{mb} MB)", p * 100.0)
            } else {
                reason.to_string()
            };
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
                reason,
                priority: if eligible { tabrank::score(mb, p).round() as i64 } else { 0 },
                p_return: p,
                est_mb: mb,
                frecency: frec,
                related,
                class: features.class,
                duplicate,
                p_rules,
                p_model: adj.p_model,
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
        // A document in use stays protected; its protection lifts only once it
        // has been left alone (tabpolicy::PROTECTION_RELEASE_MINUTES).
        let mut t = tab(1, 100);
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
/// Tabs the sweep may close outright. Only rows that already passed every unload
/// guard (`candidate`) or were unloaded earlier qualify, so a protected, recent,
/// loading or model-vetoed tab is never closed. Two reasons qualify one:
///
/// * idle at least `close_after_hours` and unlikely to be wanted back
///   (`p_return`, Jev-adjusted where Jev has answered, below `close_below_return`);
/// * an older tab on the same site (see [`site_of`]) as a more recently used one,
///   idle at least `close_duplicates_after_hours`. The newer tab already covers
///   the site, and three Google searches or two copies of a dashboard are most of
///   what makes a tab strip unreadable.
/// * outside the tab budget: not among the `keep_tabs` most recently used tabs in
///   its browser, and untouched for `budget_idle_minutes`. The budget also takes
///   tabs that are only "recent" or a duplicate's keeper, since the user's own
///   measure of "in use" is a handful of tabs, not an hour. Protected rows (active,
///   pinned, audible, private, loading, vetoed by the model, or a protection not
///   yet lifted by idle) still never qualify.
///
/// Most-redundant first: copies, then the longest-idle over-budget tabs, then
/// the stale ones by return chance.
pub fn close_candidates(rows: &[Row], cfg: &ramwarden_core::config::Tabs) -> Vec<Target> {
    let min_idle = (cfg.close_after_hours.max(1.0) * 60.0) as i64;
    let dup_idle = (cfg.close_duplicates_after_hours * 60.0) as i64;
    // Freshest tab per (browser, site): every other tab on that site is a copy.
    let mut freshest: std::collections::HashMap<(&str, String), (i64, i64)> = std::collections::HashMap::new();
    for r in rows {
        if let Some(site) = site_of(&r.target.url) {
            let e = freshest.entry((r.target.browser.as_str(), site)).or_insert((r.inactive_minutes, r.target.id));
            *e = (*e).min((r.inactive_minutes, r.target.id));
        }
    }
    let older_copy = |r: &Row| {
        dup_idle > 0
            && r.inactive_minutes >= dup_idle
            && site_of(&r.target.url).is_some_and(|site| {
                freshest.get(&(r.target.browser.as_str(), site)) != Some(&(r.inactive_minutes, r.target.id))
            })
    };
    let in_budget = budget(rows, cfg.keep_tabs);
    let budget_idle = cfg.budget_idle_minutes.max(5.0) as i64;
    let over_budget = |r: &Row| {
        cfg.keep_tabs > 0
            && r.inactive_minutes >= budget_idle
            && !in_budget.contains(&r.target)
            && matches!(r.status, "recent" | "retained")
    };
    // (row, rank): 0 = copy, 1 = over budget, 2 = stale.
    let mut picked: Vec<(&Row, u8)> = rows
        .iter()
        .filter(|r| r.closeable)
        .filter_map(|r| {
            let usual = matches!(r.status, "candidate" | "unloaded");
            if usual && older_copy(r) {
                Some((r, 0))
            } else if (usual && cfg.keep_tabs > 0 && r.inactive_minutes >= budget_idle && !in_budget.contains(&r.target))
                || over_budget(r)
            {
                Some((r, 1))
            } else if usual && r.inactive_minutes >= min_idle && r.p_return < cfg.close_below_return {
                Some((r, 2))
            } else {
                None
            }
        })
        .collect();
    picked.sort_by(|(a, a_rank), (b, b_rank)| {
        a_rank.cmp(b_rank)
            .then(b.inactive_minutes.cmp(&a.inactive_minutes))
            .then(a.p_return.total_cmp(&b.p_return))
    });
    picked.into_iter().take(cfg.max_close_per_sweep).map(|(r, _)| r.target.clone()).collect()
}

/// The `keep` most recently used tabs of each browser: what the user is working
/// in right now. Ties go to the lower tab id, so the choice is stable.
fn budget(rows: &[Row], keep: usize) -> std::collections::HashSet<Target> {
    let mut by_browser: std::collections::HashMap<&str, Vec<&Row>> = std::collections::HashMap::new();
    for r in rows {
        by_browser.entry(r.target.browser.as_str()).or_default().push(r);
    }
    let mut kept = std::collections::HashSet::new();
    for (_, mut tabs) in by_browser {
        tabs.sort_by_key(|r| (r.inactive_minutes, r.target.id));
        kept.extend(tabs.into_iter().take(keep).map(|r| r.target.clone()));
    }
    kept
}

/// The site a tab is "on" for duplicate closing: its host, plus a Google-style
/// account slot (`/u/1/`), so two signed-in accounts are not one site.
fn site_of(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    let host = parsed.host_str()?.to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host).to_string();
    let mut segments = parsed.path_segments().into_iter().flatten();
    let account = segments.by_ref().skip_while(|s| *s != "u").nth(1)
        .filter(|n| n.chars().all(|c| c.is_ascii_digit()));
    Some(match account { Some(n) => format!("{host}/u/{n}"), None => host })
}

pub fn can_close(tab: &Tab) -> bool {
    !tabpolicy::is_protected_for(&tab.url, tab.inactive_minutes)
        && !tab.incognito
        && tab.active != Some(true)
        && tab.pinned != Some(true)
        && tab.audible != Some(true)
        && tab.status.as_deref() != Some("loading")
}

#[cfg(test)]
mod manual_close_tests {
    use super::*;

    fn idle(id: i64, url: &str, minutes: i64, discarded: bool) -> Tab {
        serde_json::from_value(serde_json::json!({"id":id,"url":url,"inactiveMinutes":minutes,
            "active":false,"pinned":false,"audible":false,"discarded":discarded,"autoDiscardable":true,
            "status":"complete","discardSupported":true})).unwrap()
    }

    #[test]
    fn the_sweep_closes_long_idle_tabs_and_keeps_recent_or_pinned_ones() {
        let cfg = ramwarden_core::config::Tabs::default();
        let mut pinned = idle(4, "https://example.org/pinned", 10_000, false);
        pinned.pinned = Some(true);
        let tabs = vec![
            idle(1, "https://example.org/old", 10_000, false),
            idle(2, "https://example.org/unloaded-long-ago", 10_000, true),
            idle(3, "https://example.org/yesterday-morning", 120, false),
            pinned,
            idle(5, "https://accounts.google.com/x", 10_000, false), // a login page left for a week
            idle(6, "https://accounts.google.com/y", 30, false),     // one in use stays
        ];
        let rows = rank("b", &tabs, 30);
        let mut ids: Vec<i64> = close_candidates(&rows, &cfg).iter().map(|t| t.id).collect();
        ids.sort();
        assert_eq!(ids, vec![1, 2, 5]);
    }

    #[test]
    fn older_tabs_on_the_same_site_close_but_the_freshest_and_other_accounts_stay() {
        let cfg = ramwarden_core::config::Tabs::default();
        let tabs = vec![
            idle(1, "https://www.google.com/search?q=a", 300, false),
            idle(2, "https://www.google.com/search?q=b", 600, true),
            idle(3, "https://google.com/search?q=c", 90, false),
            idle(4, "https://mail.google.com/mail/u/0/#inbox", 300, false),
            idle(5, "https://mail.google.com/mail/u/1/#inbox", 300, false),
            idle(6, "https://example.org/one", 300, false),
        ];
        let rows = rank("b", &tabs, 30);
        let mut ids: Vec<i64> = close_candidates(&rows, &cfg).iter().map(|t| t.id).collect();
        ids.sort();
        assert_eq!(ids, vec![1, 2], "tab 3 is the freshest Google tab; the two Gmail accounts are different sites");
        assert_eq!(site_of("https://mail.google.com/mail/u/1/#inbox").as_deref(), Some("mail.google.com/u/1"));
    }

    #[test]
    fn the_budget_keeps_the_most_recent_tabs_and_closes_the_rest_once_left_alone() {
        let cfg = ramwarden_core::config::Tabs { keep_tabs: 2, ..Default::default() };
        let mut pinned = idle(7, "https://example.org/pinned", 900, false);
        pinned.pinned = Some(true);
        let tabs = vec![
            idle(1, "https://a.example/one", 2, false),
            idle(2, "https://b.example/two", 20, false),
            idle(3, "https://c.example/three", 50, false),     // outside the budget, but not left alone long enough
            idle(4, "https://mail.google.com/mail/u/0/", 300, false),
            idle(5, "https://console.tailscale.com/admin", 400, true), // protection lifted after 4 h
            idle(6, "https://console.tailscale.com/x", 100, false),    // still protected: used 100 min ago... but a copy of 5
            pinned,
            idle(8, "http://10.0.0.1/", 900, false),            // the router: never
            idle(9, "https://vscode.dev/", 900, false),         // local-state editor: never
        ];
        let rows = rank("b", &tabs, 30);
        let mut ids: Vec<i64> = close_candidates(&rows, &cfg).iter().map(|t| t.id).collect();
        ids.sort();
        assert_eq!(ids, vec![4, 5]);
    }

    #[test]
    fn a_zero_budget_turns_the_budget_off() {
        let cfg = ramwarden_core::config::Tabs { keep_tabs: 0, close_duplicates_after_hours: 0.0, ..Default::default() };
        let tabs: Vec<Tab> = (1..=9).map(|i| idle(i, &format!("https://site{i}.example/"), 200, false)).collect();
        assert!(close_candidates(&rank("b", &tabs, 30), &cfg).is_empty());
    }

    #[test]
    fn duplicate_closing_can_be_turned_off() {
        let cfg = ramwarden_core::config::Tabs { close_duplicates_after_hours: 0.0, ..Default::default() };
        let tabs = vec![idle(1, "https://example.org/a", 300, false), idle(2, "https://example.org/b", 90, false)];
        assert!(close_candidates(&rank("b", &tabs, 30), &cfg).is_empty());
    }

    #[test]
    fn the_sweep_respects_its_per_sweep_cap() {
        let cfg = ramwarden_core::config::Tabs { max_close_per_sweep: 2, ..Default::default() };
        let tabs: Vec<Tab> = (1..=6).map(|i| idle(i, &format!("https://example.org/{i}"), 10_000, false)).collect();
        assert_eq!(close_candidates(&rank("b", &tabs, 30), &cfg).len(), 2);
    }

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

    fn tab_at(id: i64, url: &str, age: i64, activations: &[f64]) -> Tab {
        serde_json::from_value(serde_json::json!({"id":id,"url":url,"inactiveMinutes":age,
            "active":false,"pinned":false,"audible":false,"discarded":false,"autoDiscardable":true,
            "status":"complete","discardSupported":true,"activations":activations}))
        .unwrap()
    }

    const DAY_MS: f64 = 86_400_000.0;

    #[test]
    fn a_tab_you_visit_often_outranks_an_identical_one_you_do_not() {
        let now = 20.0 * DAY_MS;
        let daily: Vec<f64> = (0..6).map(|d| now - d as f64 * DAY_MS - 3_600_000.0).collect();
        // Same site type, same idleness; only the visit history differs.
        let rows = rank_at(
            "b",
            &[tab_at(1, "https://a.example.org/x", 600, &[]), tab_at(2, "https://b.example.org/y", 600, &daily)],
            45,
            now,
        );
        let release_order: Vec<i64> = rows.iter().filter(|r| r.eligible).map(|r| r.target.id).collect();
        assert_eq!(release_order, vec![1, 2], "the rarely-visited tab is released first");
        let freq = rows.iter().find(|r| r.target.id == 2).unwrap();
        assert!(freq.frecency > 3.0 && freq.p_return > rows.iter().find(|r| r.target.id == 1).unwrap().p_return);
    }

    #[test]
    fn a_tab_on_the_site_you_are_using_is_kept_longer() {
        let now = 20.0 * DAY_MS;
        let mut active = tab_at(9, "https://www.example.org/now", 0, &[]);
        active.active = Some(true);
        let rows = rank_at(
            "b",
            &[active, tab_at(1, "https://docs.example.org/guide", 600, &[]), tab_at(2, "https://other.net/z", 600, &[])],
            45,
            now,
        );
        let related = rows.iter().find(|r| r.target.id == 1).unwrap();
        let stranger = rows.iter().find(|r| r.target.id == 2).unwrap();
        assert!(related.related && !stranger.related);
        assert!(related.p_return > stranger.p_return);
        assert!(related.priority < stranger.priority, "the unrelated tab goes first");
    }
}
