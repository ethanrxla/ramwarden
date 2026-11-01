//! komorebi (Windows), through `komorebic`.
//!
//! komorebi's CLI acts on the *focused* window and the *focused* workspace.
//! So a move is "focus that window, then move the focused window", and focus
//! itself goes through `eager-focus <exe>`, which picks the first window of
//! that executable. When an executable has more than one window that is
//! ambiguous, and the command is refused rather than moving whichever window
//! komorebi happens to pick.
//!
//! Workspaces without a name are addressed as `#<monitor>:<workspace>`
//! (1-based) and driven with the index commands.

use std::path::PathBuf;
use std::sync::Arc;

use ramwarden_platform::{
    Error, Result, WindowManager, WmCommand, WmKind, WmLayout, WmState, WmWindow, WmWorkspace,
};
use serde_json::Value;

use crate::runner::{Runner, argv, home};

const BIN: &str = "komorebic";

pub struct Komorebi {
    runner: Arc<dyn Runner>,
    /// The last state read, needed to translate names into indices.
    last: Option<WmState>,
    /// Windows komorebi has floating, from the last state.
    floating: Vec<u64>,
}

/// `{"elements": [...], "focused": n}`, komorebi's ring type.
fn ring(v: &Value) -> (Vec<&Value>, Option<usize>) {
    let elements = v["elements"].as_array().map(|a| a.iter().collect()).unwrap_or_default();
    (elements, v["focused"].as_u64().map(|f| f as usize))
}

impl Komorebi {
    pub fn new(runner: Arc<dyn Runner>) -> Self {
        Komorebi { runner, last: None, floating: Vec::new() }
    }

    /// Parse `komorebic state`. Returns the state and the floating hwnds.
    pub fn parse(text: &str) -> Result<(WmState, Vec<u64>)> {
        let v: Value = serde_json::from_str(text).map_err(|e| Error::os(format!("komorebic state: {e}")))?;
        let mut state = WmState::empty(WmKind::Komorebi);
        let mut floating = Vec::new();
        let (monitors, focused_monitor) = ring(&v["monitors"]);

        for (m, mon) in monitors.iter().enumerate() {
            let mon_name = mon["name"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| format!("monitor {}", m + 1));
            state.monitors.push(mon_name.clone());
            let (spaces, focused_space) = ring(&mon["workspaces"]);
            for (w, ws) in spaces.iter().enumerate() {
                let name = ws["name"]
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("#{}:{}", m + 1, w + 1));
                let ws_focused = focused_monitor == Some(m) && focused_space == Some(w);
                state.workspaces.push(WmWorkspace {
                    name: name.clone(),
                    monitor: Some(mon_name.clone()),
                    focused: ws_focused,
                });

                let (containers, focused_container) = ring(&ws["containers"]);
                for (c, con) in containers.iter().enumerate() {
                    let (wins, focused_win) = ring(&con["windows"]);
                    for (i, win) in wins.iter().enumerate() {
                        let focused = ws_focused && focused_container == Some(c) && focused_win == Some(i);
                        if let Some(w) = window(win, &name, focused) {
                            state.windows.push(w);
                        }
                    }
                }
                let floats = ws["floating_windows"]
                    .as_array()
                    .map(|a| a.iter().collect::<Vec<_>>())
                    .unwrap_or_else(|| ring(&ws["floating_windows"]).0);
                for win in floats {
                    if let Some(w) = window(win, &name, false) {
                        floating.push(w.id);
                        state.windows.push(w);
                    }
                }
            }
        }
        Ok((state, floating))
    }

    /// `(monitor, workspace)` indices, 0-based, for a workspace name.
    fn index_of(&self, workspace: &str) -> Option<(usize, usize)> {
        let rest = workspace.strip_prefix('#')?;
        let (m, w) = rest.split_once(':')?;
        Some((m.parse::<usize>().ok()?.checked_sub(1)?, w.parse::<usize>().ok()?.checked_sub(1)?))
    }

    fn monitor_index(&self, monitor: &str) -> Result<usize> {
        self.last
            .as_ref()
            .and_then(|s| s.monitors.iter().position(|m| m == monitor))
            .ok_or_else(|| Error::missing(format!("monitor {monitor}")))
    }

    fn call(&self, args: &[&str]) -> Result<()> {
        self.runner.run(BIN, &argv(args)).map(|_| ())
    }

    fn focus_workspace(&self, workspace: &str) -> Result<()> {
        match self.index_of(workspace) {
            Some((m, w)) => self.call(&["focus-monitor-workspace", &m.to_string(), &w.to_string()]),
            None => self.call(&["focus-named-workspace", workspace]),
        }
    }

    /// Focus one specific window, or refuse if komorebi cannot be told which.
    fn focus_window(&self, id: u64) -> Result<()> {
        let state = self.last.as_ref().ok_or_else(|| Error::missing("komorebi state"))?;
        let win = state.window(id).ok_or_else(|| Error::missing(format!("window {id}")))?;
        if win.focused {
            return Ok(());
        }
        let same_exe = state.windows.iter().filter(|w| w.app == win.app).count();
        if same_exe > 1 {
            return Err(Error::os(format!(
                "{} has {same_exe} windows and komorebi can only focus the first; focus it yourself and ask again",
                win.app
            )));
        }
        self.call(&["eager-focus", &win.app])
    }
}

