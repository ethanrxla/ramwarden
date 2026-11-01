//! sway and i3 (Linux), through `swaymsg` / `i3-msg`.
//!
//! The two share the i3 IPC protocol, so one type serves both. The difference
//! that matters: sway reports Wayland windows with `app_id` and a `pid`, i3
//! reports X11 windows with `window_properties.class`. A leaf with neither is
//! a split container, not a window.
//!
//! # The command string is the risk
//!
//! Unlike AeroSpace, these take one command *string*, and `;` or `,` start
//! another command. A workspace called `x; exec rm -rf ~` would be a second
//! command. Core validates names before they get here; [`quoted`] refuses them
//! again so this crate is safe on its own.

use std::path::PathBuf;
use std::sync::Arc;

use ramwarden_platform::{
    Error, Result, WindowManager, WmCommand, WmKind, WmLayout, WmState, WmWindow, WmWorkspace,
};
use serde::Deserialize;
use serde_json::Value;

use crate::runner::{Runner, argv, home};

pub struct Ipc3 {
    kind: WmKind,
    runner: Arc<dyn Runner>,
}

#[derive(Deserialize)]
struct RawWorkspace {
    name: String,
    #[serde(default)]
    output: Option<String>,
    #[serde(default)]
    focused: bool,
}

/// Characters that end or restructure an i3/sway command.
const FORBIDDEN: &[char] = &[';', ',', '"', '[', ']', '\\', '\n', '\r'];

/// `"name"`, or an error if the name could break out of its quotes.
pub fn quoted(name: &str) -> Result<String> {
    if name.is_empty() || name.contains(FORBIDDEN) || name.chars().any(char::is_control) {
        return Err(Error::os(format!("refused unsafe name {name:?}")));
    }
    Ok(format!("\"{name}\""))
}

impl Ipc3 {
    pub fn sway(runner: Arc<dyn Runner>) -> Self {
        Ipc3 { kind: WmKind::Sway, runner }
    }

    pub fn i3(runner: Arc<dyn Runner>) -> Self {
        Ipc3 { kind: WmKind::I3, runner }
    }

    fn bin(&self) -> &'static str {
        match self.kind {
            WmKind::I3 => "i3-msg",
            _ => "swaymsg",
        }
    }

    fn call(&self, args: &[&str]) -> Result<String> {
        self.runner.run(self.bin(), &argv(args))
    }

    /// Parse `get_tree` and `get_workspaces`. Split out for tests.
    pub fn parse(kind: WmKind, tree: &str, workspaces: &str) -> Result<WmState> {
        let tree: Value = serde_json::from_str(tree).map_err(|e| Error::os(format!("get_tree: {e}")))?;
        let spaces: Vec<RawWorkspace> =
            serde_json::from_str(workspaces).map_err(|e| Error::os(format!("get_workspaces: {e}")))?;

        let mut state = WmState::empty(kind);
        for output in children(&tree) {
            let name = output["name"].as_str().unwrap_or_default();
            // `__i3` holds the scratchpad, which is not a monitor.
            if output["type"] == "output" && !name.starts_with("__") {
                state.monitors.push(name.to_string());
            }
            for ws in children(output).filter(|n| n["type"] == "workspace") {
                let ws_name = ws["name"].as_str().unwrap_or_default();
                if ws_name.starts_with("__") {
                    continue;
                }
                collect_windows(ws, ws_name, &mut state.windows);
            }
        }
        state.workspaces = spaces
            .into_iter()
            .map(|s| WmWorkspace { name: s.name, monitor: s.output, focused: s.focused })
            .collect();
        Ok(state)
    }

    /// The command string for one command.
    pub fn command(cmd: &WmCommand) -> Result<String> {
        Ok(match cmd {
            WmCommand::Focus { window } => format!("[con_id={window}] focus"),
            WmCommand::MoveToWorkspace { window, workspace } => {
                format!("[con_id={window}] move container to workspace {}", quoted(workspace)?)
            }
            WmCommand::SwitchWorkspace { workspace } => format!("workspace {}", quoted(workspace)?),
            WmCommand::MoveWorkspaceToMonitor { workspace, monitor } => format!(
                "workspace {}; move workspace to output {}",
                quoted(workspace)?,
                quoted(monitor)?
            ),
            WmCommand::SetLayout { window, layout } => {
                let action = match layout {
                    WmLayout::Tiles => "layout default",
                    WmLayout::Accordion => "layout tabbed",
                    WmLayout::Horizontal => "layout splith",
                    WmLayout::Vertical => "layout splitv",
                    WmLayout::Floating => "floating enable",
                    WmLayout::Tiling => "floating disable",
                };
                format!("[con_id={window}] {action}")
            }
            WmCommand::Balance { .. } => {
                return Err(Error::Unsupported("balancing sizes on sway and i3"));
            }
        })
    }

    /// Read a command reply: `[{"success": false, "error": "…"}]` on refusal.
    fn check_reply(text: &str) -> Result<()> {
        let Ok(replies) = serde_json::from_str::<Vec<Value>>(text) else {
            return Ok(());
        };
        for r in replies {
            if r["success"] == false {
                let why = r["error"].as_str().unwrap_or("refused");
                return Err(Error::os(why.to_string()));
            }
        }
        Ok(())
    }
}

