//! What the launcher remembers between runs, in one JSON file.
//!
//! `%LOCALAPPDATA%\ramwarden\launcher\state.json` on Windows,
//! `~/.local/share/ramwarden/launcher/state.json` elsewhere. Written atomically
//! (temp file, then rename) so a crash mid-write leaves the old state, never a
//! half file that resets `highest_sequence` and reopens the replay window.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::Result;
use crate::updater::Available;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct State {
    /// The highest manifest sequence ever accepted. Never lowered.
    pub highest_sequence: u64,
    /// Warden id to installed version, for wardens other than `ram` (whose
    /// version is the launcher's own: they ship in one installer).
    pub installed: BTreeMap<String, String>,
    /// Warden id to the version an installer was started for but not yet
    /// confirmed (the Windows RAM installer replaces the launcher, so the
    /// launcher exits before it finishes).
    pub pending: BTreeMap<String, String>,
    /// Unix seconds of the last completed check.
    pub last_check: Option<u64>,
    /// What that check found, for the tray, menu-bar app and `status`.
    pub available: Vec<Available>,
    /// The last check's error, shown instead of a stale "up to date".
    pub last_error: Option<String>,
}

impl State {
    /// The launcher's own directory: state, plus `staging/` for downloads.
    pub fn default_dir() -> PathBuf {
        ramwarden_core::config::data_dir()
            .join("ramwarden")
            .join("launcher")
    }

    /// Missing or unreadable state is a fresh state: the worst case is one
    /// replay window, which the next accepted manifest closes.
    pub fn load(dir: &Path) -> State {
        std::fs::read(dir.join("state.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join("state.json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?)?;
        std::fs::rename(tmp, dir.join("state.json"))?;
        Ok(())
    }
}

/// The token the app stores after device sign-in, sent only to fetch paid
/// wardens. `None` means a free install: paid wardens show as locked.
pub fn device_token() -> Option<String> {
    let path = ramwarden_core::config::config_dir()
        .join("ramwarden")
        .join("device-token");
    std::fs::read_to_string(path)
        .ok()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_and_loads_and_tolerates_garbage() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(State::load(dir.path()), State::default());
        let mut s = State {
            highest_sequence: 9,
            ..State::default()
        };
        s.installed.insert("wazuh".into(), "0.1.0".into());
        s.save(dir.path()).unwrap();
        assert_eq!(State::load(dir.path()), s);
        std::fs::write(dir.path().join("state.json"), b"{not json").unwrap();
        assert_eq!(State::load(dir.path()), State::default());
    }
}
