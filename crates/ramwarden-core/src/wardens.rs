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
    /// The account tier a paid warden also needs (`tier` in the entitlement),
    /// on top of its own expiry. `None` when any tier will do.
    pub tier: Option<&'static str>,
    /// What a pass costs, for the UI's "Get a pass" line. `None` when free.
    pub price: Option<&'static str>,
    /// Where "Get a pass" opens. `None` when free.
    pub upgrade_url: Option<&'static str>,
}

/// The pricing page. Prices live on the site and in the checkout; the
/// `price` strings here are display copies, kept in step with them.
pub const PRICING_URL: &str = "https://ramwarden.app/#pricing";

pub const RAM: WardenInfo = WardenInfo {
    id: "ram",
    name: "RAM Warden",
    role: "Watches memory pressure, tries reversible fixes first, and protects what you're using.",
    access: Access::Free,
    tier: None,
    price: None,
    upgrade_url: None,
};

pub const THREAT: WardenInfo = WardenInfo {
    id: "threat",
    name: "ThreatWarden",
    role: "Watches for attacks, malware and intrusions, explains what it finds, and asks before acting.",
    access: Access::Pro,
    tier: Some("pro"),
    price: Some("In RamWarden Pro, from $5 a month"),
    upgrade_url: Some(PRICING_URL),
};

/// Every warden, in the order the UI shows them. Adding a warden starts here.
pub const CATALOGUE: &[WardenInfo] = &[RAM, THREAT];

pub fn find(id: &str) -> Option<&'static WardenInfo> {
    CATALOGUE.iter().find(|w| w.id == id)
}

/// What the user has paid for: the account's tier and status, and one expiry
/// per paid warden, as the account server issued it (`GET /v1/entitlement`,
/// written to `entitlement.json`).
///
/// ```json
/// {"v":2, "account":"…", "device":"…", "issued_at":1793500000,
///  "tier":"pro", "status":"trial",
///  "wardens":{"ram":1796000000, "threat":1798000000}, "sig":"<base64 ed25519>"}
/// ```
///
/// Each warden has its own expiry, so the `ram` plan never unlocks
/// ThreatWarden. ThreatWarden also needs `tier = "pro"`; `tier` can only
/// narrow access, never widen it, and `status` is for display only. `"ram"` is the hosted Jev
/// tier; the local RAM warden itself is free and ignores it. A warden missing
/// from the map stays locked.
///
/// The file is on the user's own disk, so it is signed: the server signs
/// [`Entitlement::signed_text`] with Ed25519, and [`Entitlement::load`]
/// checks it against [`PUBLIC_KEY`]. An edited, unsigned or foreign record
/// loads as "nothing unlocked". Format and reference signer:
/// `ramwarden-api/src/entitlement.rs` and `docs/specs/hosted-api.md`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Entitlement {
    pub v: u32,
    pub account: Option<String>,
    pub device: Option<String>,
    pub issued_at: u64,
    /// `ram`, `pro` or `none`.
    pub tier: Option<String>,
    /// `trial`, `active`, `grace`, `cancelled`, `lapsed` or `none`. Display only.
    pub status: Option<String>,
    /// Warden id → Unix seconds it is paid until.
    pub wardens: BTreeMap<String, u64>,
    pub sig: Option<String>,
}

/// The server's entitlement-signing public key (base64 of 32 bytes), compiled
/// in from `RAMWARDEN_ENTITLEMENT_PUBLIC_KEY` at build time. Compile-time only:
/// a key read at run time would let anyone sign their own passes. A build
/// without it unlocks no paid warden.
pub const PUBLIC_KEY: Option<&str> = option_env!("RAMWARDEN_ENTITLEMENT_PUBLIC_KEY");

/// The domain separator that starts every signed text.
const DOMAIN: &str = "ramwarden-entitlement-v2";

impl Entitlement {
    /// The exact bytes the server signs: one item per line, wardens sorted by
    /// id, no trailing newline. Built from the parsed fields, not the JSON, so
    /// key order and number formatting cannot break a signature.
    pub fn signed_text(&self) -> String {
        let mut s = format!(
            "{DOMAIN}\naccount={}\ndevice={}\nissued_at={}\ntier={}\nstatus={}",
            self.account.as_deref().unwrap_or_default(),
            self.device.as_deref().unwrap_or_default(),
            self.issued_at,
            self.tier.as_deref().unwrap_or("none"),
            self.status.as_deref().unwrap_or("none"),
        );
        for (w, t) in &self.wardens {
            s.push_str(&format!("\nwarden:{w}={t}"));
        }
        s
    }

