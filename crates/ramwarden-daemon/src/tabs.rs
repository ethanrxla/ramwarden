//! Browser tab bookkeeping, and routing close commands to the right browser.
//!
//! # Two transports, because browsers differ
//!
//! Chrome and Brave hold a WebSocket open, so the daemon can ask for tabs and
//! get an answer in the same turn. Firefox's extension is an event page that the
//! browser suspends, so it cannot hold a socket; it polls `POST /api/tabs` every
//! 30 seconds instead, and close commands are queued for it to collect on its
//! next poll.
//!
//! All of the awkward timing in here comes from that difference, and every piece
//! of it exists because of something that actually went wrong in v1:
//!
//! * **Ownership tracking.** Tab ids are only unique within a browser, so a
//!   close command has to go to the browser that reported the tab. Broadcasting
//!   would close whatever happened to share the id.
//! * **Startup grace.** Firefox's event page can take 10+ seconds to wake after
//!   a daemon restart. Analysing before it checks in means analysing without its
//!   tabs and concluding there is nothing to close.
//! * **Re-registration, not accumulation.** An extension reload arrives as the
//!   same `browser_id` with a fresh tab list. Appending would double-count every
//!   tab; replacing is correct.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// One browser tab, exactly as the extension reports it.
///
/// # Numbers from JavaScript are not integers
///
/// The shipped extension sends `lastActiveMs` straight from a JS timestamp, and
/// observed values look like `1790631842449.037`. Typing that field as an integer
/// made serde reject it — and because the tabs arrive as one array, a single bad
/// field discarded the **whole report**, so the daemon saw zero tabs and
/// concluded there was nothing to close.
///
/// Every numeric field here therefore tolerates a float. The extension is
/// installed and out of reach; the daemon is what has to be accommodating.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Tab {
    #[serde(deserialize_with = "lenient_i64")]
    pub id: i64,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub title: String,
    /// How long since the user last looked at it. Computed by the extension,
    /// which is the only thing that can see it.
    #[serde(default, rename = "inactiveMinutes", deserialize_with = "lenient_i64_opt")]
    pub inactive_minutes: i64,
    /// A JavaScript millisecond timestamp, which is a float.
    #[serde(default, rename = "lastActiveMs")]
    pub last_active_ms: f64,
    #[serde(default)]
    pub incognito: bool,
    #[serde(default)]
    pub active: Option<bool>,
    #[serde(default)]
    pub pinned: Option<bool>,
    #[serde(default)]
    pub audible: Option<bool>,
    #[serde(default)]
    pub discarded: Option<bool>,
    #[serde(default, rename = "autoDiscardable")]
    pub auto_discardable: Option<bool>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default, rename = "discardSupported")]
    pub discard_supported: Option<bool>,
}

/// Accept an integer or a float where an integer is wanted, rounding.
fn lenient_i64<'de, D>(d: D) -> std::result::Result<i64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    match serde_json::Value::deserialize(d)? {
        serde_json::Value::Number(n) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f.round() as i64))
            .ok_or_else(|| D::Error::custom("not a number")),
        other => Err(D::Error::custom(format!("expected a number, got {other}"))),
    }
}

fn lenient_i64_opt<'de, D>(d: D) -> std::result::Result<i64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(lenient_i64(d).unwrap_or(0))
}

/// A command for a browser to carry out.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Command {
    pub action: &'static str,
    #[serde(rename = "tabIds")]
    pub tab_ids: Vec<i64>,
    #[serde(rename = "requestId", skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tabs: Vec<crate::browser::Target>,
    #[serde(rename = "minInactiveMinutes", skip_serializing_if = "Option::is_none")]
    pub min_inactive_minutes: Option<i64>,
}

impl Command {
    pub fn close(tab_ids: Vec<i64>) -> Self {
        Command {
            action: "close",
            tab_ids, request_id: None, tabs: Vec::new(), min_inactive_minutes: None,
        }
    }
    pub fn discard(request_id: String, tabs: Vec<crate::browser::Target>, threshold: i64) -> Self {
        Self { action: "discard", tab_ids: tabs.iter().map(|t|t.id).collect(),
            request_id: Some(request_id), tabs, min_inactive_minutes: Some(threshold.max(5)) }
    }
}

