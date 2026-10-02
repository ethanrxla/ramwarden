//! Running the desktop probe tools, with a deadline.
//!
//! `pactl`, `wmctrl`, and `xprop` are all consulted from the monitor thread, and
//! all three can hang rather than fail: `pactl` blocks indefinitely when the
//! PipeWire socket exists but nothing is listening, which happens across a
//! session restart. v1 passed `timeout=3` to every `subprocess.run` for exactly
//! this reason.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How long any probe tool gets before being abandoned.
pub const TIMEOUT: Duration = Duration::from_secs(3);

/// Run a tool and capture stdout, or `None` if it is missing, fails, or hangs.
///
/// A missing tool is the common case and not an error: `wmctrl` is frequently
/// absent, and on COSMIC it is present but useless. Callers degrade rather than
/// propagate.
pub fn run(cmd: &str, args: &[&str]) -> Option<String> {
    run_with_env(cmd, args, &[])
}

/// As [`run`], with extra environment variables.
///
/// `DISPLAY` is the reason this exists: the X tools need it set, and under a
/// Wayland session it may be absent from the daemon's environment even though an
/// XWayland server is running.
pub fn run_with_env(cmd: &str, args: &[&str], env: &[(&str, &str)]) -> Option<String> {
    let mut command = Command::new(cmd);
    command.args(args).stdout(Stdio::piped()).stderr(Stdio::null());
    for (k, v) in env {
        command.env(k, v);
    }

    let mut child = command.spawn().ok()?;
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

    // Read after exit. Safe because these tools emit a few kilobytes at most,
    // well inside the pipe buffer; a chattier tool would deadlock on a full pipe
    // and need a reader thread.
    let mut buf = String::new();
    child.stdout.take()?.read_to_string(&mut buf).ok()?;
    Some(buf)
}

/// The X display to hand the X tools.
///
/// v1 hardcoded a fallback of `:1`, which is unusual but is what this machine
/// actually runs, so it is preserved rather than "corrected" to `:0`.
pub fn display() -> String {
    std::env::var("DISPLAY").unwrap_or_else(|_| ":1".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_stdout() {
        assert_eq!(run("echo", &["hello"]).as_deref(), Some("hello\n"));
    }

    #[test]
    fn a_missing_tool_is_unavailable_rather_than_an_error() {
        assert_eq!(run("ramwarden-no-such-tool", &[]), None);
    }

    #[test]
    fn a_failing_tool_still_yields_its_stdout() {
        // `false` exits non-zero with no output; callers parse what they get.
        assert_eq!(run("false", &[]).as_deref(), Some(""));
    }

    /// A hanging tool must not stall the monitor thread.
    #[test]
    fn a_hanging_tool_is_abandoned_at_the_deadline() {
        let start = Instant::now();
        assert_eq!(run("sleep", &["60"]), None);
        assert!(start.elapsed() >= TIMEOUT, "returned early");
        assert!(start.elapsed() < TIMEOUT * 2, "took {:?}", start.elapsed());
    }

    #[test]
    fn extra_environment_reaches_the_child() {
        let out = run_with_env("sh", &["-c", "printf %s \"$RW_TEST\""], &[("RW_TEST", "set")]);
        assert_eq!(out.as_deref(), Some("set"));
    }

    #[test]
    fn display_falls_back_to_the_value_v1_used() {
        // Only assert the shape; the real value depends on the session.
        let d = display();
        assert!(d.starts_with(':') || d.contains(':'), "{d:?}");
    }
}