    /// Whether this is a v2 record signed by `public_key_b64`.
    pub fn verify(&self, public_key_b64: &str) -> bool {
        use base64::Engine;
        use base64::engine::general_purpose::STANDARD as B64;
        use ed25519_dalek::{Signature, Verifier, VerifyingKey};
        let Some(key) = B64
            .decode(public_key_b64.trim())
            .ok()
            .and_then(|b| <[u8; 32]>::try_from(b).ok())
            .and_then(|b| VerifyingKey::from_bytes(&b).ok())
        else {
            return false;
        };
        let Some(sig) = self
            .sig
            .as_deref()
            .and_then(|s| B64.decode(s.trim()).ok())
            .and_then(|b| <[u8; 64]>::try_from(b).ok())
        else {
            return false;
        };
        self.v == 2 && key.verify(self.signed_text().as_bytes(), &Signature::from_bytes(&sig)).is_ok()
    }

    /// Whether `w` is unlocked at `now` (Unix seconds).
    pub fn allows(&self, w: &WardenInfo, now: u64) -> bool {
        match w.access {
            Access::Free => true,
            Access::Pro => {
                w.tier.is_none_or(|t| self.tier.as_deref() == Some(t))
                    && self.wardens.get(w.id).is_some_and(|&until| until > now)
            }
        }
    }

    /// The ids of every paid warden unlocked at `now`.
    pub fn paid_at(&self, now: u64) -> Vec<&str> {
        self.wardens
            .iter()
            .filter(|&(id, &u)| u > now && find(id).is_none_or(|w| self.allows(w, now)))
            .map(|(id, _)| id.as_str())
            .collect()
    }

    /// Read `entitlement.json` and check it against the compiled-in key.
    pub fn load(path: &Path) -> Entitlement {
        Entitlement::load_verified(path, PUBLIC_KEY)
    }