fn window(v: &Value, workspace: &str, focused: bool) -> Option<WmWindow> {
    Some(WmWindow {
        id: v["hwnd"].as_u64()?,
        app: v["exe"].as_str().unwrap_or_default().to_string(),
        title: v["title"].as_str().unwrap_or_default().to_string(),
        workspace: workspace.to_string(),
        pid: None,
        focused,
    })
}

impl WindowManager for Komorebi {
    fn kind(&self) -> WmKind {
        WmKind::Komorebi
    }

    fn state(&mut self) -> Result<WmState> {
        let text = self.runner.run(BIN, &argv(&["state"]))?;
        let (state, floating) = Self::parse(&text)?;
        self.last = Some(state.clone());
        self.floating = floating;
        Ok(state)
    }

    fn run(&mut self, cmd: &WmCommand) -> Result<()> {
        if self.last.is_none() {
            self.state()?;
        }
        match cmd {
            WmCommand::Focus { window } => self.focus_window(*window),
            WmCommand::MoveToWorkspace { window, workspace } => {
                self.focus_window(*window)?;
                match self.index_of(workspace) {
                    Some((m, w)) => self.call(&["move-to-monitor-workspace", &m.to_string(), &w.to_string()]),
                    None => self.call(&["move-to-named-workspace", workspace]),
                }
            }
            WmCommand::SwitchWorkspace { workspace } => self.focus_workspace(workspace),
            WmCommand::MoveWorkspaceToMonitor { workspace, monitor } => {
                let m = self.monitor_index(monitor)?;
                self.focus_workspace(workspace)?;
                self.call(&["move-workspace-to-monitor", &m.to_string()])
            }
            WmCommand::SetLayout { window, layout } => {
                self.focus_window(*window)?;
                let floating = self.floating.contains(window);
                match layout {
                    WmLayout::Floating if floating => Ok(()),
                    WmLayout::Tiling if !floating => Ok(()),
                    WmLayout::Floating | WmLayout::Tiling => self.call(&["toggle-float"]),
                    WmLayout::Tiles => self.call(&["change-layout", "bsp"]),
                    WmLayout::Horizontal => self.call(&["change-layout", "columns"]),
                    WmLayout::Vertical => self.call(&["change-layout", "rows"]),
                    WmLayout::Accordion => Err(Error::Unsupported("accordion layout on komorebi")),
                }
            }
            WmCommand::Balance { workspace } => {
                self.focus_workspace(workspace)?;
                self.call(&["retile"])
            }
        }
    }

