//! The wardens: which ones exist, which the user's plan unlocks, and how each
//! one's findings become a single level the user can read at a glance.
//!
//! A warden is a small agent that watches one thing. The RAM warden drives the
//! reclaim ladder; the Wazuh warden reports what the machine's Wazuh security
//! agent has seen. Only the RAM warden ever acts — the others observe — so the
//! rule that suspend and kill only target watchlisted names stays the whole
//! story about what RamWarden may touch. See `docs/specs/wardens.md`.
//!
//! This module is policy only. Talking to Wazuh lives in `ramwarden-wazuh`;
//! polling and the HTTP routes live in `ramwarden-daemon::wardens`.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::ladder::Rung;

/// What a warden needs the user to have.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Access {
    /// Everyone has it.
    Free,
    /// Paid: unlocked by a current pass for this warden (alone or in a bundle).
    Pro,
}

/// A warden's fixed description. Its `id` is also its add-on SKU, its config
/// section (`[wardens.<id>]`) and its icon's file name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct WardenInfo {
    pub id: &'static str,
    pub name: &'static str,
    /// One line on what it watches, for the UI's warden list.
    pub role: &'static str,
    pub access: Access,
}

pub const RAM: WardenInfo = WardenInfo {
    id: "ram",
    name: "RAM Warden",
    role: "Watches memory pressure, tries reversible fixes first, and protects what you're using.",
    access: Access::Free,
};

pub const WAZUH: WardenInfo = WardenInfo {
    id: "wazuh",
    name: "Wazuh Warden",
    role: "Watches this computer's Wazuh security agent and tells you when it sees a threat.",
    access: Access::Pro,
};

/// Every warden, in the order the UI shows them. Adding a warden starts here.
pub const CATALOGUE: &[WardenInfo] = &[RAM, WAZUH];

pub fn find(id: &str) -> Option<&'static WardenInfo> {
    CATALOGUE.iter().find(|w| w.id == id)
}

/// What the user has paid for: one expiry per paid warden, as the account
/// server issued it (`GET /v1/entitlement`, written to `entitlement.json`).
///
/// ```json
/// {"v":1, "account":"…", "device":"…", "issued_at":1793500000,
///  "wardens":{"ram":1796000000, "wazuh":1798000000}, "sig":"<base64 ed25519>"}
/// ```
///
/// Passes are sold per warden, each with its own expiry, so a RAM pass never
/// unlocks Wazuh and a bundle is simply both keys. `"ram"` is the hosted Jev
/// tier; the local RAM warden itself is free and ignores it. A warden missing
/// from the map stays locked.
///
/// The server signs the canonical JSON without `sig` with Ed25519. Checking
/// that signature belongs in [`Entitlement::load`]; until the key is compiled
/// in, an unsigned record is accepted.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Entitlement {
    pub v: u32,
    pub account: Option<String>,
    pub device: Option<String>,
    pub issued_at: u64,
    /// Warden id → Unix seconds it is paid until.
    pub wardens: BTreeMap<String, u64>,
    pub sig: Option<String>,
}

impl Entitlement {
    /// Whether `w` is unlocked at `now` (Unix seconds).
    pub fn allows(&self, w: &WardenInfo, now: u64) -> bool {
        match w.access {
            Access::Free => true,
            Access::Pro => self.wardens.get(w.id).is_some_and(|&until| until > now),
        }
    }

    /// The ids of every paid warden current at `now`.
    pub fn paid_at(&self, now: u64) -> Vec<&str> {
        self.wardens.iter().filter(|&(_, &u)| u > now).map(|(id, _)| id.as_str()).collect()
    }

    /// Read `entitlement.json`. A missing or malformed file unlocks nothing:
    /// a broken record must never lock the user out of the free wardens, and
    /// must never unlock a paid one either.
    pub fn load(path: &Path) -> Entitlement {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                tracing::warn!("{}: {e}; treating as no paid wardens", path.display());
                Entitlement::default()
            }),
            Err(_) => Entitlement::default(),
        }
    }
}

/// How much a warden needs the user right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    /// Nothing needs the user.
    Clear,
    /// Worth a look; nothing urgent.
    Watch,
    /// The user should act now.
    Alert,
    /// Unlocked, but not configured or not reachable.
    Off,
    /// Needs a plan the user does not have.
    Locked,
}

/// A warden's state in one line, plus a sentence of detail.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    pub level: Level,
    pub headline: String,
    pub detail: String,
    /// Unix seconds, or 0 if never checked.
    pub checked_at: u64,
}