/// Tiled and floating children of a node.
fn children(node: &Value) -> impl Iterator<Item = &Value> {
    let tiled = node["nodes"].as_array().into_iter().flatten();
    let floating = node["floating_nodes"].as_array().into_iter().flatten();
    tiled.chain(floating)
}

fn collect_windows(node: &Value, workspace: &str, out: &mut Vec<WmWindow>) {
    let app = node["app_id"]
        .as_str()
        .or_else(|| node["window_properties"]["class"].as_str());
    let is_window = app.is_some() || node["window"].is_u64() || node["pid"].is_i64();
    if is_window && let Some(id) = node["id"].as_u64() {
        out.push(WmWindow {
            id,
            app: app.unwrap_or_default().to_string(),
            title: node["name"].as_str().unwrap_or_default().to_string(),
            workspace: workspace.to_string(),
            pid: node["pid"].as_i64().and_then(|p| i32::try_from(p).ok()),
            focused: node["focused"] == true,
        });
    }
    for c in children(node) {
        collect_windows(c, workspace, out);
    }
}

impl WindowManager for Ipc3 {
    fn kind(&self) -> WmKind {
        self.kind
    }

    fn state(&mut self) -> Result<WmState> {
        let tree = self.call(&["-t", "get_tree"])?;
        let spaces = self.call(&["-t", "get_workspaces"])?;
        Self::parse(self.kind, &tree, &spaces)
    }

    fn run(&mut self, cmd: &WmCommand) -> Result<()> {
        let line = Self::command(cmd)?;
        let reply = self.runner.run(self.bin(), &[line])?;
        Self::check_reply(&reply)
    }

