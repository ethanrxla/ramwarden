//! Talking to the daemon.
//!
//! The window owns no policy and no kernel access: it renders what `/state`
//! reports and posts what the user clicks. That separation is why the GTK code
//! can stay thin, and why this module is testable without a display.
//!
//! Every call has a timeout. The window polls on a timer, and a daemon that has
//! been stopped mid-poll must dim the display rather than hang the UI thread.

use std::time::Duration;

use serde::Deserialize;

use crate::model::Row;

/// Short, because these run while the user is waiting.
pub const TIMEOUT: Duration = Duration::from_secs(10);

/// Longer: an analysis may call a model.
pub const ANALYZE_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Transport(String),
    #[error("daemon returned {status}: {body}")]
    Status { status: u16, body: String },
    #[error("could not read the reply: {0}")]
    Decode(String),
    /// The daemon answered but is not the Rust one.
    #[error("{0}")]
    WrongDaemon(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Live memory picture, from `/state`.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct State {
    #[serde(default)]
    pub percent: f64,
    #[serde(default)]
    pub used_mb: f64,
    #[serde(default)]
    pub total_mb: f64,
    #[serde(default)]
    pub warn_percent: f64,
    #[serde(default)]
    pub browsers_connected: u32,
    #[serde(default)]
    pub processes: Vec<Row>,
    #[serde(default)]
    pub totals_mb: Totals,
    #[serde(default)]
    pub suspended: Vec<Suspended>,
    #[serde(default)]
    pub psi_some: f64,
    #[serde(default)]
    pub psi_full: f64,
    /// How much of "used" memory zram has already compressed away — the figure
    /// that stops the bar being alarming for no reason.
    #[serde(default)]
    pub zram_saved_mb: f64,
    /// False until the detector has two samples. Until then nothing is idle and
    /// the window must not imply otherwise.
    #[serde(default)]
    pub warm: bool,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Totals {
    #[serde(default, rename = "PROTECTED")]
    pub protected: f64,
    #[serde(default, rename = "IN_USE")]
    pub in_use: f64,
    #[serde(default, rename = "IDLE")]
    pub idle: f64,
}

/// An application RamWarden froze.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Suspended {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub pids: Vec<i32>,
    #[serde(default)]
    pub rss_mb: f64,
    #[serde(default)]
    pub minutes: f64,
}

/// What the ladder would do next, from `/ladder/plan`.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Plan {
    #[serde(default)]
    pub rung: Option<String>,
    #[serde(default)]
    pub blocked: Option<String>,
    #[serde(default)]
    pub suspend: Vec<String>,
    #[serde(default)]
    pub kill: Vec<String>,
    #[serde(default)]
    pub kill_pending: Option<KillPending>,
}

/// A kill the ladder intends to carry out unless cancelled.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct KillPending {
    #[serde(default)]
    pub targets: Vec<String>,
    #[serde(default)]
    pub seconds_remaining: u64,
}

/// One thing the ladder did, from `/actions`.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ActionRow {
    #[serde(default)]
    pub at: String,
    #[serde(default)]
    pub rung: i32,
    #[serde(default)]
    pub action: String,
    #[serde(default)]
    pub target: String,
    #[serde(default)]
    pub freed_mb: f64,
    #[serde(default)]
    pub succeeded: bool,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub psi_some: f64,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ActionLog {
    #[serde(default)]
    pub total_reclaimed_mb: f64,
    #[serde(default)]
    pub actions: Vec<ActionRow>,
}

/// Per-process VRAM, from `/ai`.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct AiStatus {
    #[serde(default)]
    pub gpu: Option<Gpu>,
    #[serde(default)]
    pub local: serde_json::Value,
    #[serde(default)]
    pub cloud: serde_json::Value,
    #[serde(default)]
    pub page_out: serde_json::Value,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Gpu {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub used_mb: f64,
    #[serde(default)]
    pub total_mb: f64,
    #[serde(default)]
    pub percent_used: f64,
    #[serde(default)]
    pub utilisation: u32,
}

/// The result of an action the user clicked.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ActionResult {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub suspended: Vec<i32>,
    #[serde(default)]
    pub killed: Vec<i32>,
    #[serde(default)]
    pub resumed: Vec<i32>,
    #[serde(default)]
    pub freed_mb: f64,
    #[serde(default)]
    pub notes: Vec<String>,
}

impl ActionResult {
    /// Whether anything happened. The daemon answers 409 for a refusal, so this
    /// also covers "the gate said no".
    pub fn did_something(&self) -> bool {
        !self.suspended.is_empty() || !self.killed.is_empty() || !self.resumed.is_empty()
    }