/// Which transport a browser uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transport {
    /// Chrome/Brave: a live WebSocket.
    Socket,
    /// Firefox: HTTP polling.
    Poll,
}

/// Every connected browser, what it last reported, and what it still owes.
#[derive(Default)]
pub struct Registry {
    /// conn_id -> last tab report from a socket browser.
    socket_tabs: HashMap<String, Vec<Tab>>,
    /// browser_id -> last tab report from a polling browser.
    poll_tabs: HashMap<String, Vec<Tab>>,
    /// browser_id -> commands waiting for its next poll.
    poll_queue: HashMap<String, Vec<Command>>,
    /// tab id -> the browser that reported it.
    owners: HashMap<i64, String>,
    reported_at: HashMap<String, std::time::Instant>,
}

impl Registry {
    pub fn new() -> Self {
        Registry::default()
    }

    // ── Registration ────────────────────────────────────────────────────────

    /// Record a socket browser's connection. Its tabs arrive separately.
    pub fn connect_socket(&mut self, conn_id: &str) {
        self.socket_tabs.insert(conn_id.to_string(), Vec::new());
    }

    /// Drop a socket browser and everything it owned.
    ///
    /// Leaving its tabs behind would route later close commands into a closed
    /// socket and silently lose them.
    pub fn disconnect_socket(&mut self, conn_id: &str) {
        self.socket_tabs.remove(conn_id);
        self.owners.retain(|_, owner| owner != conn_id);
    }

    /// Store a tab report, replacing whatever that browser reported before.
    ///
    /// Replacing rather than merging is what makes an extension reload safe: it
    /// arrives as the same id with a complete fresh list.
    pub fn report(&mut self, browser: &str, transport: Transport, tabs: Vec<Tab>) {
        self.reported_at.insert(browser.to_string(), std::time::Instant::now());
        // Drop ownership of tabs this browser no longer has, so a closed tab
        // does not keep a stale owner forever.
        self.owners.retain(|_, owner| owner != browser);
        for t in &tabs {
            self.owners.insert(t.id, browser.to_string());
        }
        match transport {
            Transport::Socket => self.socket_tabs.insert(browser.to_string(), tabs),
            Transport::Poll => self.poll_tabs.insert(browser.to_string(), tabs),
        };
    }

    /// A tab the browser confirmed is gone.
    pub fn forget_tabs(&mut self, ids: &[i64]) {
        for id in ids {
            self.owners.remove(id);
        }
        for tabs in self.socket_tabs.values_mut().chain(self.poll_tabs.values_mut()) {
            tabs.retain(|t| !ids.contains(&t.id));
        }
    }

    // ── Queries ─────────────────────────────────────────────────────────────

    /// Every tab from every browser.
    pub fn all_tabs(&self) -> Vec<Tab> {
        self.socket_tabs
            .values()
            .chain(self.poll_tabs.values())
            .flatten()
            .cloned()
            .collect()
    }

    pub fn socket_ids(&self) -> Vec<String> {
        let mut v: Vec<String> = self.socket_tabs.keys().cloned().collect();
        v.sort();
        v
    }

    pub fn poll_ids(&self) -> Vec<String> {
        let mut v: Vec<String> = self.poll_tabs.keys().cloned().collect();
        v.sort();
        v
    }

    pub fn socket_count(&self) -> usize {
        self.socket_tabs.len()
    }

    pub fn poll_count(&self) -> usize {
        self.poll_tabs.len()
    }

    pub fn browsers_connected(&self) -> usize {
        self.socket_count() + self.poll_count()
    }

    pub fn has_poll_browser(&self) -> bool {
        !self.poll_tabs.is_empty()
    }

    pub fn tabs_for(&self, browser: &str) -> &[Tab] {
        self.socket_tabs
            .get(browser)
            .or_else(|| self.poll_tabs.get(browser))
            .map_or(&[], Vec::as_slice)
    }

    pub fn owner_of(&self, tab_id: i64) -> Option<&str> {
        let mut owners = self.socket_tabs.iter().chain(self.poll_tabs.iter())
            .filter(|(_, tabs)| tabs.iter().any(|t| t.id == tab_id)).map(|(b, _)| b.as_str());
        let first = owners.next()?;
        if owners.next().is_some() { None } else { Some(first) }
    }

