//! Applying, reverting and reporting tweaks — always snapshot first.

use std::path::PathBuf;

use ramwarden_nt::pipe::{self, Request, Response};
use ramwarden_platform::{Error, Result};
use serde::{Deserialize, Serialize};

use crate::catalog::{self, Profile};
use crate::journal::Journal;
use crate::tweak::{Scope, State, Tweak};

/// What applying one tweak did.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Applied {
    /// Snapshotted, then changed.
    Changed,
    /// Already in the target state; nothing recorded, so revert leaves it.
    AlreadySet,
    /// Was applied before, Windows put it back (an update, usually), and it
    /// has been re-applied against the original snapshot.
    Reapplied,
    /// The target does not exist on this machine.
    NotPresent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reverted {
    Restored,
    /// RamWarden never changed it, so there is nothing to undo.
    NotApplied,
}

/// One tweak's full picture.
#[derive(Clone, Debug, Serialize)]
pub struct Status {
    pub id: String,
    pub description: String,
    pub target: String,
    pub scope: Scope,
    pub optional: bool,
    pub state: State,
    /// RamWarden holds a snapshot for it — i.e. RamWarden made this change.
    pub recorded: bool,
    /// Recorded as applied, but Windows has since reverted it.
    pub drifted: bool,
}

/// Applies tweaks of one scope against one journal.
pub struct Engine {
    journal: Journal,
}

impl Engine {
    pub fn new(journal: Journal) -> Self {
        Engine { journal }
    }

    pub fn apply(&self, t: &dyn Tweak) -> Result<Applied> {
        let recorded = self.journal.get(t.id())?.is_some();
        match t.detect()? {
            State::Missing => return Ok(Applied::NotPresent),
            State::Applied => return Ok(Applied::AlreadySet),
            State::Default => {}
        }
        if !recorded {
            // The snapshot is on disk before anything changes.
            self.journal.record(t.id(), &t.snapshot()?)?;
        }
        if let Err(e) = t.apply() {
            // Leave the machine as it was, and the journal with it.
            if !recorded && let Some(rec) = self.journal.get(t.id())? {
                let _ = t.revert(&rec.snapshot);
                let _ = self.journal.forget(t.id());
            }
            return Err(e);
        }
        Ok(if recorded { Applied::Reapplied } else { Applied::Changed })
    }

    pub fn revert(&self, t: &dyn Tweak) -> Result<Reverted> {
        let Some(rec) = self.journal.get(t.id())? else {
            return Ok(Reverted::NotApplied);
        };
        t.revert(&rec.snapshot)?;
        self.journal.forget(t.id())?;
        Ok(Reverted::Restored)
    }

    pub fn status(&self, t: &dyn Tweak) -> Status {
        let state = t.detect().unwrap_or(State::Default);
        let recorded = self.journal.get(t.id()).ok().flatten().is_some();
        Status {
            id: t.id().to_string(),
            description: t.description().to_string(),
            target: t.target(),
            scope: t.scope(),
            optional: t.optional(),
            state,
            recorded,
            drifted: recorded && state == State::Default,
        }
    }
}

/// Where machine-wide tweaks are carried out.
enum Machine {
    /// This process is elevated (or is the service): act directly.
    Local(Engine),
    /// Ask `ramwarden-service` over its pipe.
    Service,
}

/// The whole debloat surface for one caller.
///
/// User-scope tweaks always run in this process — they live in the caller's
/// own registry hive. Machine-scope tweaks run here when elevated, otherwise
/// in the SYSTEM service, which applies only ids from the shipped catalog.
pub struct Debloat {
    user: Option<Engine>,
    machine: Machine,
}

pub fn user_journal_path() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("ramwarden").join("debloat.db")
}

pub fn machine_journal_path() -> PathBuf {
    let base = std::env::var_os("ProgramData")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
    base.join("RamWarden").join("debloat.db")
}

/// One line of an apply/revert report.
#[derive(Clone, Debug, Serialize)]
pub struct Line {
    pub id: String,
    pub ok: bool,
    pub result: String,
}

impl Line {
    fn from<T: Serialize>(id: &str, r: Result<T>) -> Self {
        match r {
            Ok(v) => Line {
                id: id.into(),
                ok: true,
                result: serde_json::to_value(v)
                    .ok()
                    .and_then(|v| v.as_str().map(String::from))
                    .unwrap_or_default(),
            },
            Err(e) => Line {
                id: id.into(),
                ok: false,
                result: e.to_string(),
            },
        }
    }
}

