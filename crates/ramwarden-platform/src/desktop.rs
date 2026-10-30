use std::collections::HashSet;

/// Signals that do not come from the process table: audio, windows, focus.
///
/// Positive evidence only. On Linux under Wayland, `wmctrl` sees XWayland
/// clients and nothing else, so an absent window proves nothing — which is why
/// the scorer never treats absence as evidence. Windows reports every top-level
/// window, but the rule stays the same on both so policy reads identically.
#[derive(Clone, Debug, Default)]
pub struct Desktop {
    /// PIDs with an active audio stream, playing or recording.
    pub audio_pids: HashSet<i32>,
    /// PIDs owning a visible top-level window.
    pub window_pids: HashSet<i32>,
    /// PID owning the focused window, if known.
    pub focused_pid: Option<i32>,
}

/// Collects [`Desktop`] signals, caching whatever is expensive to ask for.
pub trait DesktopSource: Send {
    fn sample(&mut self) -> Desktop;
}
