//! Running a window manager's CLI with a deadline, keeping what it said.
//!
//! `ramwarden_core::exec::run` exists, but it collapses every failure into
//! `None` and drops stderr. Here the error text is the point: when
//! `aerospace move-node-to-workspace` refuses, the user is shown why. It also
//! reads the pipes on threads, because `swaymsg -t get_tree` on a busy desktop
//! is larger than a pipe buffer and would deadlock a read-after-exit.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use ramwarden_platform::{Error, Result};

/// How long any window manager command gets. AeroSpace answers in
/// milliseconds; one that takes this long is wedged.
pub const TIMEOUT: Duration = Duration::from_secs(5);

/// Runs a program and returns its stdout. Abstracted so backends are tested on
/// recorded output without spawning anything.
pub trait Runner: Send + Sync {
    fn run(&self, program: &str, args: &[String]) -> Result<String>;
}

/// The real thing.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemRunner;

impl Runner for SystemRunner {
    fn run(&self, program: &str, args: &[String]) -> Result<String> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    Error::Unsupported("this window manager's CLI")
                } else {
                    Error::os(format!("{program}: {e}"))
                }
            })?;

        let drain = |pipe: Option<Box<dyn Read + Send>>| {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                if let Some(mut p) = pipe {
                    let _ = p.read_to_end(&mut buf);
                }
                String::from_utf8_lossy(&buf).into_owned()
            })
        };
        let out = drain(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
        let err = drain(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));

        let deadline = Instant::now() + TIMEOUT;
        let status = loop {
            match child.try_wait() {
                Ok(Some(s)) => break s,
                Ok(None) if Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(Error::os(format!("{program} did not answer within {TIMEOUT:?}")));
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                Err(e) => return Err(Error::os(format!("{program}: {e}"))),
            }
        };

        let stdout = out.join().unwrap_or_default();
        let stderr = err.join().unwrap_or_default();
        if status.success() {
            Ok(stdout)
        } else {
            let said = if stderr.trim().is_empty() { stdout.trim() } else { stderr.trim() };
            Err(Error::os(format!(
                "{program} {}: {}",
                args.first().map(String::as_str).unwrap_or(""),
                if said.is_empty() { "failed" } else { said }
            )))
        }
    }
}

/// argv from string literals.
pub(crate) fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

/// The user's home directory, or `.` when there is none.
pub(crate) fn home() -> std::path::PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(Into::into)
        .unwrap_or_else(|| ".".into())
}

/// Recorded answers, for tests: each call pops the next `(expected argv, reply)`.
#[cfg(test)]
pub(crate) mod scripted {
    use std::sync::Mutex;

    use super::*;

    #[derive(Default)]
    pub struct Scripted {
        pub replies: Mutex<std::collections::VecDeque<(String, Result<String>)>>,
        pub calls: Mutex<Vec<String>>,
    }

    impl Scripted {
        pub fn reply(self, argv_prefix: &str, out: &str) -> Self {
            self.replies.lock().unwrap().push_back((argv_prefix.into(), Ok(out.into())));
            self
        }
        pub fn fail(self, argv_prefix: &str, err: &str) -> Self {
            self.replies.lock().unwrap().push_back((argv_prefix.into(), Err(Error::os(err))));
            self
        }
        pub fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl Runner for Scripted {
        fn run(&self, program: &str, args: &[String]) -> Result<String> {
            let line = std::iter::once(program.to_string())
                .chain(args.iter().cloned())
                .collect::<Vec<_>>()
                .join(" ");
            self.calls.lock().unwrap().push(line.clone());
            let (want, reply) = self
                .replies
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| (String::new(), Err(Error::Unsupported("no scripted reply"))));
            assert!(line.starts_with(&want), "expected `{want}…`, ran `{line}`");
            reply
        }
    }
}