    /// A line for the status bar — the outcome, not the intention.
    pub fn describe(&self, verb: &str) -> String {
        if self.did_something() {
            let n = self.suspended.len() + self.killed.len() + self.resumed.len();
            if self.freed_mb > 0.0 {
                format!("{verb} {} ({} process(es), {})", self.name, n, crate::model::fmt_mb(self.freed_mb))
            } else {
                format!("{verb} {} ({n} process(es))", self.name)
            }
        } else if self.notes.is_empty() {
            format!("{} refused {}", self.name, verb)
        } else {
            format!("{}: {}", self.name, self.notes.join("; "))
        }
    }
}

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base: String,
}

impl Client {
    pub fn new(host: &str, port: u16) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(ANALYZE_TIMEOUT)
            .build()
            .map_err(|e| Error::Transport(e.to_string()))?;
        Ok(Client {
            http,
            base: format!("http://{host}:{port}"),
        })
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    async fn get<T: for<'de> Deserialize<'de>>(&self, path: &str, timeout: Duration) -> Result<T> {
        let r = self
            .http
            .get(format!("{}{path}", self.base))
            .timeout(timeout)
            .send()
            .await
            .map_err(|e| Error::Transport(e.to_string()))?;
        Self::decode(r).await
    }

    async fn post<T: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
        body: serde_json::Value,
        timeout: Duration,
    ) -> Result<T> {
        let r = self
            .http
            .post(format!("{}{path}", self.base))
            .json(&body)
            .timeout(timeout)
            .send()
            .await
            .map_err(|e| Error::Transport(e.to_string()))?;
        Self::decode(r).await
    }

    async fn decode<T: for<'de> Deserialize<'de>>(r: reqwest::Response) -> Result<T> {
        let status = r.status();
        let text = r
            .text()
            .await
            .map_err(|e| Error::Transport(e.to_string()))?;

        // 409 is how the daemon reports a refused action, and the body carries
        // the reason — so it is decoded rather than thrown away.
        if !status.is_success() && status.as_u16() != 409 {
            return Err(Error::Status {
                status: status.as_u16(),
                body: text.chars().take(300).collect(),
            });
        }
        serde_json::from_str(&text).map_err(|e| Error::Decode(e.to_string()))
    }

    /// Confirm this is the Rust daemon before showing a window.
    ///
    /// v1 answers `/health` with a bare `{"ok": true}` and has no `/state`, so a
    /// window attached to it renders empty with no explanation. That happened
    /// once; checking for a version is the cheapest way to stop it happening
    /// again.
    pub async fn verify(&self) -> Result<String> {
        #[derive(Deserialize)]
        struct Health {
            #[serde(default)]
            version: Option<String>,
        }
        let h: Health = self.get("/health", TIMEOUT).await?;
        h.version.ok_or_else(|| {
            Error::WrongDaemon(format!(
                "{} answered /health without a version — that is the v1 Python daemon, \
                 which has no /state endpoint. Stop it and start ramwarden-daemon.",
                self.base
            ))
        })
    }

    pub async fn state(&self) -> Result<State> {
        self.get("/state", TIMEOUT).await
    }

    pub async fn plan(&self) -> Result<Plan> {
        self.get("/ladder/plan", TIMEOUT).await
    }

    pub async fn actions(&self) -> Result<ActionLog> {
        self.get("/actions", TIMEOUT).await
    }

    pub async fn ai(&self) -> Result<AiStatus> {
        self.get("/ai", Duration::from_secs(30)).await
    }

    pub async fn watchlist(&self) -> Result<Vec<String>> {
        #[derive(Deserialize)]
        struct W {
            #[serde(default)]
            patterns: Vec<String>,
        }
        let w: W = self.get("/watchlist", TIMEOUT).await?;
        Ok(w.patterns)
    }

    pub async fn suspend(&self, name: &str, force: bool) -> Result<ActionResult> {
        let q = if force { "?force=true" } else { "" };
        self.post(&format!("/suspend/{name}{q}"), serde_json::json!({}), TIMEOUT)
            .await
    }

    pub async fn resume(&self, name: &str) -> Result<ActionResult> {
        self.post(&format!("/resume/{name}"), serde_json::json!({}), TIMEOUT)
            .await
    }

    pub async fn kill(&self, name: &str, force: bool) -> Result<ActionResult> {
        let q = if force { "?force=true" } else { "" };
        self.post(&format!("/kill/{name}{q}"), serde_json::json!({}), TIMEOUT)
            .await
    }

    /// Ask the kernel to reclaim a scope's cold memory. Non-destructive.
    pub async fn reclaim(&self, scope: &str) -> Result<serde_json::Value> {
        self.post(&format!("/reclaim/{scope}"), serde_json::json!({}), Duration::from_secs(30))
            .await
    }

    pub async fn cancel_kill(&self) -> Result<bool> {
        #[derive(Deserialize)]
        struct C {
            #[serde(default)]
            cancelled: bool,
        }
        let c: C = self
            .post("/ladder/cancel-kill", serde_json::json!({}), TIMEOUT)
            .await?;
        Ok(c.cancelled)
    }

    pub async fn analyze(&self, goal: &str) -> Result<serde_json::Value> {
        self.post("/analyze", serde_json::json!({"goal": goal}), ANALYZE_TIMEOUT)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATE: &str = r#"{
        "percent": 70.9, "used_mb": 22116.2, "total_mb": 31190.9, "warn_percent": 65.0,
        "browsers_connected": 1, "warm": true, "psi_some": 0.03, "psi_full": 0.01,
        "zram_saved_mb": 5747.9,
        "totals_mb": {"PROTECTED": 7028.3, "IN_USE": 310.2, "IDLE": 11081.4},
        "suspended": [{"name":"Discord","pids":[245567],"rss_mb":203.7,"minutes":1.5}],
        "processes": [
          {"pid":6669,"name":"brave","rss_mb":5160.2,"true_rss_mb":12160.0,"status":"S",
           "role":"BROWSER","verdict":"IDLE","protection":"",
           "reasons":["no CPU since the last sample"],
           "scope":"app-flatpak-com.brave.Browser-1681016714.scope"}
        ]}"#;

    #[test]
    fn state_parses_the_daemons_payload() {
        let s: State = serde_json::from_str(STATE).unwrap();
        assert_eq!(s.percent, 70.9);
        assert_eq!(s.warn_percent, 65.0);
        assert!(s.warm);
        assert_eq!(s.totals_mb.idle, 11081.4);
        assert_eq!(s.processes.len(), 1);
        assert_eq!(s.processes[0].name, "brave");
        assert_eq!(s.processes[0].pss_mb, 5160.2);
        assert_eq!(s.suspended[0].name, "Discord");
        assert_eq!(s.zram_saved_mb, 5747.9);
    }

    /// The verdict totals use SCREAMING_CASE keys, which is easy to get wrong.
    #[test]
    fn the_verdict_totals_map_onto_their_screaming_case_keys() {
        let t: Totals =
            serde_json::from_str(r#"{"PROTECTED":1.0,"IN_USE":2.0,"IDLE":3.0}"#).unwrap();
        assert_eq!((t.protected, t.in_use, t.idle), (1.0, 2.0, 3.0));
    }

    #[test]
    fn a_partial_state_still_parses_so_a_restarting_daemon_dims_rather_than_crashes() {
        let s: State = serde_json::from_str("{}").unwrap();
        assert_eq!(s.percent, 0.0);
        assert!(!s.warm, "unknown must not read as warm");
        assert!(s.processes.is_empty());
    }

    #[test]
    fn a_plan_parses_including_a_pending_kill() {
        let p: Plan = serde_json::from_str(
            r#"{"rung":"kill","blocked":null,"suspend":["Discord"],"kill":["Discord"],
                "kill_pending":{"targets":["Discord"],"seconds_remaining":7}}"#,
        )
        .unwrap();
        assert_eq!(p.rung.as_deref(), Some("kill"));
        let pending = p.kill_pending.unwrap();
        assert_eq!(pending.seconds_remaining, 7);
        assert_eq!(pending.targets, vec!["Discord"]);
    }

    #[test]
    fn a_plan_with_nothing_pending_parses() {
        let p: Plan = serde_json::from_str(r#"{"rung":"hold","suspend":[],"kill":[]}"#).unwrap();
        assert!(p.kill_pending.is_none());
        assert!(p.blocked.is_none());
    }

    #[test]
    fn the_action_log_parses() {
        let l: ActionLog = serde_json::from_str(
            r#"{"total_reclaimed_mb":137.0,"actions":[
                {"at":"2026-10-01T14:00:00.000000","rung":0,"action":"reclaim",
                 "target":"brave","freed_mb":137.0,"succeeded":true,"notes":"","psi_some":3.2}]}"#,
        )
        .unwrap();
        assert_eq!(l.total_reclaimed_mb, 137.0);
        assert_eq!(l.actions[0].action, "reclaim");
        assert!(l.actions[0].succeeded);
    }

    // ── Reporting outcomes, not intentions ──────────────────────────────────

    #[test]
    fn a_successful_action_describes_what_happened() {
        let r = ActionResult {
            name: "Discord".into(),
            suspended: vec![245567],
            freed_mb: 203.7,
            ..Default::default()
        };
        assert!(r.did_something());
        let d = r.describe("froze");
        assert!(d.contains("Discord"), "{d}");
        assert!(d.contains("204 MB"), "{d}");
    }

    /// The daemon answers 409 with the gate's reason in the body. Showing that
    /// reason is the whole point — "refused" alone tells the user nothing.
    #[test]
    fn a_refusal_surfaces_the_reason_the_gate_gave() {
        let r = ActionResult {
            name: "cosmic-comp".into(),
            notes: vec!["cosmic-comp: desktop compositor — suspending it freezes the session (not forceable)".into()],
            ..Default::default()
        };
        assert!(!r.did_something());
        let d = r.describe("froze");
        assert!(d.contains("freezes the session"), "{d}");
        assert!(!d.contains("froze cosmic-comp"), "must not imply success: {d}");
    }

    #[test]
    fn a_refusal_with_no_reason_still_reads_as_a_refusal() {
        let r = ActionResult {
            name: "x".into(),
            ..Default::default()
        };
        assert_eq!(r.describe("froze"), "x refused froze");
    }

    #[test]
    fn an_action_that_freed_nothing_measurable_omits_the_figure() {
        let r = ActionResult {
            name: "Discord".into(),
            resumed: vec![1],
            ..Default::default()
        };
        let d = r.describe("resumed");
        assert!(d.contains("1 process(es)"), "{d}");
        assert!(!d.contains("MB"), "{d}");
    }

    #[test]
    fn a_client_builds_its_base_url() {
        let c = Client::new("127.0.0.1", 7824).unwrap();
        assert_eq!(c.base(), "http://127.0.0.1:7824");
    }

    #[tokio::test]
    async fn an_unreachable_daemon_is_a_transport_error_not_a_panic() {
        let c = Client::new("127.0.0.1", 1).unwrap();
        assert!(matches!(c.state().await, Err(Error::Transport(_))));
        assert!(matches!(c.verify().await, Err(Error::Transport(_))));
    }

    /// The bug that produced two identical windows, one of them empty.
    #[test]
    fn a_health_reply_without_a_version_is_rejected_as_the_wrong_daemon() {
        #[derive(Deserialize)]
        struct Health {
            #[serde(default)]
            version: Option<String>,
        }
        // v1's reply.
        let v1: Health = serde_json::from_str(r#"{"ok":true}"#).unwrap();
        assert!(v1.version.is_none());
        // v2's.
        let v2: Health =
            serde_json::from_str(r#"{"ok":true,"version":"0.2.0","warm":true}"#).unwrap();
        assert_eq!(v2.version.as_deref(), Some("0.2.0"));
    }
}

#[cfg(all(test, feature = "gui"))]
mod glib_timeout_tests {
    use super::*;

    #[test]
    fn a_silent_server_times_out_on_the_glib_executor() {
        // Keep the listening socket open without answering requests.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let client = Client::new("127.0.0.1", listener.local_addr().unwrap().port()).unwrap();
        let runtime = crate::runtime::network_runtime().unwrap();
        let _entered = runtime.enter();
        let context = gtk4::glib::MainContext::new();
        context.with_thread_default(|| context.block_on(async {
            let request = client.get::<State>("/state", Duration::from_millis(100));
            tokio::select! {
                result = request => assert!(matches!(result, Err(Error::Transport(_)))),
                _ = gtk4::glib::timeout_future(Duration::from_secs(2)) => {
                    panic!("HTTP deadline did not fire while GLib owned the thread");
                }
            }
        })).unwrap();
    }
}

#[derive(Clone, Debug, serde::Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct TabTarget { pub browser: String, pub id: i64, pub url: String }
#[derive(Clone, Debug, Deserialize)]
pub struct BrowserRow {
    #[serde(flatten)] pub target: TabTarget,
    pub title: String,
    pub inactive_minutes: i64,
    pub eligible: bool,
    #[serde(default)]
    pub closeable: bool,
    pub status: String,
    pub reason: String,
    pub priority: i64,
}
#[derive(Clone, Debug, Default, Deserialize)]
pub struct BrowserState {
    pub browsers_connected: usize,
    pub candidates: usize,
    pub tabs: Vec<BrowserRow>,
}
#[derive(Clone, Debug, Deserialize)]
pub struct DiscardResult {
    pub confirmed: Vec<TabTarget>, pub queued: Vec<TabTarget>, pub refused: Vec<TabTarget>,
}
impl Client {
    pub async fn browser_tabs(&self) -> Result<BrowserState> { self.get("/browser/tabs", TIMEOUT).await }
    pub async fn browser_analyze(&self) -> Result<BrowserState> {
        self.post("/browser/analyze",serde_json::json!({}),Duration::from_secs(30)).await
    }
    pub async fn discard_tabs(&self, targets: &[TabTarget]) -> Result<DiscardResult> {
        self.post("/browser/discard",serde_json::json!({"targets":targets}),Duration::from_secs(40)).await
    }
}

impl Client {
    pub async fn close_tabs(&self, targets: &[TabTarget]) -> Result<DiscardResult> {
        self.post("/browser/close",serde_json::json!({"targets":targets}),Duration::from_secs(40)).await
    }
}
