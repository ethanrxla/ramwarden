//! Tiling window managers: what is on the desktop, and how to rearrange it.
//!
//! The seam for `docs/specs/desktop-arrange.md`. Each OS has a different
//! window manager with a different CLI (AeroSpace on macOS, sway/i3 on Linux,
//! komorebi on Windows), but what a user asks for is the same everywhere:
//! "put Slack on workspace 3", "make this one floating". So the vocabulary here
//! is shaped after the request, and each backend translates it.
//!
//! The vocabulary is closed and deliberately non-destructive. There is no
//! close or quit: ending things to free memory belongs to the ladder, which is
//! bounded by the watchlist. A model that asks to close a window has asked for
//! something this type cannot express.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::Result;

/// Which window manager is running.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WmKind {
    AeroSpace,
    Sway,
    I3,
    Komorebi,
}

impl WmKind {
    pub fn as_str(self) -> &'static str {
        match self {
            WmKind::AeroSpace => "aerospace",
            WmKind::Sway => "sway",
            WmKind::I3 => "i3",
            WmKind::Komorebi => "komorebi",
        }
    }

    /// Config tokens that make the window manager start a program.
    ///
    /// A config edit that adds a line containing one of these is refused: a
    /// layout change must never become a way to run code.
    pub fn exec_markers(self) -> &'static [&'static str] {
        match self {
            // `exec-and-forget`, `exec-on-workspace-change`, `[exec]`.
            WmKind::AeroSpace => &["exec", "after-startup-command", "after-login-command"],
            // `exec`, `exec_always`, `bindsym … exec`, and the bar launchers.
            WmKind::Sway | WmKind::I3 => &["exec", "_command"],
            // komorebi.json launches nothing itself today; these catch any
            // future command hook conservatively.
            WmKind::Komorebi => &["exec", "command"],
        }
    }
}

/// One managed window.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WmWindow {
    /// The window manager's own id: AeroSpace window id, sway `con_id`,
    /// komorebi `hwnd`.
    pub id: u64,
    pub app: String,
    pub title: String,
    pub workspace: String,
    pub pid: Option<i32>,
    pub focused: bool,
}

/// One workspace.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WmWorkspace {
    pub name: String,
    pub monitor: Option<String>,
    pub focused: bool,
}

/// Everything a request needs to be planned against.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WmState {
    pub kind: WmKind,
    pub windows: Vec<WmWindow>,
    pub workspaces: Vec<WmWorkspace>,
    pub monitors: Vec<String>,
}

impl WmState {
    pub fn empty(kind: WmKind) -> Self {
        WmState {
            kind,
            windows: Vec::new(),
            workspaces: Vec::new(),
            monitors: Vec::new(),
        }
    }

    pub fn window(&self, id: u64) -> Option<&WmWindow> {
        self.windows.iter().find(|w| w.id == id)
    }
}

/// A window's layout. Backends map what they can and refuse the rest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WmLayout {
    Tiles,
    Accordion,
    Horizontal,
    Vertical,
    Floating,
    Tiling,
}

impl WmLayout {
    pub fn as_str(self) -> &'static str {
        match self {
            WmLayout::Tiles => "tiles",
            WmLayout::Accordion => "accordion",
            WmLayout::Horizontal => "horizontal",
            WmLayout::Vertical => "vertical",
            WmLayout::Floating => "floating",
            WmLayout::Tiling => "tiling",
        }
    }
}

/// One step of an arrangement. Serialised as `{"op": "move_to_workspace", …}`,
/// which is also the shape a model is asked to reply in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum WmCommand {
    Focus { window: u64 },
    MoveToWorkspace { window: u64, workspace: String },
    SwitchWorkspace { workspace: String },
    MoveWorkspaceToMonitor { workspace: String, monitor: String },
    SetLayout { window: u64, layout: WmLayout },
    Balance { workspace: String },
}

impl WmCommand {
    /// The window this command acts on, if any.
    pub fn window(&self) -> Option<u64> {
        match self {
            WmCommand::Focus { window }
            | WmCommand::MoveToWorkspace { window, .. }
            | WmCommand::SetLayout { window, .. } => Some(*window),
            _ => None,
        }
    }

    /// Every user-supplied name in this command, for validation.
    pub fn names(&self) -> Vec<&str> {
        match self {
            WmCommand::MoveToWorkspace { workspace, .. }
            | WmCommand::SwitchWorkspace { workspace }
            | WmCommand::Balance { workspace } => vec![workspace],
            WmCommand::MoveWorkspaceToMonitor { workspace, monitor } => vec![workspace, monitor],
            _ => Vec::new(),
        }
    }
}

/// A running tiling window manager.
///
/// Mechanism only: implementations translate and run, and never decide whether
/// a command should run. Calls are blocking subprocesses with a deadline.
pub trait WindowManager: Send {
    fn kind(&self) -> WmKind;

    /// Windows, workspaces and monitors right now.
    fn state(&mut self) -> Result<WmState>;

    /// Carry out one command. `Ok` means the CLI accepted it, not that it had
    /// an effect; the caller verifies against a fresh [`state`](Self::state).
    fn run(&mut self, cmd: &WmCommand) -> Result<()>;

    /// The config file the window manager is reading, if there is one.
    fn config_path(&self) -> Option<PathBuf>;

    /// Syntax-check config text without applying it.
    fn check_config(&self, text: &str) -> Result<()>;

    /// Re-read the config file on disk. An error means the file was rejected.
    fn reload_config(&mut self) -> Result<()>;
}
