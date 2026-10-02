//! Interactive shells, and which of them are genuinely empty.
//!
//! # The two bugs this carries forward
//!
//! **Subshells are not terminals.** Docker and shell scripts spawn `sh`
//! processes that inherit the parent shell's controlling terminal. v1 closed
//! them, because by every obvious test they look like idle shells. The fix is
//! the session-leader check: a shell the *user* opened has `session == pid`,
//! while an inherited subshell shares its parent's session id. The regression
//! that taught v1 this is reproduced as a test below, PIDs and all.
//!
//! **An interactive bash ignores SIGTERM.** It is a session leader with a
//! controlling terminal, so the signal is discarded and the shell carries on.
//! v1 originally sent SIGTERM, appended the PID to its results, and reported
//! success — so RamWarden claimed to close the same six shells on every run
//! while all six stayed alive. `SIGHUP` is what a closing terminal actually
//! sends. Nothing is reported closed until the process is confirmed gone.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use ramwarden_kernel::procfd::PidFd;
use ramwarden_kernel::{Root, process};

/// Interactive shells RamWarden recognises.
const SHELLS: &[&str] = &["bash", "zsh", "fish", "sh", "dash", "ksh"];

/// How long a shell gets to act on SIGHUP before being killed.
pub const HANGUP_GRACE: Duration = Duration::from_secs(2);

/// One interactive shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Terminal {
    pub pid: i32,
    pub shell: String,
    pub ppid: i32,
    /// No descendant processes at all.
    pub is_idle: bool,
    /// Names of everything running under this shell. Shown to the user, and to
    /// the model, as the reason a busy shell is off-limits.
    pub children: Vec<String>,
    pub rss: u64,
}

impl Terminal {
    /// A one-line description of why this shell is or is not closeable.
    pub fn reason(&self) -> String {
        if self.is_idle {
            "nothing running — safe to close".to_string()
        } else {
            let shown: Vec<&str> = self.children.iter().take(3).map(String::as_str).collect();
            format!("running {}", shown.join(", "))
        }
    }
}

/// Every interactive shell the user opened.
///
/// Reads the process table once and derives everything from it, so there is no
/// window in which the parent map and the shell list disagree.
pub fn list(root: &Root) -> ramwarden_kernel::Result<Vec<Terminal>> {
    let table = process::table(root)?;

    let mut children_of: HashMap<i32, Vec<i32>> = HashMap::new();
    let mut by_pid: HashMap<i32, &process::ProcEntry> = HashMap::new();
    for p in &table {
        children_of.entry(p.ppid).or_default().push(p.pid);
        by_pid.insert(p.pid, p);
    }

    let mut out = Vec::new();
    for p in &table {
        if !SHELLS.contains(&p.comm.to_lowercase().as_str()) {
            continue;
        }
        // Not interactive: no controlling terminal.
        if !p.has_tty() {
            continue;
        }
        // Not the user's: a subshell that inherited its parent's session.
        if p.session != p.pid {
            continue;
        }

        let names = descendant_names(p.pid, &children_of, &by_pid);
        out.push(Terminal {
            pid: p.pid,
            shell: p.comm.clone(),
            ppid: p.ppid,
            is_idle: names.is_empty(),
            children: names,
            rss: p.rss_bytes(),
        });
    }
    out.sort_by_key(|t| t.pid);
    Ok(out)
}

/// Names of every descendant of `pid`, breadth-first.
fn descendant_names(
    pid: i32,
    children_of: &HashMap<i32, Vec<i32>>,
    by_pid: &HashMap<i32, &process::ProcEntry>,
) -> Vec<String> {
    let mut out = Vec::new();
    let mut queue = vec![pid];
    let mut seen: HashSet<i32> = HashSet::from([pid]);

    while let Some(cur) = queue.pop() {
        for &child in children_of.get(&cur).map(Vec::as_slice).unwrap_or(&[]) {
            if !seen.insert(child) {
                continue; // guards against a cycle in a racing /proc read
            }
            if let Some(p) = by_pid.get(&child) {
                out.push(p.comm.clone());
            }
            queue.push(child);
        }
    }
    out
}

