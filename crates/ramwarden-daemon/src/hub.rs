//! The connected browsers, and the shared state every route reads.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use ramwarden_core::actuator::Outcome;
use ramwarden_core::config::Config;
use ramwarden_core::detector::Detector;
use ramwarden_core::history::History;
use ramwarden_core::ladder::Ladder;
use ramwarden_ai::provider::Provider;
use ramwarden_kernel::Root;
use tokio::sync::{mpsc, oneshot};

use crate::tabs::{Command, Registry, Tab, Transport};

/// A polling browser needs time to wake before an analysis is meaningful.
///
/// Firefox's extension is an event page the browser suspends; after a daemon
/// restart it can take over ten seconds to wake, seed its tab activity map, and
/// post its first report. Analysing before then sees none of its tabs and
/// concludes there is nothing to close — which is what v1's startup grace exists
/// to prevent.
pub const STARTUP_GRACE: Duration = Duration::from_secs(15);

/// The shorter wait once the daemon has been up a while.
pub const CONNECT_GRACE: Duration = Duration::from_millis(3000);

/// How long the daemon counts as freshly started.
pub const STARTUP_WINDOW: Duration = Duration::from_secs(30);

/// How long to wait for a socket browser to answer a tab request.
pub const TAB_REPORT_TIMEOUT: Duration = Duration::from_secs(6);

/// How long to wait for a socket browser to confirm closes.
pub const CLOSE_TIMEOUT: Duration = Duration::from_secs(8);

/// Browsers, their sockets, and the replies the daemon is waiting on.
#[derive(Default)]
pub struct Hub {
    pub reg: Registry,
    senders: HashMap<String, mpsc::UnboundedSender<String>>,
    tab_waiters: HashMap<String, Vec<oneshot::Sender<Vec<Tab>>>>,
    close_waiters: HashMap<String, oneshot::Sender<Vec<i64>>>,
    discard_attempts: HashMap<crate::browser::Target, Instant>,
    discard_waiters: HashMap<String, (String, oneshot::Sender<Vec<i64>>)>,
}

impl Hub {
    pub fn new() -> Self {
        Hub::default()
    }

    pub fn attach(&mut self, conn_id: &str, tx: mpsc::UnboundedSender<String>) {
        self.senders.insert(conn_id.to_string(), tx);
        self.reg.connect_socket(conn_id);
    }

    /// Drop a socket and fail anything waiting on it.
    ///
    /// Leaving a waiter hanging would stall the monitor thread until its timeout
    /// on every tick, for a browser that is never coming back.
    pub fn detach(&mut self, conn_id: &str) {
        self.senders.remove(conn_id);
        self.reg.disconnect_socket(conn_id);
        if let Some(waiters) = self.tab_waiters.remove(conn_id) {
            for w in waiters { let _ = w.send(Vec::new()); }
        }
        if let Some(w) = self.close_waiters.remove(conn_id) {
            let _ = w.send(Vec::new());
        }
    }

    /// Push a message to one socket browser.
    pub fn send(&self, conn_id: &str, msg: &serde_json::Value) -> bool {
        let Some(tx) = self.senders.get(conn_id) else {
            return false;
        };
        tx.send(msg.to_string()).is_ok()
    }

    pub fn expect_tabs(&mut self, conn_id: &str) -> oneshot::Receiver<Vec<Tab>> {
        let (tx, rx) = oneshot::channel();
        self.tab_waiters.entry(conn_id.to_string()).or_default().retain(|w| !w.is_closed());
        self.tab_waiters.entry(conn_id.to_string()).or_default().push(tx);
        rx
    }

    pub fn deliver_tabs(&mut self, conn_id: &str, tabs: Vec<Tab>) {
        self.reg.report(conn_id, Transport::Socket, tabs.clone());
        if let Some(waiters) = self.tab_waiters.remove(conn_id) {
            for w in waiters { let _ = w.send(tabs.clone()); }
        }
    }

    pub fn expect_close(&mut self, conn_id: &str) -> oneshot::Receiver<Vec<i64>> {
        let (tx, rx) = oneshot::channel();
        self.close_waiters.insert(conn_id.to_string(), tx);
        rx
    }

    pub fn deliver_close(&mut self, conn_id: &str, ids: Vec<i64>) {
        self.reg.forget_tabs_for(conn_id, &ids);
        if let Some(w) = self.close_waiters.remove(conn_id) {
            let _ = w.send(ids);
        }
    }

    pub fn deliver_discard(&mut self, browser: &str, request: &str, ids: Vec<i64>) {
        if self.discard_waiters.get(request).is_some_and(|(owner,_)| owner == browser)
            && let Some((_, waiter)) = self.discard_waiters.remove(request) {
            let _ = waiter.send(ids);
        }
    }

