//! ThreatWarden, as the daemon runs it: polling, and the one-click setup.
//!
//! Its first engine is Wazuh; ClamAV, Suricata and network flows feed into
//! the same Wazuh pipeline (`docs/specs/threatwarden.md`).
//!
//! Hosted (the default): RamWarden runs Wazuh. This computer authenticates
//! with its device sign-in, the hosted API enrols it and returns alerts its AI
//! tier has already triaged, and "Turn on" installs Wazuh's own agent behind
//! one OS admin prompt. The user never sees a key, a server or a certificate.
//!
//! Self-hosted (`mode = "self"`): the user's own manager and indexer, through
//! [`ramwarden_wazuh::Client`]. See `docs/specs/wardens.md`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use ramwarden_core::config;
use ramwarden_core::wardens::{self, AgentHealth, Level, Status, ThreatThresholds};
use ramwarden_platform::Secret;
use ramwarden_wazuh::{Alert, Client, Cloud, EnrollRequest, Endpoints, Error, LocalAgent, ManagerAgent};
use serde::Serialize;

/// What ThreatWarden last saw.
#[derive(Clone, Debug, Default, Serialize)]
pub struct WazuhReport {
    pub mode: String,
    pub agent_name: String,
    pub local: LocalAgent,
    /// The manager's view of this computer's agent.
    pub manager: Option<ManagerAgent>,
    /// Hosted: every agent on the account, so one computer can see the others.
    pub agents: Vec<ManagerAgent>,
    pub alerts: Vec<Alert>,
    /// Hosted: the AI tier's one-line summary.
    pub summary: Option<String>,
    /// Why part of the picture is missing. Shown to the user as-is.
    pub problems: Vec<String>,
    /// One line per engine, filled in from the rest of the report.
    pub engines: Vec<Engine>,
}

/// One ThreatWarden engine's line in the pane.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Engine {
    /// `wazuh`, `clamav`, `suricata` or `network`.
    pub id: &'static str,
    pub name: &'static str,
    /// `on`, `off`, `planned` or `error`.
    pub state: &'static str,
    pub detail: String,
}

impl Engine {
    fn new(id: &'static str, name: &'static str, state: &'static str, detail: impl Into<String>) -> Engine {
        Engine { id, name, state, detail: detail.into() }
    }
}

impl WazuhReport {
    /// The engine lines, from the agent's health. Only Wazuh ships so far;
    /// the rest follow `docs/specs/threatwarden.md` and show as `planned`.
    pub fn engine_lines(&self) -> Vec<Engine> {
        let wazuh = match self.health() {
            AgentHealth::Connected => Engine::new("wazuh", "Wazuh agent", "on", "Connected"),
            AgentHealth::Disconnected => Engine::new("wazuh", "Wazuh agent", "error", "Installed, not connected"),
            AgentHealth::Unknown => Engine::new("wazuh", "Wazuh agent", "on", "Installed; connection not confirmed"),
            AgentHealth::Missing => Engine::new("wazuh", "Wazuh agent", "off", "Not installed"),
        };
        vec![
            wazuh,
            Engine::new("clamav", "ClamAV", "planned", "Malware scans, when memory is comfortable"),
            Engine::new("suricata", "Suricata", "planned", "Network intrusion detection, opt-in"),
            Engine::new("network", "Network view", "planned", "Who this computer talks to"),
        ]
    }

    /// Whether the agent can report, from both its own state and the
    /// manager's. Either one saying "connected" is enough; the manager saying
    /// anything but `active` outranks an unreadable local state.
    pub fn health(&self) -> AgentHealth {
        let manager_active = self.manager.as_ref().map(|m| m.status == "active");
        if !self.local.installed && self.manager.is_none() {
            return AgentHealth::Missing;
        }
        if self.local.is_connected() || manager_active == Some(true) {
            return AgentHealth::Connected;
        }
        if manager_active == Some(false) || self.local.status.is_some() {
            return AgentHealth::Disconnected;
        }
        AgentHealth::Unknown
    }
}

/// Where the one-click setup is.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Setup {
    Idle,
    Running,
    Failed(String),
}

/// ThreatWarden's state, shared by the poller and the routes.
pub struct ThreatWarden {
    cfg: config::Wazuh,
    /// Remembers the name the hosted API enrolled this computer under.
    name_file: PathBuf,
    view: RwLock<(Status, Option<WazuhReport>)>,
    setup: Mutex<Setup>,
}