    /// Read `entitlement.json` and check its signature against `key`. A
    /// missing, malformed or badly signed file, or no key, unlocks nothing: a
    /// broken record must never lock the user out of the free wardens, and
    /// must never unlock a paid one either.
    pub fn load_verified(path: &Path, key: Option<&str>) -> Entitlement {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Entitlement::default();
        };
        let e: Entitlement = match serde_json::from_str(&text) {
            Ok(e) => e,
            Err(err) => {
                tracing::warn!("{}: {err}; treating as no paid wardens", path.display());
                return Entitlement::default();
            }
        };
        match key {
            Some(k) if e.verify(k) => e,
            Some(_) => {
                tracing::warn!("{}: signature does not verify; treating as no paid wardens", path.display());
                Entitlement::default()
            }
            None => {
                tracing::warn!("this build has no entitlement key; paid wardens stay locked");
                Entitlement::default()
            }
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
    /// Installing what it needs (the Wazuh agent).
    Setup,
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

/// The rule level an alert counts at, after the hosted AI triage. A `threat`
/// verdict on an alert the user would see anyway lifts it to the alert level.
/// A model can make an alert louder, never quieter: `benign` changes nothing,
/// because Wazuh's own rule level is the floor.
pub fn effective_level(rule_level: u8, verdict: Option<&str>, t: ThreatThresholds) -> u8 {
    if verdict == Some("threat") && rule_level >= t.min_level {
        rule_level.max(t.alert_level)
    } else {
        rule_level
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

/// ThreatWarden's Wazuh verdict, from the agent's health and the rule levels of
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
    if health == AgentHealth::Unknown {
        return Status::new(Level::Watch, "Agent connection not confirmed",
            "The local state is unreadable and the manager has not confirmed this agent is connected.", now);
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
        Entitlement {
            tier: Some("pro".into()),
            wardens: paid.iter().map(|&(k, v)| (k.to_string(), v)).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn threatwarden_needs_the_pro_tier_as_well_as_its_expiry() {
        let mut e = ent(&[("ram", NOW + 10), ("threat", NOW + 10)]);
        assert!(e.allows(&THREAT, NOW));
        for tier in [Some("ram"), Some("none"), None] {
            e.tier = tier.map(str::to_string);
            assert!(!e.allows(&THREAT, NOW), "{tier:?}");
            assert!(e.allows(&RAM, NOW));
            assert_eq!(e.paid_at(NOW), vec!["ram"]);
        }
    }

    #[test]
    fn a_v1_record_is_refused_even_when_signed() {
        use base64::Engine;
        use base64::engine::general_purpose::STANDARD as B64;
        use ed25519_dalek::{Signer, SigningKey};
        let (mut e, key) = signed(&[("threat", NOW + 60)]);
        assert!(e.verify(&key));
        e.v = 1;
        e.sig = Some(B64.encode(SigningKey::from_bytes(&TEST_SEED).sign(e.signed_text().as_bytes()).to_bytes()));
        assert!(!e.verify(&key));
    }

    #[test]
    fn no_passes_unlock_only_free_wardens() {
        let e = Entitlement::default();
        assert!(e.allows(&RAM, NOW));
        assert!(!e.allows(&THREAT, NOW));
        assert!(e.paid_at(NOW).is_empty());
    }

    #[test]
    fn each_pass_unlocks_only_its_own_warden() {
        // A RAM (hosted Jev) pass must not unlock Wazuh.
        let e = ent(&[("ram", NOW + 60)]);
        assert!(!e.allows(&THREAT, NOW));
        let e = ent(&[("threat", NOW + 60)]);
        assert!(e.allows(&THREAT, NOW));
        let other = WardenInfo { id: "future", ..THREAT };
        assert!(!e.allows(&other, NOW));
    }

    #[test]
    fn a_bundle_is_both_keys_with_their_own_expiries() {
        let e = ent(&[("ram", NOW + 10), ("threat", NOW + 1000)]);
        assert_eq!(e.paid_at(NOW), vec!["ram", "threat"]);
        assert_eq!(e.paid_at(NOW + 500), vec!["threat"]);
        assert!(e.allows(&THREAT, NOW + 500));
    }

    #[test]
    fn an_expired_pass_is_locked_and_free_wardens_stay_free() {
        for until in [NOW, NOW - 1] {
            let e = ent(&[("threat", until)]);
            assert!(!e.allows(&THREAT, NOW), "{until}");
            assert!(e.allows(&RAM, NOW));
        }
    }

    /// The checkout thread's fixed test seed (`ramwarden-api` entitlement
    /// tests), so both sides check the same vector.
    const TEST_SEED: [u8; 32] = [1; 32];

    fn signed(wardens: &[(&str, u64)]) -> (Entitlement, String) {
        use base64::Engine;
        use base64::engine::general_purpose::STANDARD as B64;
        use ed25519_dalek::{Signer, SigningKey};
        let key = SigningKey::from_bytes(&TEST_SEED);
        let mut e = ent(wardens);
        e.v = 2;
        e.tier = Some("pro".into());
        e.status = Some("trial".into());
        e.account = Some("acct1".into());
        e.device = Some("dev1".into());
        e.issued_at = NOW;
        e.sig = Some(B64.encode(key.sign(e.signed_text().as_bytes()).to_bytes()));
        (e, B64.encode(key.verifying_key().as_bytes()))
    }

    #[test]
    fn the_signed_text_matches_the_servers_format() {
        let (e, _) = signed(&[("threat", 7), ("ram", 5)]);
        assert_eq!(
            e.signed_text(),
            format!("ramwarden-entitlement-v2\naccount=acct1\ndevice=dev1\nissued_at={NOW}\ntier=pro\nstatus=trial\nwarden:ram=5\nwarden:threat=7")
        );
    }

    #[test]
    fn only_a_correctly_signed_record_unlocks_anything() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("entitlement.json");
        let (e, key) = signed(&[("threat", NOW + 60)]);
        std::fs::write(&p, serde_json::to_string(&e).unwrap()).unwrap();
        assert!(Entitlement::load_verified(&p, Some(&key)).allows(&THREAT, NOW));
        assert!(!Entitlement::load_verified(&p, None).allows(&THREAT, NOW), "no key, no unlock");

        // Extending your own pass by editing the file breaks the signature.
        let mut forged = e.clone();
        forged.wardens.insert("threat".into(), NOW + 999_999);
        std::fs::write(&p, serde_json::to_string(&forged).unwrap()).unwrap();
        assert_eq!(Entitlement::load_verified(&p, Some(&key)), Entitlement::default());

        // Unsigned, garbage, missing.
        std::fs::write(&p, format!(r#"{{"v":2,"tier":"pro","wardens":{{"threat":{}}}}}"#, NOW + 5)).unwrap();
        assert_eq!(Entitlement::load_verified(&p, Some(&key)), Entitlement::default());
        std::fs::write(&p, "{not json").unwrap();
        assert_eq!(Entitlement::load_verified(&p, Some(&key)), Entitlement::default());
        assert_eq!(Entitlement::load_verified(&dir.path().join("nope"), Some(&key)), Entitlement::default());
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
    fn triage_can_raise_an_alert_but_never_lower_one() {
        let t = ThreatThresholds::default();
        assert_eq!(effective_level(8, Some("threat"), t), 12);
        assert_eq!(effective_level(14, Some("threat"), t), 14);
        assert_eq!(effective_level(14, Some("benign"), t), 14, "never quieter");
        assert_eq!(effective_level(3, Some("threat"), t), 3, "below the floor stays hidden");
        assert_eq!(effective_level(9, None, t), 9);
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
    fn unknown_connection_is_watch_not_clear() {
        let s = assess_threats(AgentHealth::Unknown, &[], ThreatThresholds::default(), NOW);
        assert_eq!(s.level, Level::Watch);
    }

    #[test]
    fn no_agent_is_off_not_clear() {
        let s = assess_threats(AgentHealth::Missing, &[], ThreatThresholds::default(), NOW);
        assert_eq!(s.level, Level::Off);
    }
}