    pub fn queue_for_poll(&mut self, browser: &str, cmd: Command) {
        self.reg.enqueue(browser, cmd);
    }
}

/// Everything the routes and the monitor loop share.
#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<Config>,
    pub root: Root,
    pub det: Arc<RwLock<Detector>>,
    pub ladder: Arc<Mutex<Ladder>>,
    pub hub: Arc<Mutex<Hub>>,
    /// A second connection to the history database, for the read endpoints.
    /// SQLite handles concurrent connections; the ladder owns its own.
    pub history: Arc<Mutex<History>>,
    /// The model tier. Shared and immutable — the clients are cheap to clone and
    /// hold no mutable state.
    pub ai: Arc<Provider>,
    pub started: Instant,
}

impl AppState {
    /// How long to wait for a polling browser to check in before giving up.
    pub fn browser_grace(&self) -> Duration {
        if self.started.elapsed() < STARTUP_WINDOW {
            STARTUP_GRACE
        } else {
            CONNECT_GRACE
        }
    }

    /// Ask every browser for its tabs and merge the answers.
    pub async fn request_all_tabs(&self) -> Vec<Tab> {
        // Wait for a polling browser to arrive, if none has yet.
        if self.hub.lock().unwrap().reg.browsers_connected() == 0 {
            let grace = self.browser_grace();
            let deadline = Instant::now() + grace;
            while Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(200)).await;
                if self.hub.lock().unwrap().reg.browsers_connected() > 0 {
                    tracing::info!("poll browser arrived after waiting");
                    break;
                }
            }
        }

        // Ask the socket browsers, all at once.
        let mut waiters = Vec::new();
        {
            let mut hub = self.hub.lock().unwrap();
            for conn_id in hub.reg.socket_ids() {
                let rx = hub.expect_tabs(&conn_id);
                if hub.send(&conn_id, &serde_json::json!({"action": "get_tabs"})) {
                    waiters.push((conn_id, rx));
                }
            }
        }

        let reports = futures::future::join_all(waiters.into_iter().map(|(conn_id,rx)| async move {
            match tokio::time::timeout(TAB_REPORT_TIMEOUT, rx).await {
                Ok(Ok(tabs)) => tabs,
                _ => { tracing::warn!("tab report timed out for [{conn_id}]"); Vec::new() }
            }
        })).await;
        let mut out: Vec<Tab> = reports.into_iter().flatten().collect();

        // Polling browsers have already reported whatever they have.
        let hub = self.hub.lock().unwrap();
        for bid in hub.reg.poll_ids() {
            out.extend(hub.reg.tabs_for(&bid).iter().cloned());
        }
        out
    }

    /// Unload a bounded set, scoped by browser, ID and URL. No close fallback.
    pub async fn discard_tabs(&self, requested: &[crate::browser::Target]) -> DiscardResult {
        let threshold = self.cfg.thresholds.inactivity_minutes as i64;
        let mut result = DiscardResult::default();
        let mut waiters = Vec::new();
        {
            let mut hub = self.hub.lock().unwrap();
            hub.discard_attempts.retain(|_, at| at.elapsed() < Duration::from_secs(90));
            let available: Vec<_> = requested.iter().filter(|t| !hub.discard_attempts.contains_key(t)).cloned().collect();
            let selected = crate::browser::select(&hub.reg, threshold, &available);
            for target in &selected { hub.discard_attempts.insert(target.clone(), Instant::now()); }
            result.refused = requested.iter().filter(|t| !selected.contains(t)).cloned().collect();
            let mut groups: HashMap<String, Vec<crate::browser::Target>> = HashMap::new();
            for target in selected { groups.entry(target.browser.clone()).or_default().push(target); }
            for (browser, targets) in groups {
                let request = uuid::Uuid::new_v4().to_string();
                let cmd = Command::discard(request.clone(), targets.clone(), threshold);
                if hub.reg.is_socket(&browser) {
                    let (tx, rx) = oneshot::channel();
                    hub.discard_waiters.insert(request.clone(), (browser.clone(),tx));
                    if hub.send(&browser, &serde_json::to_value(cmd).unwrap()) {
                        waiters.push((browser, request, targets, rx));
                    } else {
                        hub.discard_waiters.remove(&request);
                        result.refused.extend(targets);
                    }
                } else {
                    hub.queue_for_poll(&browser, cmd);
                    result.queued.extend(targets);
                }
            }
        }
        let replies = futures::future::join_all(waiters.into_iter().map(|(browser, request, targets, rx)| async move {
            let ids = tokio::time::timeout(CLOSE_TIMEOUT,rx).await.ok().and_then(Result::ok).unwrap_or_default();
            (browser,request,targets,ids)
        })).await;
        let mut hub = self.hub.lock().unwrap();
        for (browser,request,targets,ids) in replies {
            hub.discard_waiters.remove(&request);
            let confirmed: Vec<i64> = targets.iter().filter(|t|ids.contains(&t.id)).map(|t|t.id).collect();
            hub.reg.mark_discarded(&browser,&confirmed);
            for target in targets {
                if confirmed.contains(&target.id) { result.confirmed.push(target); }
                else { result.refused.push(target); }
            }
        }
        result
    }

    /// Explicit close, validated against a freshly requested report. Legacy
    /// commands carry bare IDs, so ambiguity is refused instead of guessed.
    pub async fn close_selected_tabs(&self, requested: &[crate::browser::Target]) -> DiscardResult {
        let mut result=DiscardResult::default();
        let selected = {
            let hub=self.hub.lock().unwrap();
            let mut selected=Vec::new();
            for target in requested {
                let valid=hub.reg.fresh(&target.browser)
                    && hub.reg.owner_of(target.id)==Some(target.browser.as_str())
                    && hub.reg.tabs_for(&target.browser).iter().any(|t|t.id==target.id && t.url==target.url && crate::browser::can_close(t));
                if valid { if !selected.contains(target) { selected.push(target.clone()); } }
                else { result.refused.push(target.clone()); }
            }
            selected
        };
        let ids:Vec<_>=selected.iter().map(|t|t.id).collect();
        let closed=self.close_tabs(&ids).await;
        for target in selected {
            if closed.confirmed.contains(&target.id) {result.confirmed.push(target);}
            else if closed.queued.contains(&target.id) {result.queued.push(target);}
            else {result.refused.push(target);}
        }
        result
    }

    /// Close tabs, returning only those a browser confirmed are gone.
    ///
    /// Tab ids are a browser's own numbering and have nothing to do with pids,
    /// so this returns its own type rather than squeezing them into an
    /// [`Outcome`], whose `affected` field means process ids.
    ///
    /// A socket browser answers in the same turn. A polling browser collects the
    /// command later, so its tabs are reported optimistically — v1 did the same,
    /// and the alternative is blocking an autonomous tick for up to 30 seconds.
    /// The note on the outcome says which is which.
    pub async fn close_tabs(&self, ids: &[i64]) -> CloseResult {
        let (by_owner, orphans) = self.hub.lock().unwrap().reg.route(ids);
        let mut out = CloseResult::default();
        if !orphans.is_empty() {
            out.notes
                .push(format!("{} tab(s) had no connected browser", orphans.len()));
        }

        let mut waiters = Vec::new();
        let mut optimistic = Vec::new();

        {
            let mut hub = self.hub.lock().unwrap();
            for (browser, tab_ids) in by_owner {
                if hub.reg.is_socket(&browser) {
                    let rx = hub.expect_close(&browser);
                    let msg = serde_json::json!({"action": "close", "tabIds": tab_ids});
                    if hub.send(&browser, &msg) {
                        waiters.push(rx);
                    }
                } else {
                    hub.queue_for_poll(&browser, Command::close(tab_ids.clone()));
                    optimistic.extend(tab_ids);
                }
            }
        }

        for rx in waiters {
            if let Ok(Ok(confirmed)) = tokio::time::timeout(CLOSE_TIMEOUT, rx).await {
                out.confirmed.extend(confirmed);
            } else {
                out.notes.push("a browser did not confirm its closes".into());
            }
        }

        if !optimistic.is_empty() {
            out.notes.push(format!(
                "{} tab(s) queued for a polling browser — not yet confirmed",
                optimistic.len()
            ));
            out.queued.extend(optimistic);
        }
        out
    }
}