impl ThreatWarden {
    pub fn new(cfg: &config::Config) -> ThreatWarden {
        ThreatWarden {
            cfg: cfg.wardens.threat.clone(),
            name_file: cfg.db_path.with_file_name("threat-agent-name"),
            view: RwLock::new((Status::off("Not checked yet", "", 0), None)),
            setup: Mutex::new(Setup::Idle),
        }
    }

    pub fn enabled(&self) -> bool {
        self.cfg.enabled
    }

    pub fn hosted(&self) -> bool {
        self.cfg.mode != "self"
    }

    fn thresholds(&self) -> ThreatThresholds {
        ThreatThresholds { min_level: self.cfg.min_level, alert_level: self.cfg.alert_level }
    }

    /// The status the UI shows. A running setup outranks the last poll.
    pub fn status(&self) -> Status {
        if !self.cfg.enabled {
            return Status::off("Turned off", "Set [wardens.threat] enabled = true to use it.", 0);
        }
        match &*self.setup.lock().unwrap() {
            Setup::Running => Status::new(
                Level::Setup,
                "Setting up ThreatWarden",
                "Installing the security agent. Approve the permission prompt if your computer shows one.",
                wardens::now(),
            ),
            Setup::Failed(why) => Status::off("Setup didn't finish", format!("{why} Click Turn on to try again."), wardens::now()),
            Setup::Idle => self.view.read().unwrap().0.clone(),
        }
    }

    pub fn report(&self) -> Option<WazuhReport> {
        self.view.read().unwrap().1.clone()
    }

    fn set(&self, status: Status, mut report: Option<WazuhReport>) {
        if let Some(r) = report.as_mut() {
            r.engines = r.engine_lines();
        }
        *self.view.write().unwrap() = (status, report);
    }

    fn enrolled_name(&self) -> Option<String> {
        std::fs::read_to_string(&self.name_file).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
    }

    fn cloud(&self) -> std::result::Result<Cloud, Status> {
        let Some(token) = config::device_token() else {
            return Err(Status::off(
                "Sign in to RamWarden",
                "Sign in on this computer to connect it to ThreatWarden.",
                wardens::now(),
            ));
        };
        Cloud::new(&self.cfg.cloud_url, Secret::new(token), Duration::from_secs(self.cfg.timeout_seconds))
            .map_err(|e| Status::off("RamWarden's address is misconfigured", e.to_string(), wardens::now()))
    }

    /// One poll. The caller has checked the warden is unlocked.
    pub async fn poll(&self, self_hosted: Option<&Client>) {
        if !self.cfg.enabled {
            return;
        }
        if self.hosted() {
            self.poll_hosted().await
        } else {
            self.poll_self(self_hosted).await
        }
    }

    async fn poll_hosted(&self) {
        let now = wardens::now();
        let mut report = WazuhReport {
            mode: "hosted".into(),
            agent_name: self.enrolled_name().unwrap_or_default(),
            local: LocalAgent::read(),
            ..Default::default()
        };
        let cloud = match self.cloud() {
            Ok(c) => c,
            Err(status) => return self.set(status, Some(report)),
        };
        let s = match cloud.status().await {
            Ok(s) => s,
            Err(Error::SignedOut) => {
                return self.set(
                    Status::off("Sign in to RamWarden again", "This computer's sign-in has expired.", now),
                    Some(report),
                );
            }
            Err(Error::NoPass) => {
                return self.set(
                    Status::off("Your ThreatWarden pass isn't active", "Renew your ThreatWarden pass, or a bundle that includes it.", now),
                    Some(report),
                );
            }
            Err(e) => {
                report.problems.push(e.to_string());
                let status = if report.local.is_connected() {
                    Status::new(Level::Watch, "Can't reach ThreatWarden's service", e.to_string(), now)
                } else {
                    Status::off("Can't reach ThreatWarden's service", e.to_string(), now)
                };
                return self.set(status, Some(report));
            }
        };
        report.manager = match report.agent_name.as_str() {
            "" if s.agents.len() == 1 && report.local.installed => s.agents.first().cloned(),
            "" => None,
            name => s.agents.iter().find(|a| a.name == name).cloned(),
        };
        report.agents = s.agents;
        report.summary = s.summary;
        report.alerts = s.alerts;
        let t = self.thresholds();
        let levels: Vec<u8> = report
            .alerts
            .iter()
            .map(|a| wardens::effective_level(a.level, a.verdict.as_deref(), t))
            .collect();
        let mut status = match report.health() {
            AgentHealth::Missing => Status::off(
                "Ready to turn on",
                "Click Turn on to install the security agent. Your computer will ask for permission once.",
                now,
            ),
            health => wardens::assess_threats(health, &levels, t, now),
        };
        if let Some(summary) = &report.summary
            && matches!(status.level, Level::Clear | Level::Watch | Level::Alert)
        {
            status.detail = summary.clone();
        }
        self.set(status, Some(report));
    }