    /// Group tab ids by the browser that owns them.
    ///
    /// Tab ids are unique only within a browser, so a close command must be
    /// addressed. Ids with no known owner are returned separately rather than
    /// guessed at — their browser disconnected, and the tab is already gone or
    /// unreachable.
    pub fn route(&self, tab_ids: &[i64]) -> (HashMap<String, Vec<i64>>, Vec<i64>) {
        let mut by_owner: HashMap<String, Vec<i64>> = HashMap::new();
        let mut orphans = Vec::new();
        for &id in tab_ids {
            match self.owner_of(id) {
                Some(owner) => by_owner.entry(owner.to_string()).or_default().push(id),
                None => orphans.push(id),
            }
        }
        for ids in by_owner.values_mut() {
            ids.sort_unstable();
        }
        orphans.sort_unstable();
        (by_owner, orphans)
    }

    pub fn reported_since(&self, browser: &str, since: std::time::Instant) -> bool {
        self.reported_at.get(browser).is_some_and(|at| *at >= since)
    }

    pub fn fresh(&self, browser: &str) -> bool {
        self.reported_at.get(browser).is_some_and(|at| at.elapsed() < std::time::Duration::from_secs(90))
    }

    pub fn forget_tabs_for(&mut self, browser: &str, ids: &[i64]) {
        if let Some(tabs) = self.socket_tabs.get_mut(browser).or_else(|| self.poll_tabs.get_mut(browser)) {
            tabs.retain(|t| !ids.contains(&t.id));
        }
    }

    pub fn mark_discarded(&mut self, browser: &str, ids: &[i64]) {
        if let Some(tabs) = self.socket_tabs.get_mut(browser).or_else(|| self.poll_tabs.get_mut(browser)) {
            for tab in tabs { if ids.contains(&tab.id) { tab.discarded = Some(true); } }
        }
    }

    pub fn is_socket(&self, browser: &str) -> bool {
        self.socket_tabs.contains_key(browser)
    }

    // ── Poll queue ──────────────────────────────────────────────────────────

    /// Queue a command for a polling browser to collect.
    pub fn enqueue(&mut self, browser: &str, cmd: Command) {
        self.poll_queue.entry(browser.to_string()).or_default().push(cmd);
    }

    /// Hand over and clear everything queued for a browser.
    pub fn take_queued(&mut self, browser: &str) -> Vec<Command> {
        self.poll_queue.remove(browser).unwrap_or_default()
    }

