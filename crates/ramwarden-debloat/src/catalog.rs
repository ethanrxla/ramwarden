//! The shipped profiles, and the list of things no profile may ever touch.
//!
//! Profiles are compiled into the binary rather than read from disk: the
//! SYSTEM service applies tweaks *by id from this catalog*, so a request can
//! never name a setting that is not in a reviewed, shipped profile.

use ramwarden_platform::{Error, Result};
use serde::Deserialize;

use crate::registry::{Data, KeyPath, RegistryTweak};
use crate::service::{ServiceTweak, Start};
use crate::startup::StartupTweak;
use crate::task::TaskTweak;
use crate::tweak::Tweak;

const PROFILES: &[(&str, &str)] = &[
    ("privacy", include_str!("../profiles/privacy.toml")),
    ("background", include_str!("../profiles/background.toml")),
];

/// Services that keep the machine secure, updated, fast or able to run games.
/// Absolute: a profile naming one fails to load.
const FORBIDDEN_SERVICES: &[&str] = &[
    "windefend", "wdnissvc", "sense", "securityhealthservice", "mpssvc", "bfe", "wscsvc",
    "wuauserv", "usosvc", "waasmedicsvc", "bits", "trustedinstaller", "cryptsvc",
    "sysmain", "wsearch", "eventlog", "rpcss", "dcomlaunch", "audiosrv", "nvdisplay.containerlocalsystem",
    "vgc", "vgk", "easyanticheat", "easyanticheat_eos", "beservice", "faceit",
];

/// Registry subtrees no profile may write under.
const FORBIDDEN_KEYS: &[&str] = &[
    r"software\policies\microsoft\windows defender",
    r"software\microsoft\windows defender",
    r"software\policies\microsoft\windows\windowsupdate",
    r"system\currentcontrolset\services",
    r"system\currentcontrolset\control\deviceguard",
    r"software\microsoft\windows nt\currentversion\winlogon",
];

/// Startup entries no profile may disable.
const FORBIDDEN_STARTUP: &[&str] = &["riot vanguard", "securityhealth", "windowsdefender"];

#[derive(Debug, Deserialize)]
struct RawProfile {
    name: String,
    description: String,
    #[serde(rename = "tweak", default)]
    tweaks: Vec<Spec>,
}

/// One tweak as written in a profile.
#[derive(Clone, Debug, Deserialize)]
pub struct Spec {
    pub id: String,
    pub description: String,
    pub kind: String,
    #[serde(default)]
    pub optional: bool,
    pub service: Option<String>,
    pub start: Option<String>,
    pub key: Option<String>,
    pub value: Option<String>,
    pub dword: Option<u32>,
    pub string: Option<String>,
    pub task: Option<String>,
    pub run: Option<String>,
}

impl Spec {
    /// Refuse anything on the forbidden lists.
    pub fn check(&self) -> Result<()> {
        let refuse = |what: &str| Err(Error::denied(&self.id, format!("{what} is never touched")));
        if let Some(s) = &self.service
            && FORBIDDEN_SERVICES.contains(&s.to_lowercase().as_str())
        {
            return refuse(&format!("service {s}"));
        }
        if let Some(k) = &self.key {
            let lower = KeyPath::parse(k)?.subkey.to_lowercase();
            if let Some(f) = FORBIDDEN_KEYS.iter().find(|f| lower.starts_with(*f)) {
                return refuse(&format!("registry {f}"));
            }
        }
        if let Some(r) = &self.run
            && FORBIDDEN_STARTUP.contains(&r.to_lowercase().as_str())
        {
            return refuse(&format!("startup entry {r}"));
        }
        if self.kind == "service" && self.start.as_deref().and_then(Start::parse).is_none() {
            return Err(Error::os(format!("{}: start must be disabled or manual", self.id)));
        }
        Ok(())
    }

