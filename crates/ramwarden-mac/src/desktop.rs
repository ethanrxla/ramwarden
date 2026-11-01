use std::collections::HashSet;
use std::time::{Duration, Instant};

use ramwarden_platform::{Desktop, DesktopSource};

/// Desktop signals from `lsappinfo` and `pmset`, which need no permissions
/// (unlike AppleScript/System Events, which would prompt a launchd daemon).
///
/// Positive evidence only, like every platform: `window_pids` is "regular apps
/// in the Dock", a superset of apps with a visible window, so it errs toward
/// protecting an app, never toward unloading one.
pub struct MacDesktop {
    /// Audio and Dock-app sets: two heavier subprocesses, refreshed every 5 s.
    slow: Option<(Instant, HashSet<i32>, HashSet<i32>)>,
    /// The frontmost app, refreshed every second so an app the user clicks is
    /// thawed within about a second.
    focus: Option<(Instant, Option<i32>)>,
}

impl MacDesktop {
    pub fn new() -> Self {
        MacDesktop { slow: None, focus: None }
    }
}

impl Default for MacDesktop {
    fn default() -> Self {
        Self::new()
    }
}

/// A probe that answers within a couple of seconds or is treated as unavailable.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

fn run(cmd: &str, args: &[&str]) -> Option<String> {
    crate::sys::run_timeout(cmd, args, PROBE_TIMEOUT)
}

/// pids of `type="Foreground"` apps in `lsappinfo list`.
pub fn foreground_pids(list: &str) -> HashSet<i32> {
    list.lines()
        .filter(|l| l.contains("type=\"Foreground\""))
        .filter_map(|l| l.split("pid =").nth(1)?.split_whitespace().next()?.parse().ok())
        .collect()
}

/// pids holding an audio/camera/microphone assertion in `pmset -g assertions`.
/// Prefers the "Created for PID" owner over the daemon (coreaudiod) holding it.
pub fn audio_pids(assertions: &str) -> HashSet<i32> {
    let mut out = HashSet::new();
    let mut current: Option<i32> = None;
    let mut audio = false;
    let flush = |cur: &mut Option<i32>, audio: &mut bool, out: &mut HashSet<i32>| {
        if let (Some(p), true) = (*cur, *audio) {
            out.insert(p);
        }
        *cur = None;
        *audio = false;
    };
    for line in assertions.lines() {
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix("pid ") {
            flush(&mut current, &mut audio, &mut out);
            current = rest.split('(').next().and_then(|p| p.parse().ok());
            let l = line.to_lowercase();
            audio = l.contains("audio") || l.contains("camera") || l.contains("microphone");
        } else if let Some(p) = t.split("Created for PID:").nth(1)
            && let Ok(p) = p.trim().trim_end_matches('.').parse()
        {
            current = Some(p);
        }
    }
    flush(&mut current, &mut audio, &mut out);
    out
}

fn frontmost_pid() -> Option<i32> {
    let front = run("lsappinfo", &["front"])?;
    run("lsappinfo", &["info", "-only", "pid", front.trim()])?.split('=').nth(1)?.trim().parse().ok()
}

impl DesktopSource for MacDesktop {
    fn sample(&mut self) -> Desktop {
        if self.slow.as_ref().is_none_or(|(at, ..)| at.elapsed() >= Duration::from_secs(5)) {
            // A probe that times out keeps the last good answer instead of
            // reporting "nothing is playing / no windows".
            let (old_audio, old_windows) = self
                .slow
                .take()
                .map(|(_, a, w)| (a, w))
                .unwrap_or_default();
            let audio = run("pmset", &["-g", "assertions"]).map(|o| audio_pids(&o)).unwrap_or(old_audio);
            let windows = run("lsappinfo", &["list"]).map(|o| foreground_pids(&o)).unwrap_or(old_windows);
            self.slow = Some((Instant::now(), audio, windows));
        }
        if self.focus.as_ref().is_none_or(|(at, _)| at.elapsed() >= Duration::from_secs(1)) {
            let last = self.focus.as_ref().and_then(|(_, p)| *p);
            self.focus = Some((Instant::now(), frontmost_pid().or(last)));
        }
        let (_, audio, windows) = self.slow.as_ref().expect("set above");
        Desktop {
            audio_pids: audio.clone(),
            window_pids: windows.clone(),
            focused_pid: self.focus.as_ref().and_then(|(_, p)| *p),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_foreground_apps() {
        let s = "    pid = 405 type=\"UIElement\" flavor=3\n    pid = 2447 type=\"Foreground\" flavor=3\n";
        assert_eq!(foreground_pids(s), HashSet::from([2447]));
    }

    #[test]
    fn parses_audio_owner() {
        let s = "   pid 61(coreaudiod): [0x1] 00:00:01 PreventUserIdleSystemSleep named: \"com.apple.audio.context\"\n\tCreated for PID: 777.\n   pid 343(powerd): [0x2] 00:15:18 UserIsActive named: \"x\"\n";
        assert_eq!(audio_pids(s), HashSet::from([777]));
    }

}
