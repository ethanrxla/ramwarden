//! The wardens, as the windows show them: the `/wardens` API, the
//! ThreatWarden pane's content, and the wardens' 8-bit icons.
//!
//! Every client shows RamWarden as panes, one per warden: the RAM pane is the
//! memory view each platform already had, and the ThreatWarden pane is built
//! here. Both GUIs (GTK on Linux, egui on Windows) render a [`ThreatPane`] and
//! nothing else, so what the pane says — the level words, which button it
//! offers, how an alert is labelled — is decided once, without a display, and
//! tested. The Mac menu bar (`macos/menubar/Wardens.swift`) mirrors these rules
//! in Swift because it is not a Rust program.
//!
//! The contract is `docs/specs/wardens.md`; what ThreatWarden is made of
//! (Wazuh, ClamAV, Suricata, a network view) is `docs/specs/threatwarden.md`.
//! The pane shows their output in RamWarden's own style and never opens or
//! embeds their dashboards.
//!
//! **Degrade, don't fail.** A daemon from before the wardens answers `/wardens`
//! with 404; the pane then says to update rather than going blank. Fields the
//! daemon may add later (`upgrade_url`, `price`, `report.engines`) are read
//! when present and fall back to the values below when not.

use std::time::Duration;

use serde::Deserialize;

use crate::client::{Client, Error, Result, TIMEOUT};

/// Where "Get a pass" goes until the daemon names a page of its own.
pub const UPGRADE_URL: &str = "https://ramwarden.com/#pricing";

/// ThreatWarden's price, shown on the locked pane until the daemon reports one.
/// ThreatWarden comes with RamWarden Pro ($5 a month or $24 a year, 14-day
/// trial). The account server is the authority; this is only the fallback.
pub const THREAT_PRICE: &str = "Included with RamWarden Pro";

/// The upgrade button's label, on every platform.
pub const UPGRADE_LABEL: &str = "Get RamWarden Pro";

pub const THREAT: &str = "threat";

/// How much a warden needs the user right now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Clear,
    Watch,
    Alert,
    #[default]
    Off,
    Locked,
    Setup,
    /// A level a newer daemon added. Shown neutrally rather than refused.
    #[serde(other)]
    Unknown,
}

impl Level {
    /// The badge text, in the user's words.
    pub fn label(self) -> &'static str {
        match self {
            Level::Clear => "All clear",
            Level::Watch => "Worth a look",
            Level::Alert => "Act now",
            Level::Off => "Off",
            Level::Locked => "Locked",
            Level::Setup => "Setting up",
            Level::Unknown => "Unknown",
        }
    }

    /// The same greens, ambers and reds as the process legend
    /// (`model::verdict_colour`), and the brand crimson for "locked" so the
    /// upgrade reads as RamWarden's, not as an error.
    pub fn colour(self) -> &'static str {
        match self {
            Level::Clear => "#4ade80",
            Level::Watch => "#facc15",
            Level::Alert => "#f87171",
            Level::Setup => "#60a5fa",
            Level::Locked => "#f24455",
            Level::Off | Level::Unknown => "#9ca3af",
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Status {
    #[serde(default)]
    pub level: Level,
    #[serde(default)]
    pub headline: String,
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub checked_at: u64,
}

/// One warden, from `/wardens` or `/wardens/{id}`.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Warden {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub role: String,
    /// Daemon path of the warden's SVG, or `None` for the RAM warden, which
    /// the clients draw as the brand ram.
    #[serde(default)]
    pub icon: Option<String>,
    /// `free` or `pro`.
    #[serde(default)]
    pub access: String,
    #[serde(default)]
    pub unlocked: bool,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub status: Status,
    #[serde(default)]
    pub upgrade_url: Option<String>,
    #[serde(default)]
    pub price: Option<String>,
    /// Only `/wardens/{id}` carries it, and only while unlocked.
    #[serde(default)]
    pub report: serde_json::Value,
}

impl Warden {
    /// The ThreatWarden report, if this entry has one.
    pub fn threat_report(&self) -> Option<ThreatReport> {
        if self.report.is_null() {
            return None;
        }
        serde_json::from_value(self.report.clone()).ok()
    }
}

