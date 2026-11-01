//! AeroSpace (macOS), through the `aerospace` CLI.
//!
//! Every query uses `--json` with an explicit `--format`, so the keys are the
//! format variable names (`window-id`, `app-name`, …) and do not change when a
//! user customises AeroSpace's default output. Every name is passed after `--`,
//! so a workspace called `-h` is a workspace, not a flag.

use std::path::PathBuf;
use std::sync::Arc;

use ramwarden_platform::{
    Error, Result, WindowManager, WmCommand, WmKind, WmState, WmWindow, WmWorkspace,
};
use serde::Deserialize;

use crate::runner::{Runner, argv, home};

const BIN: &str = "aerospace";

pub struct AeroSpace {
    runner: Arc<dyn Runner>,
}

#[derive(Deserialize)]
struct RawWindow {
    #[serde(rename = "window-id")]
    id: u64,
    #[serde(rename = "app-name", default)]
    app: String,
    #[serde(rename = "window-title", default)]
    title: String,
    #[serde(default)]
    workspace: String,
    #[serde(rename = "app-pid", default)]
    pid: Option<i32>,
}

#[derive(Deserialize)]
struct RawWorkspace {
    workspace: String,
    #[serde(rename = "workspace-is-focused", default)]
    focused: bool,
    #[serde(rename = "monitor-name", default)]
    monitor: Option<String>,
}

#[derive(Deserialize)]
struct RawMonitor {
    #[serde(rename = "monitor-name")]
    name: String,
}

impl AeroSpace {
    pub fn new(runner: Arc<dyn Runner>) -> Self {
        AeroSpace { runner }
    }

    fn call(&self, args: &[&str]) -> Result<String> {
        self.runner.run(BIN, &argv(args))
    }

    /// Parse the three listings into one state. Split out for tests.
    pub fn parse(windows: &str, focused: Option<&str>, workspaces: &str, monitors: &str) -> Result<WmState> {
        let bad = |what: &str, e: serde_json::Error| Error::os(format!("aerospace {what}: {e}"));
        let raw: Vec<RawWindow> = serde_json::from_str(windows).map_err(|e| bad("list-windows", e))?;
        let focused_id: Option<u64> = focused
            .and_then(|f| serde_json::from_str::<Vec<RawWindow>>(f).ok())
            .and_then(|v| v.first().map(|w| w.id));
        let spaces: Vec<RawWorkspace> =
            serde_json::from_str(workspaces).map_err(|e| bad("list-workspaces", e))?;
        let mons: Vec<RawMonitor> = serde_json::from_str(monitors).map_err(|e| bad("list-monitors", e))?;

        Ok(WmState {
            kind: WmKind::AeroSpace,
            windows: raw
                .into_iter()
                .map(|w| WmWindow {
                    focused: Some(w.id) == focused_id,
                    id: w.id,
                    app: w.app,
                    title: w.title,
                    workspace: w.workspace,
                    pid: w.pid,
                })
                .collect(),
            workspaces: spaces
                .into_iter()
                .map(|s| WmWorkspace {
                    name: s.workspace,
                    monitor: s.monitor,
                    focused: s.focused,
                })
                .collect(),
            monitors: mons.into_iter().map(|m| m.name).collect(),
        })
    }

    /// The argv for one command.
    pub fn args(cmd: &WmCommand) -> Vec<String> {
        let id = |w: &u64| w.to_string();
        match cmd {
            WmCommand::Focus { window } => argv(&["focus", "--window-id", &id(window)]),
            WmCommand::MoveToWorkspace { window, workspace } => {
                argv(&["move-node-to-workspace", "--window-id", &id(window), "--", workspace])
            }
            WmCommand::SwitchWorkspace { workspace } => argv(&["workspace", "--", workspace]),
            WmCommand::MoveWorkspaceToMonitor { workspace, monitor } => {
                argv(&["move-workspace-to-monitor", "--workspace", workspace, "--", monitor])
            }
            WmCommand::SetLayout { window, layout } => {
                argv(&["layout", "--window-id", &id(window), layout.as_str()])
            }
            WmCommand::Balance { workspace } => argv(&["balance-sizes", "--workspace", workspace]),
        }
    }
}

impl WindowManager for AeroSpace {
    fn kind(&self) -> WmKind {
        WmKind::AeroSpace
    }

    fn state(&mut self) -> Result<WmState> {
        let windows = self.call(&[
            "list-windows", "--all", "--json", "--format",
            "%{window-id}%{app-name}%{window-title}%{workspace}%{app-pid}",
        ])?;
        // Nothing focused (an empty workspace) is an error exit, not a failure.
        let focused = self.call(&["list-windows", "--focused", "--json", "--format", "%{window-id}"]).ok();
        let workspaces = self.call(&[
            "list-workspaces", "--all", "--json", "--format",
            "%{workspace}%{workspace-is-focused}%{monitor-name}",
        ])?;
        let monitors = self.call(&["list-monitors", "--json", "--format", "%{monitor-name}"])?;
        Self::parse(&windows, focused.as_deref(), &workspaces, &monitors)
    }