    fn need<'a>(&self, field: &'a Option<String>, name: &str) -> Result<&'a str> {
        field
            .as_deref()
            .ok_or_else(|| Error::os(format!("{}: {} tweak needs `{name}`", self.id, self.kind)))
    }

    /// Build the tweak this spec describes, after the forbidden-list check.
    pub fn build(&self) -> Result<Box<dyn Tweak>> {
        self.check()?;
        let (id, description, optional) = (self.id.clone(), self.description.clone(), self.optional);
        Ok(match self.kind.as_str() {
            "service" => Box::new(ServiceTweak {
                id,
                description,
                optional,
                service: self.need(&self.service, "service")?.to_string(),
                start: Start::parse(self.need(&self.start, "start")?).expect("checked"),
            }),
            "registry" => Box::new(RegistryTweak {
                id,
                description,
                optional,
                key: KeyPath::parse(self.need(&self.key, "key")?)?,
                value: self.need(&self.value, "value")?.to_string(),
                data: match (self.dword, &self.string) {
                    (Some(d), None) => Data::Dword(d),
                    (None, Some(s)) => Data::String(s.clone()),
                    _ => return Err(Error::os(format!("{}: exactly one of dword/string", self.id))),
                },
            }),
            "task" => Box::new(TaskTweak {
                id,
                description,
                optional,
                task: self.need(&self.task, "task")?.to_string(),
            }),
            "startup" => Box::new(StartupTweak {
                id,
                description,
                optional,
                run: self.need(&self.run, "run")?.to_string(),
            }),
            other => return Err(Error::os(format!("{}: unknown kind {other}", self.id))),
        })
    }
}

/// A named group of tweaks.
pub struct Profile {
    pub name: String,
    pub description: String,
    pub tweaks: Vec<Box<dyn Tweak>>,
}

/// Every shipped profile, built and checked.
pub fn profiles() -> Result<Vec<Profile>> {
    PROFILES
        .iter()
        .map(|(_, text)| {
            let raw: RawProfile = toml::from_str(text).map_err(|e| Error::os(format!("profile: {e}")))?;
            Ok(Profile {
                name: raw.name,
                description: raw.description,
                tweaks: raw.tweaks.iter().map(Spec::build).collect::<Result<_>>()?,
            })
        })
        .collect()
}

/// One tweak by id, from any shipped profile.
pub fn tweak(id: &str) -> Result<Box<dyn Tweak>> {
    profiles()?
        .into_iter()
        .flat_map(|p| p.tweaks)
        .find(|t| t.id() == id)
        .ok_or_else(|| Error::missing(format!("tweak {id} (not in any shipped profile)")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn every_shipped_profile_loads_and_passes_the_guard() {
        let ps = profiles().unwrap();
        assert_eq!(ps.len(), 2);
        let mut ids = HashSet::new();
        for p in &ps {
            assert!(!p.tweaks.is_empty(), "{}", p.name);
            for t in &p.tweaks {
                assert!(ids.insert(t.id().to_string()), "duplicate id {}", t.id());
                assert!(t.id().starts_with(&format!("{}.", p.name)), "{} not namespaced", t.id());
            }
        }
    }

    fn spec(kind: &str) -> Spec {
        Spec {
            id: "x.y".into(),
            description: "d".into(),
            kind: kind.into(),
            optional: false,
            service: None,
            start: None,
            key: None,
            value: None,
            dword: None,
            string: None,
            task: None,
            run: None,
        }
    }

    #[test]
    fn defender_update_and_anti_cheat_can_never_be_named() {
        for svc in ["WinDefend", "wuauserv", "SysMain", "WSearch", "vgc", "EasyAntiCheat_EOS"] {
            let s = Spec { service: Some(svc.into()), start: Some("disabled".into()), ..spec("service") };
            assert!(s.build().unwrap_err().is_denied(), "{svc}");
        }
        let s = Spec {
            key: Some(r"HKLM\SOFTWARE\Policies\Microsoft\Windows Defender\Real-Time Protection".into()),
            value: Some("DisableRealtimeMonitoring".into()),
            dword: Some(1),
            ..spec("registry")
        };
        assert!(s.build().unwrap_err().is_denied());
        let s = Spec { run: Some("Riot Vanguard".into()), ..spec("startup") };
        assert!(s.build().unwrap_err().is_denied());
    }

    #[test]
    fn a_service_can_only_be_made_to_start_less() {
        let s = Spec { service: Some("DiagTrack".into()), start: Some("auto".into()), ..spec("service") };
        assert!(s.build().is_err());
    }

    #[test]
    fn unknown_ids_are_refused() {
        assert!(tweak("privacy.diagtrack").is_ok());
        assert!(tweak("privacy.disable-defender").unwrap_err().is_missing());
    }
}
