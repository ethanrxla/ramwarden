//! Signals that live outside `/proc`: audio streams, windows, and focus.
//!
//! These come from `pactl`, `wmctrl`, and `xprop` — subprocesses, which is why
//! they are collected on a slower cadence than the rest of a tick and cached
//! between samples. The parsing is split from the spawning so it can be tested
//! without running anything.
//!
//! # These are positive signals only
//!
//! On COSMIC — a Wayland compositor — `wmctrl` sees XWayland clients and nothing
//! else. A native app therefore reports no window at all. So window presence is
//! evidence of use, while window *absence* is evidence of nothing, and
//! [`crate::signals::Signals::score`] is careful never to read it the other way.
//! The same caution applies to focus: on COSMIC there is frequently no answer.

use std::collections::HashSet;
use std::process::Command;
use std::time::{Duration, Instant};

use crate::detector::Desktop;

/// How long audio stream data stays fresh. `pactl` is a subprocess; running it
/// every tick would cost more than the signal is worth.
pub const AUDIO_REFRESH: Duration = Duration::from_secs(15);

/// How long window and focus data stays fresh.
pub const WINDOW_REFRESH: Duration = Duration::from_secs(5);

const TIMEOUT: Duration = Duration::from_secs(3);

/// Caches the subprocess-sourced signals so each is refreshed on its own clock.
pub struct DesktopProbe {
    audio: HashSet<i32>,
    audio_at: Option<Instant>,
    windows: HashSet<i32>,
    focused: Option<i32>,
    windows_at: Option<Instant>,
}

impl Default for DesktopProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl DesktopProbe {
    pub fn new() -> Self {
        DesktopProbe {
            audio: HashSet::new(),
            audio_at: None,
            windows: HashSet::new(),
            focused: None,
            windows_at: None,
        }
    }

    /// Current desktop signals, refreshing whichever caches have gone stale.
    pub fn sample(&mut self) -> Desktop {
        let stale = |at: Option<Instant>, ttl: Duration| at.is_none_or(|t| t.elapsed() > ttl);

        if stale(self.audio_at, AUDIO_REFRESH) {
            self.audio = audio_pids();
            self.audio_at = Some(Instant::now());
        }
        if stale(self.windows_at, WINDOW_REFRESH) {
            let (pids, focused) = window_pids();
            self.windows = pids;
            self.focused = focused;
            self.windows_at = Some(Instant::now());
        }

        Desktop {
            audio_pids: self.audio.clone(),
            window_pids: self.windows.clone(),
            focused_pid: self.focused,
        }
    }
}

/// Run a probe tool, giving up after [`TIMEOUT`].
///
/// The timeout is not defensive padding. These run on the monitor thread, and
/// `pactl` in particular blocks indefinitely when the PipeWire socket exists but
/// nothing is listening — which happens across a session restart. v1 passed
/// `timeout=3` to `subprocess.run` for exactly this reason, and dropping it
/// would turn a stalled audio daemon into a stalled RamWarden.
fn run(cmd: &str, args: &[&str]) -> Option<String> {
    use std::process::Stdio;

    let mut child = Command::new(cmd)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    let deadline = Instant::now() + TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                tracing::debug!("{cmd} timed out after {TIMEOUT:?}; treating as unavailable");
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(_) => return None,
        }
    }

    // Read only after exit. Safe because these tools emit a few kilobytes at
    // most, well inside the pipe buffer; a chattier tool would need a reader
    // thread to avoid deadlocking on a full pipe.
    let mut buf = String::new();
    use std::io::Read;
    child.stdout.take()?.read_to_string(&mut buf).ok()?;
    Some(buf)
}

/// PIDs with an audio stream, playing or recording.
pub fn audio_pids() -> HashSet<i32> {
    let mut pids = HashSet::new();
    for kind in ["sink-inputs", "source-outputs"] {
        if let Some(out) = run("pactl", &["list", kind]) {
            pids.extend(parse_pactl(&out));
        }
    }
    pids
}

/// Extract `application.process.id = "1234"` values.
pub fn parse_pactl(text: &str) -> Vec<i32> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("application.process.id") else {
            continue;
        };
        let Some((_, value)) = rest.split_once('=') else {
            continue;
        };
        if let Ok(pid) = value.trim().trim_matches('"').parse::<i32>() {
            out.push(pid);
        }
    }
    out
}

/// PIDs owning a mapped window, plus the focused window's PID.
pub fn window_pids() -> (HashSet<i32>, Option<i32>) {
    let Some(list) = run("wmctrl", &["-lp"]) else {
        return (HashSet::new(), None);
    };
    let windows = parse_wmctrl(&list);
    let pids: HashSet<i32> = windows.iter().map(|(_, pid)| *pid).collect();

    let focused = run("xprop", &["-root", "_NET_ACTIVE_WINDOW"])
        .and_then(|out| parse_active_window(&out))
        .and_then(|id| windows.iter().find(|(w, _)| *w == id).map(|(_, p)| *p));

    (pids, focused)
}