    async fn poll_self(&self, client: Option<&Client>) {
        let c = &self.cfg;
        let mut report = WazuhReport {
            mode: "self".into(),
            agent_name: if c.agent_name.trim().is_empty() {
                ramwarden_wazuh::default_agent_name()
            } else {
                c.agent_name.trim().to_string()
            },
            local: LocalAgent::read(),
            ..Default::default()
        };
        let mut levels = Vec::new();
        match client {
            None => report.problems.push("No Wazuh manager configured; showing this computer's agent only.".into()),
            Some(client) => {
                if client.has_manager() {
                    match client.agent(&report.agent_name).await {
                        Ok(Some(a)) => report.manager = Some(a),
                        Ok(None) => report.problems.push(format!(
                            "The manager has no agent named \"{}\". Set [wardens.threat] agent_name if it enrolled under another name.",
                            report.agent_name
                        )),
                        Err(e) => report.problems.push(e.to_string()),
                    }
                }
                if client.has_indexer() {
                    match client.alerts(&report.agent_name, c.min_level, c.lookback_hours).await {
                        Ok(a) => {
                            levels = a.iter().map(|x| x.level).collect();
                            report.alerts = a;
                        }
                        Err(e) => report.problems.push(e.to_string()),
                    }
                } else {
                    report.problems.push("No indexer configured, so alerts are not shown.".into());
                }
            }
        }
        let mut status = wardens::assess_threats(report.health(), &levels, self.thresholds(), wardens::now());
        // "Clear" must not hide that part of the picture is missing.
        if status.level == Level::Clear && !report.problems.is_empty() {
            let why = report.problems.join(" ");
            if report.manager.is_none() && !report.local.is_connected() {
                status = Status::off("Cannot reach Wazuh", why, status.checked_at);
            } else {
                status.detail = format!("{} {why}", status.detail);
            }
        }
        self.set(status, Some(report));
    }

    /// Start the one-click setup: enrol with the hosted API, then install
    /// Wazuh's agent behind one OS admin prompt. Returns at once; progress is
    /// the warden's status. The caller has checked the warden is unlocked.
    pub fn start_setup(self: &Arc<Self>, handle: &tokio::runtime::Handle) -> std::result::Result<(), String> {
        if !self.cfg.enabled {
            return Err("ThreatWarden is turned off in the config.".into());
        }
        if !self.hosted() {
            return Err("Self-hosted Wazuh is set up by hand; see docs/threatwarden-self-hosting.md.".into());
        }
        let cloud = self.cloud().map_err(|s| s.headline)?;
        {
            let mut setup = self.setup.lock().unwrap();
            if *setup == Setup::Running {
                return Err("Setup is already running.".into());
            }
            *setup = Setup::Running;
        }
        let me = self.clone();
        handle.spawn(async move {
            let outcome = async {
                let e = cloud.enroll(&EnrollRequest::here()).await?;
                ramwarden_wazuh::install::install(&e).await?;
                let _ = std::fs::write(&me.name_file, &e.agent_name);
                Ok::<_, Error>(e.agent_name)
            }
            .await;
            *me.setup.lock().unwrap() = match &outcome {
                Ok(name) => {
                    tracing::info!("threatwarden: agent installed and enrolled as {name}");
                    Setup::Idle
                }
                Err(e) => {
                    tracing::warn!("threatwarden setup failed: {e}");
                    Setup::Failed(sentence(&e.to_string()))
                }
            };
            if outcome.is_ok() {
                me.poll(None).await;
            }
        });
        Ok(())
    }

