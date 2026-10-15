use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ramwarden_platform::{Error, Owner, ProcessControl, ProcessHandle, ProcessSample, ProcessSource, Result};

use crate::sys;

fn owner_of(uid: u32, me: u32) -> Owner {
    match uid {
        0 => Owner::System,
        u if u == me => Owner::CurrentUser,
        _ => Owner::OtherUser,
    }
}

/// Listening ports and established-connection counts per pid, from `lsof`.
/// Slow (tens of ms), so it is cached and refreshed every 20 s.
#[derive(Default)]
struct Sockets {
    at: Option<Instant>,
    listening: std::collections::HashMap<i32, Vec<u16>>,
    established: std::collections::HashMap<i32, u32>,
}

impl Sockets {
    fn refresh(&mut self) {
        if self.at.is_some_and(|t| t.elapsed() < Duration::from_secs(20)) {
            return;
        }
        self.at = Some(Instant::now());
        self.listening.clear();
        self.established.clear();
        for (state, listen) in [("LISTEN", true), ("ESTABLISHED", false)] {
            let Some(out) = sys::run_timeout(
                "lsof",
                &["-nP", "-iTCP", &format!("-sTCP:{state}"), "-F", "pn"],
                Duration::from_secs(3),
            ) else {
                continue;
            };
            let mut pid = 0i32;
            for line in out.lines() {
                if let Some(p) = line.strip_prefix('p') {
                    pid = p.parse().unwrap_or(0);
                } else if let Some(n) = line.strip_prefix('n') {
                    if listen {
                        if let Some(port) = n.rsplit(':').next().and_then(|p| p.parse().ok()) {
                            let v = self.listening.entry(pid).or_default();
                            if !v.contains(&port) {
                                v.push(port);
                            }
                        }
                    } else {
                        *self.established.entry(pid).or_default() += 1;
                    }
                }
            }
        }
    }
}

pub struct MacProcesses {
    me: u32,
    sockets: Sockets,
    argbuf: Vec<u8>,
    degraded: bool,
}

impl MacProcesses {
    pub fn new() -> Self {
        // SAFETY: getuid has no preconditions.
        MacProcesses { me: unsafe { libc::getuid() }, sockets: Sockets::default(), argbuf: Vec::new(), degraded: false }
    }
}

impl Default for MacProcesses {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessSource for MacProcesses {
    fn sample(&mut self) -> Result<Vec<ProcessSample>> {
        let pids = sys::all_pids();
        if pids.is_empty() {
            return Err(Error::Unsupported("proc_listallpids"));
        }
        self.sockets.refresh();
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let mut degraded = false;
        let mut out = Vec::with_capacity(pids.len());

        for pid in pids {
            let Some(b) = sys::bsd_info(pid) else { continue };
            let name = {
                let n = sys::cstr(&b.pbi_name);
                if n.is_empty() { sys::cstr(&b.pbi_comm) } else { n }
            };
            let owner = owner_of(b.pbi_ruid, self.me);
            // SIDL=1 SRUN=2 SSLEEP=3 SSTOP=4 SZOMB=5. BSD status reports SRUN for
            // nearly everything on macOS, so only stopped and zombie are told apart.
            let state = match b.pbi_status {
                1 => 'I',
                4 => 'T',
                5 => 'Z',
                _ => 'S',
            };
            let task = sys::task_info(pid);
            let resident = task.map(|t| t.pti_resident_size).unwrap_or(0);
            let cpu_seconds = task
                .map(|t| sys::abs_to_ns(t.pti_total_user + t.pti_total_system) as f64 / 1e9)
                .unwrap_or(0.0);
            let proportional = match sys::footprint(pid) {
                Some(f) => f,
                None => {
                    // Other users' processes are refused by design; our own are not.
                    degraded |= owner == Owner::CurrentUser;
                    resident
                }
            };
            let cmdline = if owner == Owner::CurrentUser { sys::cmdline(pid, &mut self.argbuf) } else { String::new() };
            out.push(ProcessSample {
                pid,
                ppid: b.pbi_ppid as i32,
                name,
                cmdline,
                owner,
                cpu_seconds,
                resident,
                proportional,
                state,
                has_tty: b.e_tdev != 0 && b.e_tdev != u32::MAX,
                age_seconds: now.saturating_sub(b.pbi_start_tvsec) as f64,
                listening_ports: self.sockets.listening.get(&pid).cloned().unwrap_or_default(),
                established: self.sockets.established.get(&pid).copied().unwrap_or(0),
                group: None,
            });
        }
        self.degraded = degraded;
        Ok(out)
    }