    fn config_path(&self) -> Option<PathBuf> {
        // Both report the file they actually loaded.
        if let Ok(v) = self.call(&["-t", "get_version"])
            && let Ok(v) = serde_json::from_str::<Value>(&v)
            && let Some(p) = v["loaded_config_file_name"].as_str()
        {
            return Some(PathBuf::from(p));
        }
        let dir = if self.kind == WmKind::I3 { "i3" } else { "sway" };
        let p = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join(".config"))
            .join(dir)
            .join("config");
        p.exists().then_some(p)
    }

    fn check_config(&self, text: &str) -> Result<()> {
        // `sway -C` / `i3 -C` validate a file without loading it. If the
        // binary cannot be run here, the reload is the check, and a failed
        // reload restores the backup.
        let tmp = std::env::temp_dir().join(format!("ramwarden-{}-check.conf", std::process::id()));
        std::fs::write(&tmp, text).map_err(|e| Error::os(format!("writing a check file: {e}")))?;
        let bin = if self.kind == WmKind::I3 { "i3" } else { "sway" };
        let res = self.runner.run(bin, &argv(&["-C", "-c", &tmp.to_string_lossy()]));
        let _ = std::fs::remove_file(&tmp);
        match res {
            Err(Error::Unsupported(_)) | Ok(_) => Ok(()),
            Err(e) => Err(e),
        }
    }

    fn reload_config(&mut self) -> Result<()> {
        let reply = self.call(&["reload"])?;
        Self::check_reply(&reply)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::scripted::Scripted;

    const TREE: &str = r#"{
      "id": 1, "type": "root", "nodes": [
        {"id": 2, "type": "output", "name": "__i3", "nodes": [
          {"id": 3, "type": "workspace", "name": "__i3_scratch", "nodes": [], "floating_nodes": [
            {"id": 30, "type": "floating_con", "app_id": "hidden", "pid": 5, "name": "x", "nodes": []}
          ]}
        ]},
        {"id": 10, "type": "output", "name": "DP-1", "nodes": [
          {"id": 11, "type": "workspace", "name": "1", "nodes": [
            {"id": 12, "type": "con", "name": null, "layout": "splith", "nodes": [
              {"id": 13, "type": "con", "app_id": "foot", "pid": 1200, "name": "~", "focused": true, "nodes": []},
              {"id": 14, "type": "con", "app_id": null, "window": 6291459,
               "window_properties": {"class": "Slack"}, "name": "Slack | general", "nodes": []}
            ]}
          ], "floating_nodes": [
            {"id": 15, "type": "floating_con", "app_id": "pavucontrol", "pid": 1300, "name": "Volume", "nodes": []}
          ]}
        ]}
      ]}"#;
    const SPACES: &str = r#"[{"name": "1", "output": "DP-1", "focused": true}, {"name": "web", "output": "DP-1", "focused": false}]"#;

    #[test]
    fn the_tree_flattens_to_windows_and_skips_the_scratchpad_and_splits() {
        let s = Ipc3::parse(WmKind::Sway, TREE, SPACES).unwrap();
        let ids: Vec<u64> = s.windows.iter().map(|w| w.id).collect();
        assert_eq!(ids, vec![13, 14, 15]);
        assert_eq!(s.window(13).unwrap().pid, Some(1200));
        assert!(s.window(13).unwrap().focused);
        assert_eq!(s.window(14).unwrap().app, "Slack");
        assert_eq!(s.window(15).unwrap().workspace, "1");
        assert_eq!(s.monitors, vec!["DP-1"]);
        assert_eq!(s.workspaces.len(), 2);
    }

    #[test]
    fn names_that_could_start_a_second_command_are_refused() {
        for bad in ["x; exec rm -rf ~", "a,b", "q\"", "[x]", "", "a\nb"] {
            assert!(quoted(bad).is_err(), "{bad:?} should be refused");
        }
        let c = Ipc3::command(&WmCommand::MoveToWorkspace { window: 14, workspace: "web".into() }).unwrap();
        assert_eq!(c, "[con_id=14] move container to workspace \"web\"");
        assert!(Ipc3::command(&WmCommand::SwitchWorkspace { workspace: "1; exec x".into() }).is_err());
    }

    #[test]
    fn a_reply_marked_unsuccessful_is_an_error_with_swaymsgs_reason() {
        let r = Arc::new(Scripted::default().reply("swaymsg", r#"[{"success": false, "error": "No matching node"}]"#));
        let mut w = Ipc3::sway(r);
        let e = w.run(&WmCommand::Focus { window: 99 }).unwrap_err();
        assert!(e.to_string().contains("No matching node"));
    }

    #[test]
    fn balance_is_unsupported_rather_than_faked() {
        assert!(matches!(
            Ipc3::command(&WmCommand::Balance { workspace: "1".into() }),
            Err(Error::Unsupported(_))
        ));
    }
}