impl Status {
    pub fn new(level: Level, headline: impl Into<String>, detail: impl Into<String>, checked_at: u64) -> Self {
        Status { level, headline: headline.into(), detail: detail.into(), checked_at }
    }

    pub fn locked(w: &WardenInfo) -> Self {
        Status::new(
            Level::Locked,
            format!("{} is a Pro warden", w.name),
            "Get a pass for this warden, or a bundle that includes it, to turn it on.",
            0,
        )
    }

    pub fn off(headline: impl Into<String>, detail: impl Into<String>, checked_at: u64) -> Self {
        Status::new(Level::Off, headline, detail, checked_at)
    }
}

/// Thresholds for turning Wazuh findings into a [`Level`].
#[derive(Clone, Copy, Debug)]
pub struct ThreatThresholds {
    /// Alerts below this rule level are ignored.
    pub min_level: u8,
    /// Alerts at or above this rule level are [`Level::Alert`]. Twelve is where
    /// Wazuh's rule classification starts calling events "high importance".
    pub alert_level: u8,
}

impl Default for ThreatThresholds {
    fn default() -> Self {
        ThreatThresholds { min_level: 7, alert_level: 12 }
    }
}

/// Whether the security agent can report at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentHealth {
    /// Connected to its manager.
    Connected,
    /// Installed, but not connected (or the manager says it is not active).
    Disconnected,
    /// Installed; its state could not be read and the manager was not asked.
    Unknown,
    /// No agent on this machine.
    Missing,
}

/// The Wazuh warden's verdict, from the agent's health and the rule levels of
/// the alerts seen in the lookback window.
///
/// Silence from an agent that cannot report is not safety: a disconnected agent
/// is at least [`Level::Watch`], whatever the alert count says.
pub fn assess_threats(health: AgentHealth, alert_levels: &[u8], t: ThreatThresholds, now: u64) -> Status {
    let high = alert_levels.iter().filter(|&&l| l >= t.alert_level).count();
    let notable = alert_levels.iter().filter(|&&l| l >= t.min_level).count();
    let plural = |n: usize| if n == 1 { "" } else { "s" };

    if health == AgentHealth::Missing {
        return Status::off(
            "No Wazuh agent on this computer",
            "Install the Wazuh agent and enrol it with your manager to turn this warden on.",
            now,
        );
    }
    if high > 0 {
        return Status::new(
            Level::Alert,
            format!("{high} high-severity security alert{}", plural(high)),
            "Wazuh flagged events at level 12 or above. Review them below.",
            now,
        );
    }
    if health == AgentHealth::Disconnected {
        return Status::new(
            Level::Watch,
            "Wazuh agent is not connected",
            "The agent cannot report threats until it reconnects to its manager.",
            now,
        );
    }
    if notable > 0 {
        return Status::new(
            Level::Watch,
            format!("{notable} security alert{} worth a look", plural(notable)),
            format!("Wazuh flagged events at level {} or above.", t.min_level),
            now,
        );
    }
    let detail = match health {
        AgentHealth::Unknown => "No alerts found. The agent's own state file was not readable.",
        _ => "The agent is connected and has raised nothing notable.",
    };
    Status::new(Level::Clear, "No threats seen", detail, now)
}

/// The RAM warden's level from the ladder's current rung: reversible rungs are
/// worth a look, suspend and kill mean RamWarden is about to take something
/// from the user.
pub fn ram_status(rung: Rung, used_percent: f64, now: u64) -> Status {
    let used = format!("{used_percent:.0}% of memory in use.");
    match rung {
        Rung::Release | Rung::Hold => Status::new(Level::Clear, "Memory is comfortable", used, now),
        Rung::Reclaim | Rung::PageOut | Rung::Tabs => Status::new(
            Level::Watch,
            "Memory is tight; reclaiming reversibly",
            format!("{used} Compressing and paging out idle memory first."),
            now,
        ),
        Rung::Suspend | Rung::Kill => Status::new(
            Level::Alert,
            "Memory is critical",
            format!("{used} Watchlisted apps may be paused or closed."),
            now,
        ),
    }
}