impl Debloat {
    /// For the interactive user: the daemon or the CLI.
    pub fn for_user() -> Result<Self> {
        let machine = if ramwarden_nt::privilege::is_elevated() {
            Machine::Local(Engine::new(Journal::open(&machine_journal_path())?))
        } else {
            Machine::Service
        };
        Ok(Debloat {
            user: Some(Engine::new(Journal::open(&user_journal_path())?)),
            machine,
        })
    }

    /// For `ramwarden-service`: machine scope only.
    pub fn for_service() -> Result<Self> {
        Ok(Debloat {
            user: None,
            machine: Machine::Local(Engine::new(Journal::open(&machine_journal_path())?)),
        })
    }

    pub fn profiles() -> Result<Vec<Profile>> {
        catalog::profiles()
    }

    fn user_engine(&self) -> Result<&Engine> {
        self.user
            .as_ref()
            .ok_or_else(|| Error::denied("user tweak", "the service cannot reach the user's registry hive"))
    }

    /// A user-policy tweak bound to the user running this process.
    fn for_me(t: &dyn Tweak) -> Result<Box<dyn Tweak>> {
        let sid = ramwarden_nt::owner::current_user_sid().ok_or_else(|| Error::os("cannot read this user's SID"))?;
        t.for_user(&sid).ok_or_else(|| Error::os(format!("{} cannot be bound to a user", t.id())))
    }

    pub fn apply(&self, t: &dyn Tweak) -> Result<Applied> {
        match (t.scope(), &self.machine) {
            (Scope::UserPolicy, Machine::Local(e)) => e.apply(Self::for_me(t)?.as_ref()),
            (Scope::UserPolicy, Machine::Service) => via_service(Request::TweakApply { id: t.id().to_string() }),
            (Scope::User, _) => self.user_engine()?.apply(t),
            (Scope::Machine, Machine::Local(e)) => e.apply(t),
            (Scope::Machine, Machine::Service) => {
                via_service(Request::TweakApply { id: t.id().to_string() })
            }
        }
    }

    pub fn revert(&self, t: &dyn Tweak) -> Result<Reverted> {
        match (t.scope(), &self.machine) {
            (Scope::UserPolicy, Machine::Local(e)) => e.revert(Self::for_me(t)?.as_ref()),
            (Scope::UserPolicy, Machine::Service) => via_service(Request::TweakRevert { id: t.id().to_string() }),
            (Scope::User, _) => self.user_engine()?.revert(t),
            (Scope::Machine, Machine::Local(e)) => e.revert(t),
            (Scope::Machine, Machine::Service) => {
                via_service(Request::TweakRevert { id: t.id().to_string() })
            }
        }
    }

    pub fn status(&self, t: &dyn Tweak) -> Status {
        // A user-policy tweak's record lives in the machine journal under its
        // per-user id; report it under the profile's id.
        if t.scope() == Scope::UserPolicy
            && let Ok(mine) = Self::for_me(t)
        {
            let mut s = self.status(mine.as_ref());
            s.id = t.id().to_string();
            s.scope = Scope::UserPolicy;
            s.target = t.target();
            return s;
        }
        match (t.scope(), &self.machine) {
            (Scope::User, _) if self.user.is_some() => self.user.as_ref().unwrap().status(t),
            (Scope::Machine, Machine::Local(e)) => e.status(t),
            // Unelevated: read the service's journal directly (ProgramData is
            // world-readable), so status needs no round trip.
            _ => match Journal::open(&machine_journal_path()) {
                Ok(j) => Engine::new(j).status(t),
                Err(_) => Engine::new(Journal::in_memory().expect("in-memory journal")).status(t),
            },
        }
    }

    /// Every tweak in every profile.
    pub fn status_all(&self) -> Result<Vec<(String, Status)>> {
        Ok(catalog::profiles()?
            .into_iter()
            .flat_map(|p| {
                let name = p.name.clone();
                p.tweaks.into_iter().map(move |t| (name.clone(), t))
            })
            .map(|(p, t)| (p, self.status(t.as_ref())))
            .collect())
    }

    /// Apply a whole profile (non-optional tweaks) or one tweak id.
    pub fn apply_named(&self, name: &str, include_optional: bool) -> Result<Vec<Line>> {
        let tweaks = select(name, include_optional)?;
        Ok(tweaks.iter().map(|t| Line::from(t.id(), self.apply(t.as_ref()))).collect())
    }

