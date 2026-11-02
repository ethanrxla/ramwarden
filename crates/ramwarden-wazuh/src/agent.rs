//! The local Wazuh agent: is it installed, and is it connected?
//!
//! The agent writes its connection state to a small file every few seconds:
//!
//! ```text
//! # State file for wazuh-agentd
//! status='connected'
//! last_keepalive='2026-11-01 08:00:01'
//! last_ack='2026-11-01 08:00:01'
//! msg_count='3104'
//! ```
//!
//! | OS | Install dir | State file |
//! |---|---|---|
//! | Linux | `/var/ossec` | `var/run/wazuh-agentd.state` |
//! | macOS | `/Library/Ossec` | `var/run/wazuh-agentd.state` |
//! | Windows | `%ProgramFiles(x86)%\ossec-agent` | `wazuh-agent.state` |
//!
//! On Linux and macOS the install dir is `750 root:wazuh`, so a daemon running
//! as the user sees that the agent is installed but cannot read its state.
//! That is reported as installed-but-unknown, and the manager's view fills in.

use std::path::{Path, PathBuf};

use serde::Serialize;

/// What the local agent says about itself.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct LocalAgent {
    pub installed: bool,
    /// `connected`, `disconnected` or `pending`; `None` when the state file
    /// could not be read.
    pub status: Option<String>,
    pub last_keepalive: Option<String>,
}

impl LocalAgent {
    /// Read the agent installed at this OS's standard location.
    pub fn read() -> LocalAgent {
        match install_dir() {
            Some(dir) => LocalAgent::read_at(&dir),
            None => LocalAgent::default(),
        }
    }

    /// Read an agent installed at `dir`. Split out so tests can use a
    /// synthetic install.
    pub fn read_at(dir: &Path) -> LocalAgent {
        if !dir.exists() {
            return LocalAgent::default();
        }
        match std::fs::read_to_string(state_file(dir)) {
            Ok(text) => LocalAgent { installed: true, ..LocalAgent::parse(&text) },
            Err(_) => LocalAgent { installed: true, ..LocalAgent::default() },
        }
    }

    /// Parse the state file's `key='value'` lines.
    pub fn parse(text: &str) -> LocalAgent {
        let mut out = LocalAgent::default();
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('#') {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else { continue };
            let v = v.trim().trim_matches(['\'', '"']).to_string();
            match k.trim() {
                "status" if !v.is_empty() => out.status = Some(v),
                "last_keepalive" if !v.is_empty() => out.last_keepalive = Some(v),
                _ => {}
            }
        }
        out
    }

    pub fn is_connected(&self) -> bool {
        self.status.as_deref() == Some("connected")
    }
}

/// Where the agent installs on this OS.
pub fn install_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        let base = std::env::var_os("ProgramFiles(x86)")
            .or_else(|| std::env::var_os("ProgramFiles"))
            .map(PathBuf::from)?;
        Some(base.join("ossec-agent"))
    } else if cfg!(target_os = "macos") {
        Some(PathBuf::from("/Library/Ossec"))
    } else {
        Some(PathBuf::from("/var/ossec"))
    }
}

/// The state file inside an install dir.
pub fn state_file(dir: &Path) -> PathBuf {
    if cfg!(windows) {
        dir.join("wazuh-agent.state")
    } else {
        dir.join("var").join("run").join("wazuh-agentd.state")
    }
}

/// The name the agent enrols under unless told otherwise: the hostname.
pub fn default_agent_name() -> String {
    if let Ok(n) = std::env::var("COMPUTERNAME")
        && !n.trim().is_empty()
    {
        return n.trim().to_string();
    }
    for f in ["/proc/sys/kernel/hostname", "/etc/hostname"] {
        if let Ok(n) = std::fs::read_to_string(f)
            && !n.trim().is_empty()
        {
            return n.trim().to_string();
        }
    }
    // macOS has neither file.
    std::process::Command::new("hostname")
        .arg("-s")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATE: &str = "# State file for wazuh-agentd\n\
        status='connected'\n\
        last_keepalive='2026-11-01 08:00:01'\n\
        last_ack='2026-11-01 08:00:01'\n\
        msg_count='3104'\n";

    #[test]
    fn parses_the_agent_state_file() {
        let a = LocalAgent::parse(STATE);
        assert_eq!(a.status.as_deref(), Some("connected"));
        assert_eq!(a.last_keepalive.as_deref(), Some("2026-11-01 08:00:01"));
        assert!(a.is_connected());
        assert!(!LocalAgent::parse("status='disconnected'\n").is_connected());
        assert_eq!(LocalAgent::parse("garbage\n# status='connected'"), LocalAgent::default());
    }

    #[test]
    fn a_missing_install_is_not_installed() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(LocalAgent::read_at(&dir.path().join("nope")), LocalAgent::default());
    }

    #[test]
    fn an_unreadable_state_is_installed_with_unknown_status() {
        let dir = tempfile::tempdir().unwrap();
        let a = LocalAgent::read_at(dir.path());
        assert!(a.installed);
        assert_eq!(a.status, None);
    }

    #[test]
    fn reads_a_synthetic_install() {
        let dir = tempfile::tempdir().unwrap();
        let f = state_file(dir.path());
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(&f, STATE).unwrap();
        let a = LocalAgent::read_at(dir.path());
        assert!(a.installed && a.is_connected());
    }
}
