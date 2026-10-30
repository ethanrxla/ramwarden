//! An app's "start with Windows" entry, switched off the way Task Manager does.
//!
//! The `Run` value itself is never touched. Windows keeps a parallel
//! `StartupApproved\Run` value per entry whose first byte says whether it is
//! enabled (`02`) or disabled (`03`); Task Manager's Startup tab and Settings
//! both flip that byte. Doing the same means the app's own setting, its
//! updater and the user's Settings page all stay in agreement with RamWarden.

use ramwarden_platform::Result;
use serde_json::Value;
use winreg::RegValue;
use winreg::enums::REG_BINARY;

use crate::registry::{KeyPath, io, restore_value, snapshot_value};
use crate::tweak::{Scope, State, Tweak};

const RUN: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
const APPROVED: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";

/// Disabled marker: `03` followed by the FILETIME of the change.
fn disabled_bytes() -> Vec<u8> {
    const EPOCH_DIFF: u64 = 116_444_736_000_000_000;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let filetime = EPOCH_DIFF + (now.as_nanos() / 100) as u64;
    let mut b = vec![3, 0, 0, 0];
    b.extend_from_slice(&filetime.to_le_bytes());
    b
}

/// Whether the approval bytes mark an entry disabled (odd first byte).
pub fn is_disabled(bytes: &[u8]) -> bool {
    bytes.first().is_some_and(|b| b & 1 == 1)
}

/// Turns off one per-user startup entry by its `Run` value name.
pub struct StartupTweak {
    pub id: String,
    pub description: String,
    pub optional: bool,
    /// The value name under `...\CurrentVersion\Run`, e.g. `EpicGamesLauncher`.
    pub run: String,
}

impl StartupTweak {
    fn approved() -> KeyPath {
        KeyPath::parse(APPROVED).expect("constant path")
    }

    fn registered(&self) -> bool {
        let run = KeyPath::parse(RUN).expect("constant path");
        snapshot_value(&run, &self.run)["exists"].as_bool() == Some(true)
    }
}

impl Tweak for StartupTweak {
    fn id(&self) -> &str {
        &self.id
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn scope(&self) -> Scope {
        Scope::User
    }

    fn optional(&self) -> bool {
        self.optional
    }

    fn target(&self) -> String {
        format!("startup entry {:?} -> disabled", self.run)
    }

    fn detect(&self) -> Result<State> {
        if !self.registered() {
            return Ok(State::Missing);
        }
        let snap = snapshot_value(&Self::approved(), &self.run);
        let bytes: Vec<u8> = serde_json::from_value(snap["bytes"].clone()).unwrap_or_default();
        Ok(if is_disabled(&bytes) { State::Applied } else { State::Default })
    }

    fn snapshot(&self) -> Result<Value> {
        Ok(snapshot_value(&Self::approved(), &self.run))
    }

    fn apply(&self) -> Result<()> {
        let key = Self::approved();
        let value = RegValue {
            bytes: disabled_bytes().into(),
            vtype: REG_BINARY,
        };
        winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER)
            .create_subkey(&key.subkey)
            .and_then(|(k, _)| k.set_raw_value(&self.run, &value))
            .map_err(|e| io(&self.target(), e))
    }

    fn revert(&self, snapshot: &Value) -> Result<()> {
        restore_value(&Self::approved(), &self.run, snapshot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_task_manager_encoding_is_understood() {
        assert!(!is_disabled(&[2, 0, 0, 0]));
        assert!(is_disabled(&[3, 0, 0, 0]));
        assert!(!is_disabled(&[6, 0, 0, 0]), "06 is enabled too");
        assert!(!is_disabled(&[]));
        let d = disabled_bytes();
        assert_eq!(d.len(), 12);
        assert!(is_disabled(&d));
    }

    #[test]
    fn an_app_with_no_startup_entry_is_missing_not_default() {
        let t = StartupTweak {
            id: "t".into(),
            description: "t".into(),
            optional: false,
            run: "RamWardenNoSuchApp".into(),
        };
        assert_eq!(t.detect().unwrap(), State::Missing);
    }
}