    fn accounting_degraded(&self) -> bool {
        self.degraded
    }
}

/// A handle on one process, without pidfd.
///
/// macOS has no pidfd, so PID reuse is guarded by identity: the start time is
/// recorded when opened and re-checked before every signal. A recycled PID has
/// a different start time, so the signal is refused rather than delivered to a
/// stranger. The check and the `kill` are not atomic; the window is microseconds.
pub struct MacHandle {
    pid: i32,
    start: (u64, u64),
}

fn start_of(pid: i32) -> Option<(u64, u64)> {
    sys::bsd_info(pid).map(|b| (b.pbi_start_tvsec, b.pbi_start_tvusec))
}

impl MacHandle {
    pub fn open(pid: i32) -> Result<Self> {
        start_of(pid).map(|start| MacHandle { pid, start }).ok_or_else(|| Error::missing(format!("pid {pid}")))
    }

    fn signal(&self, sig: i32) -> Result<()> {
        if start_of(self.pid) != Some(self.start) {
            return Err(Error::missing(format!("pid {} (exited or PID reused)", self.pid)));
        }
        // SAFETY: plain kill(2) on a pid whose identity was just verified.
        if unsafe { libc::kill(self.pid, sig) } == 0 {
            return Ok(());
        }
        Err(match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::ESRCH) => Error::missing(format!("pid {}", self.pid)),
            Some(libc::EPERM) => Error::denied(format!("pid {}", self.pid), "process belongs to another user"),
            _ => Error::os(std::io::Error::last_os_error()),
        })
    }
}

impl ProcessHandle for MacHandle {
    fn pid(&self) -> i32 {
        self.pid
    }

    fn is_alive(&self) -> bool {
        start_of(self.pid) == Some(self.start) && sys::bsd_info(self.pid).is_some_and(|b| b.pbi_status != 5)
    }

    fn suspend(&self) -> Result<()> {
        self.signal(libc::SIGSTOP)
    }

    fn resume(&self) -> Result<()> {
        self.signal(libc::SIGCONT)
    }

    fn terminate(&self) -> Result<()> {
        self.signal(libc::SIGTERM)
    }

    fn kill(&self) -> Result<()> {
        self.signal(libc::SIGKILL)
    }

    fn wait_exit(&self, timeout: Duration) -> Result<bool> {
        let deadline = Instant::now() + timeout;
        loop {
            if !self.is_alive() {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

pub struct MacControl;

impl ProcessControl for MacControl {
    fn open(&self, pid: i32) -> Result<Box<dyn ProcessHandle>> {
        Ok(Box::new(MacHandle::open(pid)?))
    }

    fn proportional(&self, pid: i32) -> Option<u64> {
        sys::footprint(pid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn sees_ourselves() {
        let me = std::process::id() as i32;
        let s = MacProcesses::new().sample().unwrap();
        let mine = s.iter().find(|p| p.pid == me).expect("own process");
        assert_eq!(mine.owner, Owner::CurrentUser);
        assert!(mine.proportional > 0);
        assert!(mine.cmdline.contains("ramwarden"));
    }

    #[test]
    fn suspend_and_resume_a_child() {
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        let h = MacHandle::open(child.id() as i32).unwrap();
        h.suspend().unwrap();
        let state = sys::bsd_info(h.pid()).unwrap().pbi_status;
        assert_eq!(state, 4, "stopped");
        h.resume().unwrap();
        h.terminate().unwrap();
        assert!(h.wait_exit(Duration::from_secs(3)).unwrap() || child.try_wait().unwrap().is_some());
        let _ = child.wait();
    }

    #[test]
    fn refuses_a_reused_pid() {
        let mut h = MacHandle::open(std::process::id() as i32).unwrap();
        h.start.0 = h.start.0.wrapping_sub(1);
        assert!(h.terminate().unwrap_err().is_missing());
    }

    #[test]
    fn procargs_parse() {
        let mut b = 2i32.to_ne_bytes().to_vec();
        b.extend_from_slice(b"/bin/ls\0\0\0ls\0-l\0PATH=/bin\0");
        assert_eq!(sys::parse_procargs(&b), "ls -l");
    }
}