/// Parse `wmctrl -lp` into (window id, pid) pairs.
///
/// Flatpak and other sandboxed clients report a PID from inside their own
/// namespace, which means nothing on this host — those are filtered by the
/// caller matching them against the real process table.
pub fn parse_wmctrl(text: &str) -> Vec<(u64, i32)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 3 {
            continue;
        }
        let Ok(win) = u64::from_str_radix(f[0].trim_start_matches("0x"), 16) else {
            continue;
        };
        let Ok(pid) = f[2].parse::<i32>() else {
            continue;
        };
        if pid > 1 {
            out.push((win, pid));
        }
    }
    out
}

/// Pull the window id out of `xprop -root _NET_ACTIVE_WINDOW`.
pub fn parse_active_window(text: &str) -> Option<u64> {
    let hex = text.split("0x").nth(1)?;
    let digits: String = hex.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
    u64::from_str_radix(&digits, 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PACTL: &str = r#"
Sink Input #1234
    Driver: PipeWire
    Properties:
        application.name = "Brave"
        application.process.id = "6669"
        application.process.binary = "brave"
Sink Input #1235
    Properties:
        application.name = "mpv"
        application.process.id = "98765"
"#;

    const WMCTRL: &str = "\
0x01000003  0 1234   hostname  Terminal
0x01200005  1 5678   hostname  Brave Browser - some page
0x01400007  0 0      hostname  a window with no usable pid
0x01600009 -1 91011  hostname  Sticky window
";

    #[test]
    fn parses_pactl_process_ids() {
        assert_eq!(parse_pactl(PACTL), vec![6669, 98765]);
    }

    #[test]
    fn pactl_output_with_no_streams_yields_nothing() {
        assert!(parse_pactl("").is_empty());
        assert!(parse_pactl("Failure: Connection refused\n").is_empty());
    }

    #[test]
    fn a_malformed_pactl_line_is_skipped() {
        assert!(parse_pactl("        application.process.id = \"notanumber\"\n").is_empty());
        assert!(parse_pactl("        application.process.id\n").is_empty());
    }

    #[test]
    fn parses_wmctrl_window_and_pid_pairs() {
        let w = parse_wmctrl(WMCTRL);
        assert_eq!(w, vec![(0x01000003, 1234), (0x01200005, 5678), (0x01600009, 91011)]);
    }

    #[test]
    fn wmctrl_rows_without_a_usable_pid_are_dropped() {
        // pid 0 is the "no pid reported" case, common for sandboxed clients.
        assert!(!parse_wmctrl(WMCTRL).iter().any(|(_, p)| *p <= 1));
    }

    #[test]
    fn wmctrl_output_that_failed_yields_nothing() {
        // wmctrl on COSMIC frequently errors out entirely.
        assert!(parse_wmctrl("").is_empty());
        assert!(parse_wmctrl("Cannot get window list\n").is_empty());
    }

    #[test]
    fn parses_the_active_window_id() {
        let out = "_NET_ACTIVE_WINDOW(WINDOW): window id # 0x1200005\n";
        assert_eq!(parse_active_window(out), Some(0x1200005));
    }

    #[test]
    fn an_absent_active_window_is_none_rather_than_a_guess() {
        assert_eq!(parse_active_window(""), None);
        // What xprop prints under a compositor that does not set the property.
        assert_eq!(
            parse_active_window("_NET_ACTIVE_WINDOW:  not found.\n"),
            None
        );
    }

    #[test]
    fn focus_resolves_through_the_window_list() {
        let windows = parse_wmctrl(WMCTRL);
        let id = parse_active_window("_NET_ACTIVE_WINDOW(WINDOW): window id # 0x1200005\n").unwrap();
        let pid = windows.iter().find(|(w, _)| *w == id).map(|(_, p)| *p);
        assert_eq!(pid, Some(5678));
    }

    #[test]
    fn a_focused_window_not_in_the_list_yields_no_focused_pid() {
        let windows = parse_wmctrl(WMCTRL);
        let id = parse_active_window("window id # 0xdeadbeef\n").unwrap();
        assert!(!windows.iter().any(|(w, _)| *w == id));
    }

    #[test]
    fn a_tool_that_does_not_exist_is_simply_unavailable() {
        assert_eq!(run("ramwarden-no-such-tool", &[]), None);
    }

    /// A tool that never returns must not stall the tick.
    #[test]
    fn a_hanging_tool_is_abandoned_at_the_timeout() {
        let start = Instant::now();
        let out = run("sleep", &["60"]);
        assert_eq!(out, None, "a timed-out probe yields no data");
        assert!(start.elapsed() < TIMEOUT * 2, "took {:?}", start.elapsed());
        assert!(start.elapsed() >= TIMEOUT, "returned before the deadline");
    }

    /// The probe must survive a desktop where none of the three tools work,
    /// which is the realistic COSMIC case for `wmctrl` and `xprop`.
    #[test]
    fn the_probe_works_when_every_tool_is_missing() {
        let mut p = DesktopProbe::new();
        let d = p.sample();
        // Whatever this machine reports, sampling must not panic and must be
        // internally consistent.
        if let Some(f) = d.focused_pid {
            assert!(f > 1);
        }
        // A second sample inside the TTL must not re-run the subprocesses.
        let before = p.audio_at;
        let _ = p.sample();
        assert_eq!(before, p.audio_at, "cached within the refresh window");
    }
}