    pub fn queued_count(&self, browser: &str) -> usize {
        self.poll_queue.get(browser).map_or(0, Vec::len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(id: i64, url: &str, inactive: i64) -> Tab {
        Tab {
            id,
            url: url.to_string(),
            title: format!("tab {id}"),
            inactive_minutes: inactive,
            ..Default::default()
        }
    }

    /// Verbatim shape from v1's `test_tab_pipeline.py` fixtures.
    fn firefox_tabs() -> Vec<Tab> {
        vec![
            tab(1, "https://github.com/ekomsSavior/REDflare-v2", 6087),
            tab(2, "https://labs.infoguard.ch/posts/ghost-sender/", 3346),
        ]
    }

    #[test]
    fn a_poll_browsers_report_is_stored_and_visible() {
        let mut r = Registry::new();
        r.report("firefox-abc", Transport::Poll, firefox_tabs());

        assert_eq!(r.poll_count(), 1);
        assert_eq!(r.socket_count(), 0);
        assert_eq!(r.tabs_for("firefox-abc").len(), 2);
        assert_eq!(r.all_tabs().len(), 2);
        assert!(r.has_poll_browser());
    }

    /// v1's `_request_all_tabs` had to merge both transports; a tab list missing
    /// Firefox's half means concluding there is nothing to close.
    #[test]
    fn tabs_from_both_transports_are_merged() {
        let mut r = Registry::new();
        r.connect_socket("brave01");
        r.report("brave01", Transport::Socket, vec![tab(10, "https://brave/", 100)]);
        r.report("firefox-abc", Transport::Poll, firefox_tabs());

        let all = r.all_tabs();
        assert_eq!(all.len(), 3);
        assert_eq!(r.browsers_connected(), 2);
        let ids: Vec<i64> = {
            let mut v: Vec<i64> = all.iter().map(|t| t.id).collect();
            v.sort();
            v
        };
        assert_eq!(ids, vec![1, 2, 10]);
    }

    /// v1's duplicate-browser-id test: an extension reload must not double-count.
    #[test]
    fn re_reporting_replaces_rather_than_accumulates() {
        let mut r = Registry::new();
        r.report("firefox-abc", Transport::Poll, firefox_tabs());
        r.report("firefox-abc", Transport::Poll, firefox_tabs());

        assert_eq!(r.poll_count(), 1, "one browser, not two");
        assert_eq!(r.all_tabs().len(), 2, "two tabs, not four");
    }

    #[test]
    fn a_shrinking_report_releases_ownership_of_the_tabs_that_went_away() {
        let mut r = Registry::new();
        r.report("ff", Transport::Poll, firefox_tabs());
        assert_eq!(r.owner_of(2), Some("ff"));

        r.report("ff", Transport::Poll, vec![tab(1, "https://x/", 10)]);
        assert_eq!(r.owner_of(1), Some("ff"));
        assert_eq!(r.owner_of(2), None, "tab 2 is gone");
    }

    #[test]
    fn a_report_expires_and_a_new_report_renews_it() {
        let mut reg=Registry::new();reg.report("b",Transport::Poll,vec![]);
        assert!(reg.fresh("b"));
        reg.reported_at.insert("b".into(),std::time::Instant::now()-std::time::Duration::from_secs(91));
        assert!(!reg.fresh("b"));reg.report("b",Transport::Poll,vec![]);assert!(reg.fresh("b"));
    }

    // ── Routing ─────────────────────────────────────────────────────────────

    /// Tab ids are unique only within a browser, so broadcasting a close would
    /// shut whatever happened to share the number.
    #[test]
    fn close_commands_are_addressed_to_the_owning_browser() {
        let mut r = Registry::new();
        r.connect_socket("brave01");
        r.report("brave01", Transport::Socket, vec![tab(1, "https://brave/", 10)]);
        r.report("firefox-abc", Transport::Poll, vec![tab(2, "https://ff/", 10)]);

        let (by_owner, orphans) = r.route(&[1, 2]);
        assert_eq!(by_owner["brave01"], vec![1]);
        assert_eq!(by_owner["firefox-abc"], vec![2]);
        assert!(orphans.is_empty());
    }

    /// Two browsers can legitimately both have a tab with id 1.
    #[test]
    fn ambiguous_legacy_ids_are_refused() {
        let mut r = Registry::new();
        r.report("brave", Transport::Socket, vec![tab(1,"https://a/",100)]);
        r.report("firefox", Transport::Poll, vec![tab(1,"https://b/",100)]);
        let (routes, orphans) = r.route(&[1]);
        assert!(routes.is_empty());
        assert_eq!(orphans, vec![1]);
        assert_eq!(r.all_tabs().len(),2);
    }

    #[test]
    fn a_tab_with_no_known_owner_is_reported_rather_than_guessed_at() {
        let r = Registry::new();
        let (by_owner, orphans) = r.route(&[42, 7]);
        assert!(by_owner.is_empty());
        assert_eq!(orphans, vec![7, 42]);
    }

    #[test]
    fn disconnecting_a_socket_browser_releases_its_tabs() {
        let mut r = Registry::new();
        r.connect_socket("brave01");
        r.report("brave01", Transport::Socket, vec![tab(1, "https://brave/", 10)]);
        assert_eq!(r.owner_of(1), Some("brave01"));

        r.disconnect_socket("brave01");
        assert_eq!(r.socket_count(), 0);
        assert_eq!(r.owner_of(1), None, "a closed socket owns nothing");
        assert!(r.all_tabs().is_empty());
    }

    #[test]
    fn confirmed_closes_are_forgotten() {
        let mut r = Registry::new();
        r.report("ff", Transport::Poll, firefox_tabs());
        r.forget_tabs(&[1]);

        assert_eq!(r.owner_of(1), None);
        assert_eq!(r.all_tabs().len(), 1);
        assert_eq!(r.all_tabs()[0].id, 2);
    }

    // ── Poll queue ──────────────────────────────────────────────────────────

    /// A polling browser cannot be pushed to, so commands wait for it.
    #[test]
    fn commands_for_a_polling_browser_are_queued_until_it_checks_in() {
        let mut r = Registry::new();
        r.report("ff", Transport::Poll, firefox_tabs());
        r.enqueue("ff", Command::close(vec![1, 2]));

        assert_eq!(r.queued_count("ff"), 1);
        let taken = r.take_queued("ff");
        assert_eq!(taken, vec![Command::close(vec![1, 2])]);
        assert_eq!(r.queued_count("ff"), 0, "collecting clears the queue");
    }

    #[test]
    fn several_queued_commands_are_delivered_together_in_order() {
        let mut r = Registry::new();
        r.enqueue("ff", Command::close(vec![1]));
        r.enqueue("ff", Command::close(vec![2]));
        let taken = r.take_queued("ff");
        assert_eq!(taken.len(), 2);
        assert_eq!(taken[0].tab_ids, vec![1]);
        assert_eq!(taken[1].tab_ids, vec![2]);
    }

    #[test]
    fn a_browser_with_nothing_queued_collects_an_empty_list() {
        let mut r = Registry::new();
        assert!(r.take_queued("nobody").is_empty());
    }

    #[test]
    fn transport_is_distinguishable_so_routing_can_pick_push_or_queue() {
        let mut r = Registry::new();
        r.connect_socket("brave01");
        r.report("brave01", Transport::Socket, vec![]);
        r.report("ff", Transport::Poll, vec![]);
        assert!(r.is_socket("brave01"));
        assert!(!r.is_socket("ff"));
    }

    // ── Wire format ─────────────────────────────────────────────────────────

    /// The extension reads `tabIds`, not `tab_ids`.
    #[test]
    fn a_command_serialises_in_the_shape_the_extension_expects() {
        let json = serde_json::to_string(&Command::close(vec![1, 2])).unwrap();
        assert_eq!(json, r#"{"action":"close","tabIds":[1,2]}"#);
    }

    /// The extension sends camelCase and may omit fields entirely.
    #[test]
    fn a_tab_deserialises_from_what_the_extension_sends() {
        let t: Tab = serde_json::from_str(
            r#"{"id":1,"url":"https://x/","title":"X","lastActiveMs":0,
                "inactiveMinutes":6087,"incognito":false}"#,
        )
        .unwrap();
        assert_eq!(t.id, 1);
        assert_eq!(t.inactive_minutes, 6087);
        assert!(!t.incognito);
    }

    /// The exact value the shipped extension was observed to send. Typed as an
    /// integer this rejected the whole report and the daemon saw zero tabs.
    #[test]
    fn a_floating_point_timestamp_does_not_discard_the_report() {
        let t: Tab = serde_json::from_str(
            r#"{"id":1,"url":"https://x/","lastActiveMs":1790631842449.037,"inactiveMinutes":90}"#,
        )
        .expect("a float timestamp must not fail the parse");
        assert_eq!(t.id, 1);
        assert!((t.last_active_ms - 1_790_631_842_449.037).abs() < 1.0);
        assert_eq!(t.inactive_minutes, 90);
    }

    /// One bad field in one tab must not throw away the other twenty.
    #[test]
    fn a_whole_report_survives_a_float_in_any_numeric_field() {
        let tabs: Vec<Tab> = serde_json::from_str(
            r#"[{"id":1,"lastActiveMs":1790631842449.037,"inactiveMinutes":6087.4},
                {"id":2.0,"lastActiveMs":0,"inactiveMinutes":12}]"#,
        )
        .unwrap();
        assert_eq!(tabs.len(), 2);
        assert_eq!(tabs[0].inactive_minutes, 6087, "rounded");
        assert_eq!(tabs[1].id, 2, "a float id is rounded, not rejected");
    }

    #[test]
    fn a_tab_with_only_an_id_still_deserialises() {
        let t: Tab = serde_json::from_str(r#"{"id":9}"#).unwrap();
        assert_eq!(t.id, 9);
        assert_eq!(t.inactive_minutes, 0);
        assert!(t.url.is_empty());
    }

    #[test]
    fn an_incognito_tab_round_trips() {
        let t: Tab = serde_json::from_str(r#"{"id":3,"incognito":true}"#).unwrap();
        assert!(t.incognito);
        let back = serde_json::to_value(&t).unwrap();
        assert_eq!(back["incognito"], true);
        assert_eq!(back["inactiveMinutes"], 0);
    }
}