    fn config_path(&self) -> Option<PathBuf> {
        let dir = std::env::var_os("KOMOREBI_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(home);
        let p = dir.join("komorebi.json");
        p.exists().then_some(p)
    }

    fn check_config(&self, text: &str) -> Result<()> {
        serde_json::from_str::<serde_json::Map<String, Value>>(text)
            .map(|_| ())
            .map_err(|e| Error::os(format!("not a JSON object: {e}")))
    }

    fn reload_config(&mut self) -> Result<()> {
        self.call(&["reload-configuration"])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::scripted::Scripted;

    const STATE: &str = r#"{"monitors": {"focused": 0, "elements": [
      {"name": "DISPLAY1", "workspaces": {"focused": 1, "elements": [
        {"name": "code", "containers": {"focused": 0, "elements": [
          {"windows": {"focused": 0, "elements": [{"hwnd": 100, "exe": "Code.exe", "title": "main.rs"}]}}
        ]}, "floating_windows": []},
        {"name": null, "containers": {"focused": 1, "elements": [
          {"windows": {"focused": 0, "elements": [{"hwnd": 200, "exe": "Slack.exe", "title": "general"}]}},
          {"windows": {"focused": 0, "elements": [{"hwnd": 300, "exe": "firefox.exe", "title": "a"}]}},
          {"windows": {"focused": 0, "elements": [{"hwnd": 301, "exe": "firefox.exe", "title": "b"}]}}
        ]}, "floating_windows": [{"hwnd": 400, "exe": "Spotify.exe", "title": "music"}]}
      ]}}
    ]}}"#;

    #[test]
    fn the_rings_flatten_and_unnamed_workspaces_get_an_index_name() {
        let (s, floating) = Komorebi::parse(STATE).unwrap();
        assert_eq!(s.windows.len(), 5);
        assert_eq!(s.window(200).unwrap().workspace, "#1:2");
        assert!(s.window(300).unwrap().focused);
        assert!(!s.window(100).unwrap().focused);
        assert_eq!(floating, vec![400]);
        assert!(s.workspaces[1].focused);
        assert_eq!(s.monitors, vec!["DISPLAY1"]);
    }

    fn with_state(r: Scripted) -> (Arc<Scripted>, Komorebi) {
        let r = Arc::new(r);
        let mut k = Komorebi::new(r.clone());
        k.state().unwrap();
        (r, k)
    }

    #[test]
    fn a_move_focuses_the_window_by_exe_then_moves_the_focused_window() {
        let (r, mut k) = with_state(
            Scripted::default()
                .reply("komorebic state", STATE)
                .reply("komorebic eager-focus Slack.exe", "")
                .reply("komorebic move-to-named-workspace code", ""),
        );
        k.run(&WmCommand::MoveToWorkspace { window: 200, workspace: "code".into() }).unwrap();
        assert_eq!(r.calls().len(), 3);
    }

    #[test]
    fn an_exe_with_two_windows_is_refused_rather_than_guessed() {
        let (_r, mut k) = with_state(Scripted::default().reply("komorebic state", STATE));
        // 301 is not focused and firefox.exe has two windows.
        let e = k.run(&WmCommand::Focus { window: 301 }).unwrap_err();
        assert!(e.to_string().contains("2 windows"));
    }

    #[test]
    fn an_index_named_workspace_uses_the_index_commands() {
        let (r, mut k) = with_state(
            Scripted::default()
                .reply("komorebic state", STATE)
                .reply("komorebic eager-focus Code.exe", "")
                .reply("komorebic move-to-monitor-workspace 0 1", ""),
        );
        k.run(&WmCommand::MoveToWorkspace { window: 100, workspace: "#1:2".into() }).unwrap();
        assert_eq!(r.calls().len(), 3);
    }

    #[test]
    fn floating_an_already_floating_window_runs_nothing() {
        let (r, mut k) = with_state(
            Scripted::default()
                .reply("komorebic state", STATE)
                .reply("komorebic eager-focus Spotify.exe", ""),
        );
        k.run(&WmCommand::SetLayout { window: 400, layout: WmLayout::Floating }).unwrap();
        assert_eq!(r.calls().len(), 2);
    }
}
