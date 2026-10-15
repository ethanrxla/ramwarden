use std::collections::HashSet;
use std::process::Command;
use std::time::{Duration, Instant};

use ramwarden_platform::{Desktop, DesktopSource};

/// Desktop signals from `lsappinfo` and `pmset`, which need no permissions
/// (unlike AppleScript/System Events, which would prompt a launchd daemon).
///
/// Positive evidence only, like every platform: `window_pids` is "regular apps
/// in the Dock", a superset of apps with a visible window, so it errs toward
/// protecting an app, never toward unloading one.
pub struct MacDesktop {
    cached: Option<(Instant, Desktop)>,
}

impl MacDesktop {
    pub fn new() -> Self {
        MacDesktop { cached: None }
    }
}

impl Default for MacDesktop {
    fn default() -> Self {
        Self::new()
    }
}

fn run(cmd: &str, args: &[&str]) -> String {
    Command::new(cmd).args(args).output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
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
        } else if let Some(p) = t.split("Created for PID:").nth(1) {
            if let Ok(p) = p.trim().trim_end_matches('.').parse() {
                current = Some(p);
            }
        }
    }
    flush(&mut current, &mut audio, &mut out);
    out
}

impl DesktopSource for MacDesktop {
    fn sample(&mut self) -> Desktop {
        if let Some((at, d)) = &self.cached {
            if at.elapsed() < Duration::from_secs(5) {
                return d.clone();
            }
        }
        let front = run("lsappinfo", &["front"]);
        let focused_pid = run("lsappinfo", &["info", "-only", "pid", front.trim()])
            .split('=')
            .nth(1)
            .and_then(|p| p.trim().parse().ok());
        let d = Desktop {
            audio_pids: audio_pids(&run("pmset", &["-g", "assertions"])),
            window_pids: foreground_pids(&run("lsappinfo", &["list"])),
            focused_pid,
        };
        self.cached = Some((Instant::now(), d.clone()));
        d
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