/// Unix seconds now.
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000;

    fn ent(paid: &[(&str, u64)]) -> Entitlement {
        Entitlement { wardens: paid.iter().map(|&(k, v)| (k.to_string(), v)).collect(), ..Default::default() }
    }

    #[test]
    fn no_passes_unlock_only_free_wardens() {
        let e = Entitlement::default();
        assert!(e.allows(&RAM, NOW));
        assert!(!e.allows(&WAZUH, NOW));
        assert!(e.paid_at(NOW).is_empty());
    }

    #[test]
    fn each_pass_unlocks_only_its_own_warden() {
        // A RAM (hosted Jev) pass must not unlock Wazuh.
        let e = ent(&[("ram", NOW + 60)]);
        assert!(!e.allows(&WAZUH, NOW));
        let e = ent(&[("wazuh", NOW + 60)]);
        assert!(e.allows(&WAZUH, NOW));
        let other = WardenInfo { id: "future", ..WAZUH };
        assert!(!e.allows(&other, NOW));
    }

    #[test]
    fn a_bundle_is_both_keys_with_their_own_expiries() {
        let e = ent(&[("ram", NOW + 10), ("wazuh", NOW + 1000)]);
        assert_eq!(e.paid_at(NOW), vec!["ram", "wazuh"]);
        assert_eq!(e.paid_at(NOW + 500), vec!["wazuh"]);
        assert!(e.allows(&WAZUH, NOW + 500));
    }

    #[test]
    fn an_expired_pass_is_locked_and_free_wardens_stay_free() {
        for until in [NOW, NOW - 1] {
            let e = ent(&[("wazuh", until)]);
            assert!(!e.allows(&WAZUH, NOW), "{until}");
            assert!(e.allows(&RAM, NOW));
        }
    }

    #[test]
    fn a_missing_or_malformed_file_unlocks_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("entitlement.json");
        assert_eq!(Entitlement::load(&p), Entitlement::default());
        std::fs::write(&p, "{not json").unwrap();
        assert_eq!(Entitlement::load(&p), Entitlement::default());
        std::fs::write(
            &p,
            format!(r#"{{"v":1,"account":"a","device":"d","issued_at":{NOW},"wardens":{{"wazuh":{}}},"sig":"x"}}"#, NOW + 5),
        )
        .unwrap();
        assert!(Entitlement::load(&p).allows(&WAZUH, NOW));
    }

    #[test]
    fn ids_are_unique_and_findable() {
        for w in CATALOGUE {
            assert_eq!(find(w.id), Some(w));
            assert_eq!(CATALOGUE.iter().filter(|x| x.id == w.id).count(), 1);
        }
        assert!(find("nope").is_none());
    }

    #[test]
    fn rule_levels_map_to_clear_watch_and_alert() {
        let t = ThreatThresholds::default();
        let c = AgentHealth::Connected;
        assert_eq!(assess_threats(c, &[], t, NOW).level, Level::Clear);
        assert_eq!(assess_threats(c, &[3, 6], t, NOW).level, Level::Clear);
        assert_eq!(assess_threats(c, &[7], t, NOW).level, Level::Watch);
        assert_eq!(assess_threats(c, &[11, 7], t, NOW).level, Level::Watch);
        let s = assess_threats(c, &[12, 3], t, NOW);
        assert_eq!(s.level, Level::Alert);
        assert_eq!(s.headline, "1 high-severity security alert");
        assert_eq!(assess_threats(c, &[15, 13], t, NOW).headline, "2 high-severity security alerts");
    }

    #[test]
    fn a_disconnected_agent_is_never_clear() {
        let t = ThreatThresholds::default();
        let s = assess_threats(AgentHealth::Disconnected, &[], t, NOW);
        assert_eq!(s.level, Level::Watch);
        assert!(s.headline.contains("not connected"));
        // A high alert still outranks the connection problem.
        assert_eq!(assess_threats(AgentHealth::Disconnected, &[14], t, NOW).level, Level::Alert);
    }

    #[test]
    fn ram_levels_follow_the_ladder() {
        assert_eq!(ram_status(Rung::Hold, 40.0, NOW).level, Level::Clear);
        assert_eq!(ram_status(Rung::Release, 40.0, NOW).level, Level::Clear);
        assert_eq!(ram_status(Rung::PageOut, 80.0, NOW).level, Level::Watch);
        assert_eq!(ram_status(Rung::Kill, 95.0, NOW).level, Level::Alert);
        assert!(ram_status(Rung::Hold, 40.4, NOW).detail.starts_with("40%"));
    }

    #[test]
    fn no_agent_is_off_not_clear() {
        let s = assess_threats(AgentHealth::Missing, &[], ThreatThresholds::default(), NOW);
        assert_eq!(s.level, Level::Off);
    }
}