/// `GET /wardens`.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct WardenList {
    #[serde(default)]
    pub paid: Vec<String>,
    #[serde(default)]
    pub wardens: Vec<Warden>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct LocalAgent {
    #[serde(default)]
    pub installed: bool,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub last_keepalive: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ManagerAgent {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub last_keep_alive: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Alert {
    #[serde(default)]
    pub timestamp: String,
    #[serde(default)]
    pub rule_id: String,
    #[serde(default)]
    pub level: u8,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub groups: Vec<String>,
    #[serde(default)]
    pub explanation: Option<String>,
    #[serde(default)]
    pub advice: Option<String>,
    /// `benign`, `suspicious` or `threat`.
    #[serde(default)]
    pub verdict: Option<String>,
    #[serde(default)]
    pub confidence: Option<f64>,
}

/// One engine's own line, when the daemon reports it.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct Engine {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    /// `on`, `off`, `planned` or `error`.
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub detail: String,
}

/// What ThreatWarden last saw, from `/wardens/threat`.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ThreatReport {
    #[serde(default)]
    pub sensors: Sensors,
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub agent_name: String,
    #[serde(default)]
    pub local: LocalAgent,
    #[serde(default)]
    pub manager: Option<ManagerAgent>,
    #[serde(default)]
    pub agents: Vec<ManagerAgent>,
    #[serde(default)]
    pub alerts: Vec<Alert>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub problems: Vec<String>,
    #[serde(default)]
    pub engines: Vec<Engine>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Sensors { #[serde(default)] pub network: Option<NetworkSnapshot> }
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct NetworkSnapshot {
    #[serde(default)] pub checked_at:u64,
    #[serde(default)] pub connections:Vec<Connection>,
    #[serde(default)] pub problem:Option<String>,
    #[serde(default)] pub scope:String,
}
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Connection {
    #[serde(default)] pub pid:u32,
    #[serde(default)] pub process:String,
    #[serde(default)] pub local:String,
    #[serde(default)] pub remote:String,
}

/// The answer to "Turn on".
#[derive(Clone, Debug, Default, Deserialize)]
pub struct SetupReply {
    #[serde(default)]
    pub started: bool,
    #[serde(default)]
    pub error: Option<String>,
}

impl Client {
    pub async fn wardens(&self) -> Result<WardenList> {
        self.get("/wardens", TIMEOUT).await
    }

    pub async fn warden(&self, id: &str) -> Result<Warden> {
        self.get(&format!("/wardens/{id}"), TIMEOUT).await
    }

    /// Start a warden's one-click setup. The daemon answers at once; progress
    /// shows in the warden's status.
    pub async fn warden_setup(&self, id: &str) -> Result<SetupReply> {
        self.post(
            &format!("/wardens/{id}/setup"),
            serde_json::json!({}),
            Duration::from_secs(20),
        )
        .await
    }

    /// A warden's icon, by the path `/wardens` gave for it.
    pub async fn warden_icon(&self, path: &str) -> Result<Sprite> {
        let svg = self.get_text(path, TIMEOUT).await?;
        Sprite::from_svg(&svg).ok_or_else(|| Error::Decode(format!("{path} is not a pixel sprite")))
    }

    /// The ThreatWarden pane, whatever state the daemon is in.
    pub async fn threat_pane(&self) -> ThreatPane {
        match self.warden(THREAT).await {
            Ok(w) => ThreatPane::from_warden(&w),
            Err(Error::Status { status: 404, .. }) => ThreatPane::unavailable(
                "This RamWarden is older than ThreatWarden",
                "Update RamWarden to get the ThreatWarden pane.",
            ),
            Err(e) => ThreatPane::unavailable("Cannot reach the daemon", &e.to_string()),
        }
    }
}

// ── The pane ────────────────────────────────────────────────────────────────

/// The one button the pane offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaneAction {
    None,
    /// No pass: open the pricing page.
    Upgrade {
        url: String,
        price: String,
    },
    /// Unlocked but not set up: install and enrol (one OS admin prompt).
    TurnOn,
}

impl PaneAction {
    pub fn is_upgrade(&self) -> bool {
        matches!(self, PaneAction::Upgrade { .. })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineLine {
    pub name: String,
    pub state: String,
    pub detail: String,
}

impl EngineLine {
    pub fn colour(&self) -> &'static str {
        match self.state.as_str() {
            "on" => Level::Clear.colour(),
            "error" => Level::Watch.colour(),
            "planned" => Level::Setup.colour(),
            _ => Level::Off.colour(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct AlertLine {
    /// `MM-DD HH:MM`, from the alert's timestamp.
    pub when: String,
    /// Which engine raised it, from its Wazuh groups.
    pub engine: &'static str,
    pub title: String,
    /// The AI triage's plain-language explanation, else empty.
    pub explanation: String,
    pub advice: String,
    /// `threat`, `suspicious`, `benign`, or the Wazuh rule level.
    pub tag: String,
    pub colour: &'static str,
}

/// Everything the ThreatWarden pane shows.
#[derive(Clone, Debug, PartialEq)]
pub struct ThreatPane {
    pub network: Option<NetworkSnapshot>,
    pub unlocked: bool,
    pub name: String,
    pub level: Level,
    pub headline: String,
    pub detail: String,
    /// The AI tier's one-line summary of the day.
    pub summary: Option<String>,
    pub action: PaneAction,
    pub engines: Vec<EngineLine>,
    pub alerts: Vec<AlertLine>,
    /// Other computers on the account, as `name: status`.
    pub agents: Vec<String>,
    pub problems: Vec<String>,
    /// The daemon path of the cyborg icon.
    pub icon: Option<String>,
}

/// The engines in the order they ship (`docs/specs/threatwarden.md`).
const ENGINES: [(&str, &str); 4] = [
    ("wazuh", "Wazuh agent"),
    ("clamav", "ClamAV malware scans"),
    ("suricata", "Suricata network IDS"),
    ("network", "Network view"),
];

impl ThreatPane {
    /// At most this many alerts are drawn; the daemon keeps up to 50.
    pub const MAX_ALERTS: usize = 8;

    pub fn unavailable(headline: &str, detail: &str) -> ThreatPane {
        ThreatPane {
            network:None,
            unlocked: false,
            name: "ThreatWarden".into(),
            level: Level::Off,
            headline: headline.into(),
            detail: detail.into(),
            summary: None,
            action: PaneAction::None,
            engines: Vec::new(),
            alerts: Vec::new(),
            agents: Vec::new(),
            problems: Vec::new(),
            icon: Some("/wardens/threat/icon.svg".into()),
        }
    }

    pub fn from_warden(w: &Warden) -> ThreatPane {
        let report = w.unlocked.then(|| w.threat_report()).flatten();
        let level = if w.unlocked {
            w.status.level
        } else {
            Level::Locked
        };
        let action = if !w.unlocked {
            PaneAction::Upgrade {
                url: w.upgrade_url.clone().unwrap_or_else(|| UPGRADE_URL.into()),
                price: w.price.clone().unwrap_or_else(|| THREAT_PRICE.into()),
            }
        } else if w.enabled
            && level == Level::Off
            && !report.as_ref().is_some_and(|r| r.local.installed)
        {
            PaneAction::TurnOn
        } else {
            PaneAction::None
        };
        let mut pane = ThreatPane {
            network:None,
            unlocked: w.unlocked,
            name: if w.name.is_empty() {
                "ThreatWarden".into()
            } else {
                w.name.clone()
            },
            level,
            headline: w.status.headline.clone(),
            detail: if w.status.detail.is_empty() && !w.unlocked {
                w.role.clone()
            } else {
                w.status.detail.clone()
            },
            summary: None,
            action,
            engines: Vec::new(),
            alerts: Vec::new(),
            agents: Vec::new(),
            problems: Vec::new(),
            icon: w.icon.clone(),
        };
        // Locked wardens do no work, so there is nothing else to show: the
        // engines list is the sales pitch instead.
        let Some(r) = report else {
            if !w.unlocked {
                pane.engines = ENGINES
                    .iter()
                    .map(|(_, name)| EngineLine {
                        name: (*name).into(),
                        state: "locked".into(),
                        detail: String::new(),
                    })
                    .collect();
            }
            return pane;
        };
        pane.summary = r.summary.clone().filter(|s| !s.is_empty());
        pane.network=r.sensors.network.clone();
        if let Some(network)=&mut pane.network {network.connections.truncate(512);}
        pane.engines = Self::engines(&r);
        pane.alerts = r
            .alerts
            .iter()
            .take(Self::MAX_ALERTS)
            .map(AlertLine::from_alert)
            .collect();
        pane.agents = r
            .agents
            .iter()
            .filter(|a| a.name != r.agent_name)
            .map(|a| format!("{}: {}", a.name, a.status.replace('_', " ")))
            .collect();
        pane.problems = r.problems.clone();
        pane
    }

    /// The daemon's own engine lines when it sends them; otherwise Wazuh's line
    /// from agent health and the others from the alerts they have raised.
    fn engines(r: &ThreatReport) -> Vec<EngineLine> {
        if !r.engines.is_empty() {
            return r
                .engines
                .iter()
                .map(|e| EngineLine {
                    name: e.name.clone(),
                    state: e.state.clone(),
                    detail: e.detail.clone(),
                })
                .collect();
        }
        ENGINES
            .iter()
            .map(|(id, name)| {
                let (state, detail) = match *id {
                    "wazuh" => Self::agent_line(r),
                    _ => {
                        let n = r
                            .alerts
                            .iter()
                            .filter(|a| engine_of(&a.groups) == engine_name(id))
                            .count();
                        if n > 0 {
                            (
                                "on",
                                format!("{n} alert{} recently", if n == 1 { "" } else { "s" }),
                            )
                        } else {
                            ("planned", "Arrives in a later update".to_string())
                        }
                    }
                };
                EngineLine {
                    name: (*name).into(),
                    state: state.into(),
                    detail,
                }
            })
            .collect()
    }

    /// The same order of evidence as the daemon's `WazuhReport::health`: either
    /// side saying "connected" is enough; the manager saying otherwise outranks
    /// an unreadable local state.
    fn agent_line(r: &ThreatReport) -> (&'static str, String) {
        let manager = r.manager.as_ref().map(|m| m.status.as_str());
        let local = r.local.status.as_deref();
        if !r.local.installed && manager.is_none() {
            return ("off", "Not installed".into());
        }
        if local == Some("connected") || manager == Some("active") {
            let seen = r
                .local
                .last_keepalive
                .clone()
                .or_else(|| r.manager.as_ref().and_then(|m| m.last_keep_alive.clone()))
                .map(|t| format!(", last check-in {}", short_time(&t)))
                .unwrap_or_default();
            return ("on", format!("Connected{seen}"));
        }
        match manager.or(local) {
            Some(s) => ("error", format!("Installed, {}", s.replace('_', " "))),
            None => ("error", "Installed, connection unknown".into()),
        }
    }
}

impl AlertLine {
    fn from_alert(a: &Alert) -> AlertLine {
        let (tag, colour) = match a.verdict.as_deref() {
            Some("threat") => ("threat".to_string(), Level::Alert.colour()),
            Some("suspicious") => ("suspicious".to_string(), Level::Watch.colour()),
            Some("benign") => ("benign".to_string(), Level::Clear.colour()),
            _ if a.level >= 12 => (format!("level {}", a.level), Level::Alert.colour()),
            _ => (format!("level {}", a.level), Level::Watch.colour()),
        };
        AlertLine {
            when: short_time(&a.timestamp),
            engine: engine_of(&a.groups),
            title: a.description.clone(),
            explanation: a.explanation.clone().unwrap_or_default(),
            advice: a.advice.clone().unwrap_or_default(),
            tag,
            colour,
        }
    }
}

/// The engine an alert came from, by its Wazuh rule groups.
pub fn engine_of(groups: &[String]) -> &'static str {
    let has = |g: &str| groups.iter().any(|x| x.eq_ignore_ascii_case(g));
    if has("clamav") || has("freshclam") || has("virus") {
        "Antivirus"
    } else if has("suricata") || has("ids") {
        "Threat detection"
    } else if has("syscheck") || has("fim") {
        "File integrity"
    } else if has("rootcheck") {
        "Rootkit check"
    } else if has("sca") {
        "Config check"
    } else if has("vulnerability-detector") {
        "Vulnerable software"
    } else {
        "Sentinel"
    }
}

fn engine_name(id: &str) -> &'static str {
    match id {
        "clamav" => "Antivirus",
        "suricata" => "Threat detection",
        _ => "",
    }
}

/// `2026-11-01T13:05:44.123+0000` → `11-01 13:05`. Anything else is kept.
fn short_time(t: &str) -> String {
    match (t.get(5..10), t.get(11..16)) {
        (Some(d), Some(hm))
            if t.as_bytes()
                .get(10)
                .is_some_and(|c| *c == b'T' || *c == b' ') =>
        {
            format!("{d} {hm}")
        }
        _ => t.to_string(),
    }
}

// ── Icons ───────────────────────────────────────────────────────────────────

/// A warden's 8-bit icon as RGBA pixels.
///
/// The icons are pixel sprites (`scripts/warden-sprites.py`): an SVG made only
/// of axis-aligned `<rect>`s on a 48x48 grid. Reading those rects is all the
/// SVG support the clients need, so neither GUI pulls in an SVG renderer, and
/// the sprite stays crisp at any whole-number scale.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sprite {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Sprite {
    pub fn from_svg(svg: &str) -> Option<Sprite> {
        let root = svg.find("<svg")?;
        let tag = &svg[root..root + svg[root..].find('>')?];
        let view: Vec<u32> = attr(tag, "viewBox")?
            .split_whitespace()
            .filter_map(|v| v.parse().ok())
            .collect();
        let [_, _, width, height] = view[..] else {
            return None;
        };
        if width == 0 || height == 0 || width > 512 || height > 512 {
            return None;
        }
        let mut rgba = vec![0u8; (width * height * 4) as usize];
        let mut drawn = false;
        for piece in svg.split("<rect").skip(1) {
            let tag = &piece[..piece.find('>')?];
            let num = |k| attr(tag, k).and_then(|v| v.parse::<u32>().ok());
            let (Some(x), Some(y), Some(w), Some(h)) =
                (num("x"), num("y"), num("width"), num("height"))
            else {
                continue;
            };
            let Some(c) = attr(tag, "fill").and_then(hex) else {
                continue;
            };
            for yy in y..(y + h).min(height) {
                for xx in x..(x + w).min(width) {
                    let i = ((yy * width + xx) * 4) as usize;
                    rgba[i..i + 4].copy_from_slice(&[c[0], c[1], c[2], 255]);
                    drawn = true;
                }
            }
        }
        drawn.then_some(Sprite {
            width,
            height,
            rgba,
        })
    }

    /// Nearest-neighbour upscale, so pixels stay pixels.
    pub fn scaled(&self, k: u32) -> Sprite {
        let k = k.max(1);
        let (w, h) = (self.width * k, self.height * k);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let s = (((y / k) * self.width + x / k) * 4) as usize;
                let d = ((y * w + x) * 4) as usize;
                rgba[d..d + 4].copy_from_slice(&self.rgba[s..s + 4]);
            }
        }
        Sprite {
            width: w,
            height: h,
            rgba,
        }
    }
}

fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let needle = format!(" {name}=\"");
    let start = tag.find(&needle)? + needle.len();
    let end = tag[start..].find('"')?;
    Some(&tag[start..start + end])
}

