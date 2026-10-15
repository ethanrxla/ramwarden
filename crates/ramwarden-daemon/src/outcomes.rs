//! What happens after a tab is unloaded: did the user come back?
//!
//! Every confirmed unload is recorded with the numbers that justified it
//! (idle time, return chance, size estimate, frecency). Later reports are
//! compared against those records: a tab that was unloaded and is now loaded
//! again, recently used, on the same site, was returned to. Those labelled rows
//! are what calibrates the closed-form model, and what any learned tier is judged
//! against. Only the host is stored, never the URL or title.

use ramwarden_core::history::{History, NewUnload};

use crate::browser::Row;
use crate::tabrank;
use crate::tabs::Tab;

/// A report this soon after an unload may still show the tab as loaded.
const SETTLE_SECONDS: i64 = 20;
/// A tab reloaded by a user was activated within about this long ago.
const RECENT_MINUTES: i64 = 1;

fn class_name(url: &str) -> &'static str {
    match tabrank::classify(url) {
        tabrank::Class::Feed => "feed",
        tabrank::Class::App => "app",
        tabrank::Class::Reference => "reference",
        tabrank::Class::Page => "page",
    }
}

/// Record a confirmed unload. Tabs whose host cannot be read are skipped.
pub fn record(h: &History, rows: &[&Row], now: i64) -> usize {
    rows.iter()
        .filter_map(|r| {
            let host = tabrank::site_key(&r.target.url)?;
            let u = NewUnload {
                browser: r.target.browser.clone(),
                tab_id: r.target.id,
                host,
                class: class_name(&r.target.url).into(),
                idle_minutes: r.inactive_minutes,
                p_return: r.p_return,
                est_mb: r.est_mb as i64,
                frecency: r.frecency,
            };
            h.record_unload(&u, now).ok()
        })
        .count()
}

/// Compare a browser's latest report with its pending unloads and mark the ones
/// the user has come back to. Returns how many.
///
/// Cost: one query for the pending set, then a hash lookup per reported tab.
pub fn reconcile(h: &History, browser: &str, tabs: &[Tab], now: i64) -> usize {
    let Ok(pending) = h.pending_unloads(browser) else { return 0 };
    if pending.is_empty() {
        return 0;
    }
    let by_id: std::collections::HashMap<i64, (&str, i64)> =
        pending.iter().map(|(id, host, at)| (*id, (host.as_str(), *at))).collect();
    let mut returned = 0;
    for t in tabs {
        let Some(&(host, unloaded_at)) = by_id.get(&t.id) else { continue };
        let loaded_again = t.discarded == Some(false);
        let used_since = t.inactive_minutes <= RECENT_MINUTES;
        // Tab ids restart with the browser; the host guards against a new tab
        // that merely reuses the number.
        let same_site = tabrank::site_key(&t.url).as_deref() == Some(host);
        if loaded_again && used_since && same_site && now - unloaded_at >= SETTLE_SECONDS {
            returned += h.mark_returned(browser, t.id, now).unwrap_or(0);
        }
    }
    returned
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::Target;

    fn row(id: i64, url: &str) -> Row {
        Row {
            target: Target { browser: "b".into(), id, url: url.into() },
            title: "t".into(),
            inactive_minutes: 300,
            eligible: true,
            closeable: true,
            status: "candidate",
            reason: String::new(),
            priority: 1,
            p_return: 0.2,
            est_mb: 120,
            frecency: 0.0,
            related: false,
        }
    }

    fn tab(id: i64, url: &str, discarded: bool, idle: i64) -> Tab {
        serde_json::from_value(serde_json::json!({"id":id,"url":url,"inactiveMinutes":idle,"discarded":discarded})).unwrap()
    }

    #[test]
    fn a_returned_tab_is_marked_and_a_still_unloaded_one_is_not() {
        let h = History::in_memory().unwrap();
        let (a, b) = (row(1, "https://news.example.com/a"), row(2, "https://shop.example.org/b"));
        assert_eq!(record(&h, &[&a, &b], 1_000), 2);

        // Both still discarded: nothing changes.
        let still = [tab(1, a.target.url.as_str(), true, 400), tab(2, b.target.url.as_str(), true, 400)];
        assert_eq!(reconcile(&h, "b", &still, 2_000), 0);

        // Tab 1 is loaded again and in use; tab 2 stays unloaded.
        let back = [tab(1, a.target.url.as_str(), false, 0), tab(2, b.target.url.as_str(), true, 400)];
        assert_eq!(reconcile(&h, "b", &back, 2_000), 1);
        assert_eq!(h.pending_unloads("b").unwrap().len(), 1);
    }

    #[test]
    fn it_waits_for_the_report_to_settle_and_ignores_a_reused_tab_id() {
        let h = History::in_memory().unwrap();
        record(&h, &[&row(1, "https://news.example.com/a")], 1_000);
        let loaded = [tab(1, "https://news.example.com/a", false, 0)];
        assert_eq!(reconcile(&h, "b", &loaded, 1_005), 0, "within the settle window");
        // Same id, different site: a new tab that took the number.
        let impostor = [tab(1, "https://elsewhere.net/", false, 0)];
        assert_eq!(reconcile(&h, "b", &impostor, 5_000), 0);
        // Loaded but untouched for an hour: reloaded by something else, not a return.
        let idle = [tab(1, "https://news.example.com/a", false, 60)];
        assert_eq!(reconcile(&h, "b", &idle, 5_000), 0);
        assert_eq!(reconcile(&h, "b", &loaded, 5_000), 1);
    }

    #[test]
    fn tabs_with_an_unreadable_host_are_not_recorded() {
        let h = History::in_memory().unwrap();
        assert_eq!(record(&h, &[&row(1, "not a url")], 1_000), 0);
        assert_eq!(h.count_outcomes().unwrap(), 0);
    }
}