/// A human summary, as v1 gave the model.
pub fn summary(terminals: &[Terminal]) -> String {
    let idle: Vec<&Terminal> = terminals.iter().filter(|t| t.is_idle).collect();
    let busy = terminals.len() - idle.len();

    let mut parts = Vec::new();
    if !idle.is_empty() {
        let pids: Vec<String> = idle.iter().map(|t| t.pid.to_string()).collect();
        parts.push(format!(
            "{} idle terminal(s) (PIDs: {})",
            idle.len(),
            pids.join(", ")
        ));
    }
    if busy > 0 {
        parts.push(format!(
            "{busy} busy terminal(s) with active processes — do NOT auto-close"
        ));
    }
    if parts.is_empty() {
        return "no interactive terminals found".to_string();
    }
    parts.join("; ")
}

/// Close idle shells, returning only the PIDs confirmed gone.
///
/// Busy shells are never signalled, whatever the caller passed in — the check is
/// repeated here rather than trusted, because this is the last point before a
/// signal leaves the process.
pub fn close_idle(terminals: &[Terminal]) -> Vec<i32> {
    let mut pending = Vec::new();

    for t in terminals.iter().filter(|t| t.is_idle) {
        // Open the handle first: the shell may have exited since it was listed,
        // and signalling by PID could reach whoever inherited the number.
        let Ok(fd) = PidFd::open(t.pid) else {
            tracing::debug!("shell {} exited before it could be closed", t.pid);
            continue;
        };
        match fd.hangup() {
            Ok(()) => pending.push(fd),
            Err(e) => tracing::warn!("could not hang up on PID {}: {e}", t.pid),
        }
    }

    // Wait on all of them against one shared deadline rather than serially.
    let deadline = Instant::now() + HANGUP_GRACE;
    let mut survivors = Vec::new();
    let mut closed = Vec::new();

    for fd in pending {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if fd.wait_exit(remaining).unwrap_or(false) {
            closed.push(fd.pid());
        } else {
            survivors.push(fd);
        }
    }

    for fd in survivors {
        tracing::info!("PID {} ignored SIGHUP — escalating to SIGKILL", fd.pid());
        if fd.kill().is_err() {
            continue;
        }
        if fd.wait_exit(HANGUP_GRACE).unwrap_or(false) {
            closed.push(fd.pid());
        } else {
            // Unkillable means stuck in uninterruptible sleep. Reporting it
            // closed is the exact lie v1 used to tell.
            tracing::warn!("PID {} survived SIGKILL — not reporting it closed", fd.pid());
        }
    }

    closed.sort_unstable();
    closed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Write a synthetic `/proc/<pid>/stat`, so the session-leader and tty logic
    /// is exercised against the same field layout the kernel produces.
    struct ProcFixture {
        _dir: tempfile::TempDir,
        root: Root,
    }

    impl ProcFixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let root = Root::at(dir.path());
            fs::create_dir_all(root.join("proc")).unwrap();
            fs::write(root.join("proc/stat"), "btime 1790097664\n").unwrap();
            ProcFixture { _dir: dir, root }
        }

        /// `tty` 0 means no controlling terminal; `session` is the session id.
        fn add(&self, pid: i32, comm: &str, ppid: i32, session: i32, tty: i32) -> &Self {
            let d = self.root.join(format!("proc/{pid}"));
            fs::create_dir_all(&d).unwrap();
            // fields: pid (comm) state ppid pgrp session tty_nr ... utime stime
            // ... starttime vsize rss
            let stat = format!(
                "{pid} ({comm}) S {ppid} {session} {session} {tty} -1 4194304 \
                 0 0 0 0 10 5 0 0 20 0 1 0 1000 1000000 2560 \
                 18446744073709551615 0 0 0 0 0 0 0 0 0 0 0 0 17 0 0 0 0 0 0 0 0 0 0 0 0 0"
            );
            fs::write(d.join("stat"), stat).unwrap();
            self
        }

        fn list(&self) -> Vec<Terminal> {
            list(&self.root).unwrap()
        }
    }

    /// v1's `test_session_leader_shells_included`.
    #[test]
    fn a_shell_the_user_opened_is_listed() {
        let f = ProcFixture::new();
        f.add(1000, "bash", 500, 1000, 34817);
        let t = f.list();
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].pid, 1000);
        assert_eq!(t[0].shell, "bash");
        assert!(t[0].is_idle);
    }

    /// v1's `test_subshell_not_included`.
    #[test]
    fn a_subshell_sharing_its_parents_session_is_not_listed() {
        let f = ProcFixture::new();
        // sh with pid 9999 but session 1000 — inherited, not opened by the user.
        f.add(9999, "sh", 1000, 1000, 34817);
        assert!(f.list().is_empty());
    }

    /// v1's `test_docker_sh_subshell_not_closed`, with the production PIDs:
    /// antigravity -> bash (SID 1112824) -> sg -> sh (SID 1112824).
    #[test]
    fn the_docker_subshell_regression_stays_fixed() {
        let f = ProcFixture::new();
        f.add(1112824, "bash", 500, 1112824, 34828);
        f.add(1113046, "sh", 1112824, 1112824, 34828);

        let pids: Vec<i32> = f.list().iter().map(|t| t.pid).collect();
        assert!(pids.contains(&1112824), "the user's bash must be listed");
        assert!(
            !pids.contains(&1113046),
            "the Docker sh subshell must not be listed"
        );
    }

    /// v1's `test_shell_without_tty_excluded`.
    #[test]
    fn a_shell_with_no_controlling_terminal_is_not_interactive() {
        let f = ProcFixture::new();
        f.add(4000, "bash", 1, 4000, 0);
        assert!(f.list().is_empty());
    }

    /// v1's `test_idle_shell_has_no_children`.
    #[test]
    fn a_shell_with_no_children_is_idle() {
        let f = ProcFixture::new();
        f.add(2000, "bash", 500, 2000, 34817);
        assert!(f.list()[0].is_idle);
        assert!(f.list()[0].children.is_empty());
    }

    /// v1's `test_busy_shell_has_children`.
    #[test]
    fn a_shell_running_something_is_busy() {
        let f = ProcFixture::new();
        f.add(2001, "bash", 500, 2001, 34818);
        f.add(2002, "python3", 2001, 2001, 34818);

        let t = f.list();
        assert_eq!(t.len(), 1, "python3 is not a shell");
        assert!(!t[0].is_idle);
        assert_eq!(t[0].children, vec!["python3"]);
        assert!(t[0].reason().contains("running python3"));
    }

    #[test]
    fn a_shell_is_busy_when_a_grandchild_is_working() {
        let f = ProcFixture::new();
        f.add(3000, "bash", 500, 3000, 34817);
        f.add(3001, "make", 3000, 3000, 34817);
        f.add(3002, "cc1plus", 3001, 3000, 34817);

        let t = f.list();
        assert!(!t[0].is_idle);
        assert_eq!(t[0].children.len(), 2, "{:?}", t[0].children);
        assert!(t[0].children.contains(&"cc1plus".to_string()));
    }

    /// An agent session is exactly what must never be hung up on.
    #[test]
    fn a_shell_holding_an_agent_is_busy() {
        let f = ProcFixture::new();
        f.add(5000, "bash", 500, 5000, 34817);
        f.add(5001, "claude", 5000, 5000, 34817);
        assert!(!f.list()[0].is_idle);
    }

    #[test]
    fn several_shells_are_listed_in_pid_order() {
        let f = ProcFixture::new();
        f.add(900, "zsh", 1, 900, 34820);
        f.add(100, "bash", 1, 100, 34817);
        f.add(500, "fish", 1, 500, 34819);
        let pids: Vec<i32> = f.list().iter().map(|t| t.pid).collect();
        assert_eq!(pids, vec![100, 500, 900]);
    }

    #[test]
    fn a_non_shell_process_with_a_tty_is_not_a_terminal() {
        let f = ProcFixture::new();
        f.add(6000, "vim", 1, 6000, 34817);
        f.add(6001, "htop", 1, 6001, 34818);
        assert!(f.list().is_empty());
    }

    #[test]
    fn shell_detection_is_case_insensitive() {
        let f = ProcFixture::new();
        f.add(7000, "BASH", 1, 7000, 34817);
        assert_eq!(f.list().len(), 1);
    }

    #[test]
    fn a_cycle_in_the_parent_map_does_not_hang_the_walk() {
        let f = ProcFixture::new();
        f.add(8000, "bash", 8001, 8000, 34817);
        f.add(8001, "weird", 8000, 8000, 34817);
        // Terminates, and does not double-count.
        let t = f.list();
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].children, vec!["weird"]);
    }

    // ── Summary ─────────────────────────────────────────────────────────────

    #[test]
    fn the_summary_names_idle_pids_and_warns_about_busy_ones() {
        let f = ProcFixture::new();
        f.add(100, "bash", 1, 100, 34817);
        f.add(200, "bash", 1, 200, 34818);
        f.add(201, "vim", 200, 200, 34818);

        let s = summary(&f.list());
        assert!(s.contains("1 idle terminal(s) (PIDs: 100)"), "{s}");
        assert!(s.contains("1 busy terminal(s)"), "{s}");
        assert!(s.contains("do NOT auto-close"), "{s}");
    }

    #[test]
    fn the_summary_says_so_when_there_are_no_terminals() {
        assert_eq!(summary(&[]), "no interactive terminals found");
    }

    // ── Closing, against real processes ─────────────────────────────────────

    /// v1's `test_close_idle_terminals_skips_busy`, but with real processes:
    /// the busy shell must not even be signalled.
    #[test]
    fn closing_skips_busy_shells_entirely() {
        let mut idle = std::process::Command::new("sleep").arg("60").spawn().unwrap();
        let mut busy = std::process::Command::new("sleep").arg("60").spawn().unwrap();
        let (idle_pid, busy_pid) = (idle.id() as i32, busy.id() as i32);

        let terms = vec![
            Terminal {
                pid: idle_pid,
                shell: "bash".into(),
                ppid: 1,
                is_idle: true,
                children: vec![],
                rss: 0,
            },
            Terminal {
                pid: busy_pid,
                shell: "bash".into(),
                ppid: 1,
                is_idle: false,
                children: vec!["vim".into()],
                rss: 0,
            },
        ];

        let closed = close_idle(&terms);
        assert_eq!(closed, vec![idle_pid]);
        assert!(
            PidFd::open(busy_pid).unwrap().is_alive(),
            "a busy shell must never be signalled"
        );

        idle.wait().ok();
        busy.kill().ok();
        busy.wait().ok();
    }

    /// v1's `test_shell_that_survives_is_not_reported_closed`. `sleep` ignores
    /// nothing, so this uses a shell that traps SIGHUP to stand in for an
    /// interactive bash discarding it.
    #[test]
    fn a_shell_that_ignores_hangup_is_escalated_and_only_then_reported() {
        let mut child = std::process::Command::new("sh")
            .arg("-c")
            .arg("trap '' HUP; sleep 60")
            .spawn()
            .unwrap();
        let pid = child.id() as i32;

        // Wait for the trap to be installed, or SIGHUP lands on a shell still
        // using the default disposition and the escalation path is never taken.
        let installed = (0..200).any(|_| {
            let ignored = std::fs::read_to_string(format!("/proc/{pid}/status"))
                .ok()
                .and_then(|s| {
                    s.lines()
                        .find(|l| l.starts_with("SigIgn:"))?
                        .split_whitespace()
                        .nth(1)
                        .and_then(|h| u64::from_str_radix(h, 16).ok())
                })
                .is_some_and(|bits| bits & (1 << 0) != 0); // SIGHUP is signal 1
            if !ignored {
                std::thread::sleep(Duration::from_millis(10));
            }
            ignored
        });
        assert!(installed, "shell never installed the SIGHUP trap");

        let terms = vec![Terminal {
            pid,
            shell: "sh".into(),
            ppid: 1,
            is_idle: true,
            children: vec![],
            rss: 0,
        }];

        let closed = close_idle(&terms);
        assert_eq!(closed, vec![pid], "escalation must still confirm the close");
        child.wait().ok();
        assert!(!PidFd::open(pid).map(|f| f.is_alive()).unwrap_or(false));
    }

    #[test]
    fn closing_nothing_signals_nothing() {
        assert!(close_idle(&[]).is_empty());
    }

    #[test]
    fn a_shell_that_exited_before_being_closed_is_not_reported() {
        let mut child = std::process::Command::new("sleep").arg("60").spawn().unwrap();
        let pid = child.id() as i32;
        child.kill().unwrap();
        child.wait().unwrap();

        let terms = vec![Terminal {
            pid,
            shell: "bash".into(),
            ppid: 1,
            is_idle: true,
            children: vec![],
            rss: 0,
        }];
        assert!(close_idle(&terms).is_empty());
    }
}