fn hex(s: &str) -> Option<[u8; 3]> {
    let s = s.strip_prefix('#')?;
    let v = u32::from_str_radix(s, 16).ok()?;
    match s.len() {
        6 => Some([(v >> 16) as u8, (v >> 8) as u8, v as u8]),
        3 => Some([
            ((v >> 8) & 0xf) as u8 * 17,
            ((v >> 4) & 0xf) as u8 * 17,
            (v & 0xf) as u8 * 17,
        ]),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn warden(json: &str) -> Warden {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn network_details_require_the_unlocked_report_and_are_bounded() {
        let mut value:serde_json::Value=serde_json::from_str(LOCKED).unwrap();
        value["report"]=serde_json::json!({"sensors":{"network":{"checked_at":123,"scope":"Local TCP only","connections":(0..520).map(|pid|serde_json::json!({"pid":pid,"process":"fixture","local":"127.0.0.1:12","remote":"192.0.2.1:443"})).collect::<Vec<_>>()}}});
        let locked:Warden=serde_json::from_value(value.clone()).unwrap();
        assert!(ThreatPane::from_warden(&locked).network.is_none());
        value["unlocked"]=serde_json::json!(true);
        let unlocked:Warden=serde_json::from_value(value).unwrap();
        let pane=ThreatPane::from_warden(&unlocked);
        let network=pane.network.unwrap();
        assert_eq!(network.connections.len(),512);
        assert_eq!(network.connections[0].remote,"192.0.2.1:443");
        assert_eq!(network.checked_at,123);
    }

    const LOCKED: &str = r#"{"id":"threat","name":"ThreatWarden","role":"Watches for attacks.","icon":"/wardens/threat/icon.svg",
        "access":"pro","unlocked":false,"enabled":true,
        "status":{"level":"locked","headline":"ThreatWarden is a Pro warden","detail":"Get a pass.","checked_at":0},"report":null}"#;

    #[test]
    fn a_locked_warden_offers_the_upgrade_and_nothing_else() {
        let p = ThreatPane::from_warden(&warden(LOCKED));
        assert_eq!(p.level, Level::Locked);
        assert_eq!(
            p.action,
            PaneAction::Upgrade {
                url: UPGRADE_URL.into(),
                price: THREAT_PRICE.into()
            }
        );
        assert!(p.alerts.is_empty());
        assert_eq!(
            p.engines.len(),
            4,
            "the locked pane lists what the pass unlocks"
        );
        assert!(p.engines.iter().all(|e| e.state == "locked"));
        assert_eq!(p.icon.as_deref(), Some("/wardens/threat/icon.svg"));
    }

    #[test]
    fn the_daemon_can_name_its_own_price_and_page() {
        let mut w = warden(LOCKED);
        w.upgrade_url = Some("https://example.test/buy".into());
        w.price = Some("$5".into());
        let p = ThreatPane::from_warden(&w);
        assert_eq!(
            p.action,
            PaneAction::Upgrade {
                url: "https://example.test/buy".into(),
                price: "$5".into()
            }
        );
    }

    #[test]
    fn locked_wardens_hide_stale_reports() {
        let mut w = warden(LOCKED);
        w.report = serde_json::json!({
            "summary": "stale summary",
            "alerts": [{"description": "stale alert"}],
            "agents": [{"name": "other computer"}],
            "problems": ["stale problem"]
        });
        let p = ThreatPane::from_warden(&w);
        assert!(p.summary.is_none());
        assert!(p.alerts.is_empty());
        assert!(p.agents.is_empty());
        assert!(p.problems.is_empty());
        assert!(p.engines.iter().all(|e| e.state == "locked"));
    }

    #[test]
    fn unlocked_is_never_shown_as_locked_even_if_status_lags() {
        let w = warden(r#"{"id":"threat","unlocked":false,"status":{"level":"clear"}}"#);
        assert_eq!(ThreatPane::from_warden(&w).level, Level::Locked);
    }

    #[test]
    fn unlocked_but_not_installed_offers_turn_on() {
        let w = warden(
            r#"{"id":"threat","unlocked":true,"enabled":true,
            "status":{"level":"off","headline":"ThreatWarden is not set up yet","detail":"Turn it on."},
            "report":{"mode":"hosted","local":{"installed":false},"alerts":[]}}"#,
        );
        let p = ThreatPane::from_warden(&w);
        assert_eq!(p.action, PaneAction::TurnOn);
        assert_eq!(
            p.engines[0],
            EngineLine {
                name: "Wazuh agent".into(),
                state: "off".into(),
                detail: "Not installed".into()
            }
        );
        assert_eq!(p.engines[1].state, "planned");
    }

    #[test]
    fn an_installed_agent_that_is_off_is_not_offered_a_reinstall() {
        let w = warden(
            r#"{"id":"threat","unlocked":true,"enabled":true,"status":{"level":"off","headline":"Manager unreachable"},
            "report":{"local":{"installed":true,"status":"disconnected"}}}"#,
        );
        let p = ThreatPane::from_warden(&w);
        assert_eq!(p.action, PaneAction::None);
        assert_eq!(p.engines[0].state, "error");
        assert_eq!(p.engines[0].detail, "Installed, disconnected");
    }

    #[test]
    fn alerts_carry_their_engine_and_the_ai_verdict() {
        let w = warden(
            r#"{"id":"threat","name":"ThreatWarden","unlocked":true,"enabled":true,
            "status":{"level":"alert","headline":"1 serious alert","detail":""},
            "report":{"mode":"hosted","agent_name":"me","local":{"installed":true,"status":"connected","last_keepalive":"2026-11-01T13:00:00Z"},
              "manager":{"name":"me","status":"active"},
              "agents":[{"name":"me","status":"active"},{"name":"laptop","status":"never_connected"}],
              "summary":"One thing to look at today.",
              "alerts":[
                {"timestamp":"2026-11-01T13:05:44.123+0000","rule_id":"52502","level":12,"description":"ClamAV: virus detected",
                 "groups":["clamav","virus"],"explanation":"A downloaded file matched a known trojan.","advice":"Delete it.","verdict":"threat","confidence":0.97},
                {"timestamp":"2026-11-01T12:00:00Z","rule_id":"86601","level":8,"description":"Suricata: ET SCAN",
                 "groups":["ids","suricata"]},
                {"timestamp":"2026-11-01T11:00:00Z","rule_id":"550","level":7,"description":"Integrity checksum changed.","groups":["ossec","syscheck"],"verdict":"benign"}
              ]}}"#,
        );
        let p = ThreatPane::from_warden(&w);
        assert_eq!(p.level, Level::Alert);
        assert_eq!(p.summary.as_deref(), Some("One thing to look at today."));
        assert_eq!(p.action, PaneAction::None);
        let a = &p.alerts[0];
        assert_eq!(
            (a.when.as_str(), a.engine, a.tag.as_str()),
            ("11-01 13:05", "Antivirus", "threat")
        );
        assert_eq!(a.colour, Level::Alert.colour());
        assert_eq!(a.explanation, "A downloaded file matched a known trojan.");
        assert_eq!(
            (p.alerts[1].engine, p.alerts[1].tag.as_str()),
            ("Threat detection", "level 8")
        );
        assert_eq!(p.alerts[2].engine, "File integrity");
        assert_eq!(p.engines[0].detail, "Connected, last check-in 11-01 13:00");
        assert_eq!(p.engines[1].detail, "1 alert recently");
        assert_eq!(p.engines[2].state, "on");
        assert_eq!(
            p.agents,
            vec!["laptop: never connected".to_string()],
            "this computer is not listed twice"
        );
    }

    #[test]
    fn engine_lines_from_the_daemon_win() {
        let w = warden(
            r#"{"id":"threat","unlocked":true,"status":{"level":"clear"},
            "report":{"engines":[{"id":"clamav","name":"Antivirus","state":"on","detail":"Last sweep 02:00, nothing found"}]}}"#,
        );
        let p = ThreatPane::from_warden(&w);
        assert_eq!(
            p.engines,
            vec![EngineLine {
                name: "Antivirus".into(),
                state: "on".into(),
                detail: "Last sweep 02:00, nothing found".into()
            }]
        );
    }

    #[test]
    fn a_level_from_a_newer_daemon_is_shown_not_refused() {
        let w = warden(r#"{"id":"threat","unlocked":true,"status":{"level":"quarantined"}}"#);
        assert_eq!(ThreatPane::from_warden(&w).level, Level::Unknown);
    }

    #[test]
    fn the_list_parses() {
        let l: WardenList = serde_json::from_str(
            r#"{"paid":[],"wardens":[{"id":"ram","icon":null,"access":"free","unlocked":true,"enabled":true,
            "status":{"level":"clear","headline":"Memory is comfortable","detail":"","checked_at":1}},
            {"id":"threat","icon":"/wardens/threat/icon.svg","access":"pro","unlocked":false,"status":{"level":"locked"}}]}"#,
        )
        .unwrap();
        assert_eq!(l.wardens.len(), 2);
        assert!(l.wardens[0].icon.is_none());
    }

    #[test]
    fn a_pixel_svg_becomes_a_sprite() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 4 2" width="96" height="96">
            <title>t</title>
            <rect x="1" y="0" width="2" height="1" fill="#ff2b3a"/>
            <rect x="0" y="1" width="1" height="1" fill="#0d0b0e"/>
        </svg>"##;
        let s = Sprite::from_svg(svg).unwrap();
        assert_eq!((s.width, s.height), (4, 2));
        assert_eq!(
            &s.rgba[0..4],
            &[0, 0, 0, 0],
            "unpainted pixels are transparent"
        );
        assert_eq!(&s.rgba[4..8], &[0xff, 0x2b, 0x3a, 255]);
        assert_eq!(&s.rgba[16..20], &[0x0d, 0x0b, 0x0e, 255]);
        let big = s.scaled(3);
        assert_eq!((big.width, big.height), (12, 6));
        assert_eq!(&big.rgba[(3 * 4)..(3 * 4 + 4)], &[0xff, 0x2b, 0x3a, 255]);
        assert!(Sprite::from_svg("<html>not an icon</html>").is_none());
    }

    /// The real cyborg, once the wardens backend has added it to the tree.
    #[test]
    fn the_threatwarden_icon_is_a_48px_sprite() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../web/assets/wardens/threat.svg"
        );
        let Ok(svg) = std::fs::read_to_string(path) else {
            return;
        };
        let s = Sprite::from_svg(&svg).expect("the icon parses");
        assert_eq!((s.width, s.height), (48, 48));
        let painted = s.rgba.chunks(4).filter(|p| p[3] == 255).count();
        assert!(painted > 600, "only {painted} pixels drawn");
    }
}