/// What a close request achieved.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CloseResult {
    /// Tabs a browser said are gone.
    pub confirmed: Vec<i64>,
    /// Tabs handed to a polling browser, which will act on them later.
    pub queued: Vec<i64>,
    pub notes: Vec<String>,
}

impl CloseResult {
    /// Everything that will close, confirmed or queued. This is what goes in the
    /// history record, since a queued close does happen — just not yet.
    pub fn total(&self) -> usize {
        self.confirmed.len() + self.queued.len()
    }

    /// As an [`Outcome`] for the ladder, which measures in bytes.
    ///
    /// `bytes_freed` is left at zero deliberately: the browser does not say how
    /// much a tab held, and v1's habit of dividing an estimate across tabs put a
    /// number in the log that was never measured.
    pub fn as_outcome(&self) -> Outcome {
        Outcome {
            affected: Vec::new(),
            bytes_freed: 0,
            notes: {
                let mut n = self.notes.clone();
                n.push(format!(
                    "{} closed, {} queued",
                    self.confirmed.len(),
                    self.queued.len()
                ));
                n
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hub_with_socket() -> (Hub, mpsc::UnboundedReceiver<String>) {
        let mut hub = Hub::new();
        let (tx, rx) = mpsc::unbounded_channel();
        hub.attach("brave01", tx);
        (hub, rx)
    }

    #[test]
    fn concurrent_analysis_requests_both_receive_the_report() {
        let (mut hub,_)=hub_with_socket();
        let a=hub.expect_tabs("brave01");let b=hub.expect_tabs("brave01");
        hub.deliver_tabs("brave01",vec![Tab {id:1,..Default::default()}]);
        assert_eq!(a.blocking_recv().unwrap().len(),1);
        assert_eq!(b.blocking_recv().unwrap().len(),1);
    }

    #[test]
    fn attaching_registers_the_browser_as_a_socket() {
        let (hub, _rx) = hub_with_socket();
        assert_eq!(hub.reg.socket_count(), 1);
        assert!(hub.reg.is_socket("brave01"));
    }

    #[test]
    fn a_pushed_message_reaches_the_socket() {
        let (hub, mut rx) = hub_with_socket();
        assert!(hub.send("brave01", &serde_json::json!({"action": "get_tabs"})));
        let got = rx.try_recv().unwrap();
        assert_eq!(got, r#"{"action":"get_tabs"}"#);
    }

    #[test]
    fn pushing_to_an_unknown_browser_fails_rather_than_panicking() {
        let (hub, _rx) = hub_with_socket();
        assert!(!hub.send("nobody", &serde_json::json!({})));
    }

    /// A waiter left hanging would stall the monitor thread for its full timeout
    /// on every tick, for a browser that is never coming back.
    #[test]
    fn detaching_fails_the_waiters_immediately() {
        let (mut hub, _rx) = hub_with_socket();
        let tab_rx = hub.expect_tabs("brave01");
        let close_rx = hub.expect_close("brave01");

        hub.detach("brave01");

        assert_eq!(tab_rx.blocking_recv().unwrap(), Vec::<Tab>::new());
        assert_eq!(close_rx.blocking_recv().unwrap(), Vec::<i64>::new());
        assert_eq!(hub.reg.socket_count(), 0);
    }

    #[test]
    fn delivering_tabs_both_stores_them_and_wakes_the_waiter() {
        let (mut hub, _rx) = hub_with_socket();
        let rx = hub.expect_tabs("brave01");
        let tabs = vec![Tab {
            id: 7,
            url: "https://x/".into(),
            ..Default::default()
        }];
        hub.deliver_tabs("brave01", tabs.clone());

        assert_eq!(rx.blocking_recv().unwrap(), tabs);
        assert_eq!(hub.reg.tabs_for("brave01").len(), 1);
        assert_eq!(hub.reg.owner_of(7), Some("brave01"));
    }

    #[test]
    fn a_confirmed_close_forgets_the_tab_and_wakes_the_waiter() {
        let (mut hub, _rx) = hub_with_socket();
        hub.deliver_tabs(
            "brave01",
            vec![Tab {
                id: 7,
                ..Default::default()
            }],
        );
        let rx = hub.expect_close("brave01");
        hub.deliver_close("brave01", vec![7]);

        assert_eq!(rx.blocking_recv().unwrap(), vec![7]);
        assert_eq!(hub.reg.owner_of(7), None);
        assert!(hub.reg.tabs_for("brave01").is_empty());
    }

    #[test]
    fn an_unrequested_report_is_stored_without_a_waiter() {
        let (mut hub, _rx) = hub_with_socket();
        hub.deliver_tabs(
            "brave01",
            vec![Tab {
                id: 1,
                ..Default::default()
            }],
        );
        assert_eq!(hub.reg.all_tabs().len(), 1);
    }

    #[test]
    fn a_polling_browsers_commands_are_queued_not_pushed() {
        let mut hub = Hub::new();
        hub.reg.report("ff", Transport::Poll, vec![]);
        hub.queue_for_poll("ff", Command::close(vec![1, 2]));
        assert_eq!(hub.reg.queued_count("ff"), 1);
    }

    /// Firefox needs the long wait only just after a restart.
    #[test]
    fn the_browser_grace_shortens_once_the_daemon_has_settled() {
        let fresh = Instant::now();
        let old = Instant::now() - STARTUP_WINDOW - Duration::from_secs(1);
        assert!(fresh.elapsed() < STARTUP_WINDOW);
        assert!(old.elapsed() >= STARTUP_WINDOW);
        assert!(STARTUP_GRACE > CONNECT_GRACE);
    }
}

#[derive(Default, serde::Serialize)]
pub struct DiscardResult {
    pub confirmed: Vec<crate::browser::Target>,
    pub queued: Vec<crate::browser::Target>,
    pub refused: Vec<crate::browser::Target>,
}