    /// Poll forever. A self-hosted configuration error is reported as the
    /// warden's status, once, and the poller stops. `unlocked` is asked before
    /// every poll, so a locked warden never contacts anything.
    pub fn spawn(self: Arc<Self>, handle: &tokio::runtime::Handle, unlocked: impl Fn() -> bool + Send + 'static) {
        if !self.cfg.enabled {
            return;
        }
        handle.spawn(async move {
            let client = if self.hosted() {
                None
            } else {
                match self_hosted_client(&self.cfg) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!("threatwarden off: {e}");
                        self.set(Status::off("Wazuh settings need fixing", e, wardens::now()), None);
                        return;
                    }
                }
            };
            let every = Duration::from_secs(self.cfg.poll_seconds.max(30));
            tracing::info!("threatwarden ({}): polling every {}s", self.cfg.mode, every.as_secs());
            loop {
                if unlocked() {
                    self.poll(client.as_ref()).await;
                }
                tokio::time::sleep(every).await;
            }
        });
    }
}

fn sentence(s: &str) -> String {
    let mut s = s.trim().to_string();
    if let Some(first) = s.get(0..1) {
        s.replace_range(0..1, &first.to_uppercase());
    }
    if !s.ends_with('.') {
        s.push('.');
    }
    s
}

/// `Ok(None)` when neither manager nor indexer is configured.
pub fn self_hosted_client(c: &config::Wazuh) -> std::result::Result<Option<Client>, String> {
    if c.manager_url.trim().is_empty() && c.indexer_url.trim().is_empty() {
        return Ok(None);
    }
    let ca_pem = match &c.ca_file {
        Some(p) => Some(std::fs::read(p).map_err(|e| format!("cannot read ca_file {}: {e}", p.display()))?),
        None => None,
    };
    Client::new(Endpoints {
        manager_url: c.manager_url.clone(),
        api_user: c.api_user.clone(),
        api_password: Secret::new(c.api_password.clone()),
        indexer_url: c.indexer_url.clone(),
        indexer_user: c.indexer_user.clone(),
        indexer_password: Secret::new(c.indexer_password.clone()),
        ca_pem,
        timeout: Duration::from_secs(c.timeout_seconds),
    })
    .map(Some)
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(installed: bool, local: Option<&str>, manager: Option<&str>) -> WazuhReport {
        WazuhReport {
            local: LocalAgent { installed, status: local.map(str::to_string), last_keepalive: None },
            manager: manager.map(|s| ManagerAgent {
                id: "001".into(),
                name: "desk".into(),
                status: s.into(),
                version: None,
                last_keep_alive: None,
            }),
            ..Default::default()
        }
    }

    #[test]
    fn health_combines_the_local_and_manager_views() {
        assert_eq!(report(false, None, None).health(), AgentHealth::Missing);
        assert_eq!(report(true, None, None).health(), AgentHealth::Unknown);
        assert_eq!(report(true, Some("connected"), None).health(), AgentHealth::Connected);
        assert_eq!(report(true, None, Some("active")).health(), AgentHealth::Connected);
        assert_eq!(report(true, None, Some("disconnected")).health(), AgentHealth::Disconnected);
        assert_eq!(report(true, Some("pending"), None).health(), AgentHealth::Disconnected);
        assert_eq!(report(false, None, Some("never_connected")).health(), AgentHealth::Disconnected);
    }

    #[test]
    fn engines_follow_the_agent_and_list_the_roadmap() {
        let state = |r: WazuhReport| r.engine_lines().iter().map(|e| (e.id, e.state)).collect::<Vec<_>>();
        assert_eq!(
            state(report(true, Some("connected"), None)),
            [("wazuh", "on"), ("clamav", "planned"), ("suricata", "planned"), ("network", "planned")]
        );
        assert_eq!(state(report(false, None, None))[0], ("wazuh", "off"));
        assert_eq!(state(report(true, None, Some("disconnected")))[0], ("wazuh", "error"));
    }

    #[test]
    fn a_misconfigured_manager_url_is_reported_not_fatal() {
        let c = config::Wazuh { manager_url: "http://wazuh.lan:55000".into(), ..Default::default() };
        let e = self_hosted_client(&c).err().unwrap();
        assert!(e.contains("https"), "{e}");
        assert!(self_hosted_client(&config::Wazuh::default()).unwrap().is_none());
    }

    #[test]
    fn errors_read_as_sentences() {
        assert_eq!(sentence("the agent was not installed"), "The agent was not installed.");
        assert_eq!(sentence("Done."), "Done.");
    }
}