    /// Revert a profile, one id, or `all`.
    pub fn revert_named(&self, name: &str) -> Result<Vec<Line>> {
        let tweaks = select(name, true)?;
        Ok(tweaks.iter().map(|t| Line::from(t.id(), self.revert(t.as_ref()))).collect())
    }
}

/// `all`, a profile name, or a single tweak id.
fn select(name: &str, include_optional: bool) -> Result<Vec<Box<dyn Tweak>>> {
    let profiles = catalog::profiles()?;
    if name == "all" {
        return Ok(profiles.into_iter().flat_map(|p| p.tweaks).collect());
    }
    if let Some(p) = profiles.into_iter().find(|p| p.name == name) {
        return Ok(p.tweaks.into_iter().filter(|t| include_optional || !t.optional()).collect());
    }
    Ok(vec![catalog::tweak(name)?])
}

fn via_service<T: serde::de::DeserializeOwned>(req: Request) -> Result<T> {
    match pipe::call(&req)? {
        Response::Ok { detail, .. } => serde_json::from_value(detail)
            .map_err(|e| Error::os(format!("unexpected service reply: {e}"))),
        Response::Error { message } => Err(Error::os(message)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{Data, KeyPath, RegistryTweak};

    fn scratch_tweak() -> RegistryTweak {
        RegistryTweak {
            id: "test.engine".into(),
            description: "test".into(),
            optional: false,
            key: KeyPath::parse(&format!(
                r"HKCU\Software\RamWardenTest\engine-{}-{}",
                std::process::id(),
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
            ))
            .unwrap(),
            value: "V".into(),
            data: Data::Dword(7),
        }
    }

    #[test]
    fn apply_snapshots_first_and_revert_restores() {
        let _guard = crate::REGISTRY_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let e = Engine::new(Journal::in_memory().unwrap());
        let t = scratch_tweak();
        assert_eq!(e.apply(&t).unwrap(), Applied::Changed);
        assert!(e.status(&t).recorded);
        assert_eq!(e.apply(&t).unwrap(), Applied::AlreadySet);
        assert_eq!(e.revert(&t).unwrap(), Reverted::Restored);
        assert_eq!(t.detect().unwrap(), State::Default);
        assert_eq!(e.revert(&t).unwrap(), Reverted::NotApplied);
    }

    /// Windows Update reverting a setting shows up as drift, and re-applying
    /// keeps the *original* snapshot so revert still goes all the way back.
    #[test]
    fn drift_is_reported_and_reapplied_against_the_original_snapshot() {
        let _guard = crate::REGISTRY_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let e = Engine::new(Journal::in_memory().unwrap());
        let t = scratch_tweak();
        e.apply(&t).unwrap();
        let snap = e.journal.get(t.id()).unwrap().unwrap().snapshot;
        t.revert(&snap).unwrap(); // "Windows" puts it back
        let s = e.status(&t);
        assert!(s.drifted && s.recorded);
        assert_eq!(e.apply(&t).unwrap(), Applied::Reapplied);
        e.revert(&t).unwrap();
        assert_eq!(t.detect().unwrap(), State::Default);
    }

    /// Something already set the way a tweak wants is left alone and not
    /// recorded — so revert can never undo a choice the user made themselves.
    #[test]
    fn a_setting_already_in_place_is_not_claimed() {
        let _guard = crate::REGISTRY_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let e = Engine::new(Journal::in_memory().unwrap());
        let t = scratch_tweak();
        t.apply().unwrap();
        assert_eq!(e.apply(&t).unwrap(), Applied::AlreadySet);
        assert_eq!(e.revert(&t).unwrap(), Reverted::NotApplied);
        assert_eq!(t.detect().unwrap(), State::Applied, "the user's own setting survives");
        let _ = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER).delete_subkey_all(&t.key.subkey);
    }

    #[test]
    fn selection_understands_profiles_ids_and_optional() {
        let all = select("background", true).unwrap();
        let required = select("background", false).unwrap();
        assert!(all.len() > required.len(), "optional tweaks exist");
        assert!(required.iter().all(|t| !t.optional()));
        assert_eq!(select("privacy.diagtrack", false).unwrap().len(), 1);
        assert!(select("nonsense", false).is_err());
    }
}