    fn run(&mut self, cmd: &WmCommand) -> Result<()> {
        self.runner.run(BIN, &Self::args(cmd)).map(|_| ())
    }

    fn config_path(&self) -> Option<PathBuf> {
        if let Ok(p) = self.call(&["config", "--config-path"]) {
            let p = p.trim();
            if !p.is_empty() {
                return Some(PathBuf::from(p));
            }
        }
        // AeroSpace's own search order, for when the CLI could not say.
        let dot = home().join(".aerospace.toml");
        let xdg = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join(".config"))
            .join("aerospace")
            .join("aerospace.toml");
        [dot, xdg].into_iter().find(|p| p.exists())
    }

    fn check_config(&self, text: &str) -> Result<()> {
        text.parse::<toml::Table>()
            .map(|_| ())
            .map_err(|e| Error::os(format!("not valid TOML: {e}")))
    }

    fn reload_config(&mut self) -> Result<()> {
        self.call(&["reload-config", "--no-gui"]).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::scripted::Scripted;
    use ramwarden_platform::WmLayout;

    const WINDOWS: &str = r#"[
      {"window-id": 4242, "app-name": "Slack", "window-title": "general", "workspace": "1", "app-pid": 901},
      {"window-id": 77, "app-name": "Safari", "window-title": "Docs", "workspace": "B", "app-pid": 455}
    ]"#;
    const FOCUSED: &str = r#"[{"window-id": 77}]"#;
    const SPACES: &str = r#"[
      {"workspace": "1", "workspace-is-focused": false, "monitor-name": "Built-in Retina Display"},
      {"workspace": "B", "workspace-is-focused": true, "monitor-name": "LG UltraFine"}
    ]"#;
    const MONITORS: &str = r#"[{"monitor-name": "Built-in Retina Display"}, {"monitor-name": "LG UltraFine"}]"#;

    #[test]
    fn listings_parse_into_one_state() {
        let s = AeroSpace::parse(WINDOWS, Some(FOCUSED), SPACES, MONITORS).unwrap();
        assert_eq!(s.windows.len(), 2);
        assert_eq!(s.window(4242).unwrap().app, "Slack");
        assert_eq!(s.window(4242).unwrap().pid, Some(901));
        assert!(s.window(77).unwrap().focused);
        assert!(!s.window(4242).unwrap().focused);
        assert!(s.workspaces[1].focused);
        assert_eq!(s.workspaces[1].monitor.as_deref(), Some("LG UltraFine"));
        assert_eq!(s.monitors.len(), 2);
    }

    #[test]
    fn no_focused_window_is_not_an_error() {
        let s = AeroSpace::parse(WINDOWS, None, SPACES, MONITORS).unwrap();
        assert!(s.windows.iter().all(|w| !w.focused));
    }

    #[test]
    fn names_go_after_a_double_dash_so_they_cannot_be_flags() {
        let a = AeroSpace::args(&WmCommand::MoveToWorkspace { window: 9, workspace: "-h".into() });
        assert_eq!(a, argv(&["move-node-to-workspace", "--window-id", "9", "--", "-h"]));
        let a = AeroSpace::args(&WmCommand::SetLayout { window: 9, layout: WmLayout::Floating });
        assert_eq!(a, argv(&["layout", "--window-id", "9", "floating"]));
    }

    #[test]
    fn state_runs_the_four_listings_and_tolerates_no_focus() {
        let r = Arc::new(
            Scripted::default()
                .reply("aerospace list-windows --all", WINDOWS)
                .fail("aerospace list-windows --focused", "no window is focused")
                .reply("aerospace list-workspaces --all", SPACES)
                .reply("aerospace list-monitors", MONITORS),
        );
        let mut a = AeroSpace::new(r.clone());
        let s = a.state().unwrap();
        assert_eq!(s.windows.len(), 2);
        assert_eq!(r.calls().len(), 4);
    }

    #[test]
    fn a_refused_command_carries_aerospaces_own_words() {
        let r = Arc::new(Scripted::default().fail("aerospace move-node-to-workspace", "Invalid window-id"));
        let mut a = AeroSpace::new(r);
        let e = a.run(&WmCommand::MoveToWorkspace { window: 1, workspace: "2".into() }).unwrap_err();
        assert!(e.to_string().contains("Invalid window-id"));
    }

    #[test]
    fn config_text_is_checked_as_toml() {
        let a = AeroSpace::new(Arc::new(Scripted::default()));
        assert!(a.check_config("gaps.inner.horizontal = 8\n").is_ok());
        assert!(a.check_config("gaps = [").is_err());
    }
}
