use std::time::Duration;

use crate::Result;

/// Who a process runs as, reduced to the only distinction policy makes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Owner {
    /// The interactive user running RamWarden.
    CurrentUser,
    /// root on Linux; SYSTEM, LocalService, NetworkService or session 0 on
    /// Windows. Always classified as a system role, and so never touched.
    System,
    /// Some other ordinary account.
    OtherUser,
    /// Could not be read. Treated as neither safe nor system.
    #[default]
    Unknown,
}

/// One process at one moment, as the mechanism layer measured it.
///
/// Raw facts only. The CPU figure is cumulative; the detector differences two
/// samples itself, because "was it busy?" is a policy question.
#[derive(Clone, Debug, Default)]
pub struct ProcessSample {
    pub pid: i32,
    pub ppid: i32,
    /// Short process name: `comm` on Linux, the image name without `.exe` on
    /// Windows.
    pub name: String,
    pub cmdline: String,
    pub owner: Owner,
    /// Total CPU time consumed since the process started, in seconds.
    pub cpu_seconds: f64,
    /// Resident bytes as a naive tool would report them (RSS, working set).
    /// Kept only to show how far that figure misleads.
    pub resident: u64,
    /// Bytes this process genuinely owns — PSS on Linux, private working set on
    /// Windows. The figure that is meaningful to sum.
    pub proportional: u64,
    /// `S`, `R`, `T` (stopped/suspended), `Z`... Windows reports `T` when every
    /// thread is suspended and `S` otherwise.
    pub state: char,
    pub has_tty: bool,
    pub age_seconds: f64,
    pub listening_ports: Vec<u16>,
    pub established: u32,
    /// The reclaim group this process belongs to, if any. `None` means the
    /// ladder cannot reach it through group reclaim.
    pub group: Option<String>,
}

/// Enumerates every process the user can see.
pub trait ProcessSource: Send + Sync {
    fn sample(&mut self) -> Result<Vec<ProcessSample>>;

    /// True when proportional accounting has silently degraded to resident
    /// size — the overcount the rewrite exists to fix. Must be surfaced.
    fn accounting_degraded(&self) -> bool {
        false
    }
}

/// A handle bound to one specific process for its whole lifetime.
///
/// Holding a handle, not a number, is what guarantees a recycled PID is never
/// signalled: once the process exits, every call on its handle fails.
pub trait ProcessHandle: Send + Sync {
    fn pid(&self) -> i32;
    fn is_alive(&self) -> bool;
    /// Freeze every thread. Reversible with [`ProcessHandle::resume`].
    fn suspend(&self) -> Result<()>;
    fn resume(&self) -> Result<()>;
    /// Ask politely: `SIGTERM`, or `WM_CLOSE` to the process's windows.
    fn terminate(&self) -> Result<()>;
    /// Insist: `SIGKILL`, or `TerminateProcess`.
    fn kill(&self) -> Result<()>;
    /// Wait up to `timeout` for exit. `Ok(true)` if it exited.
    fn wait_exit(&self, timeout: Duration) -> Result<bool>;
}

/// Opens handles and answers point queries about single processes.
pub trait ProcessControl: Send + Sync {
    fn open(&self, pid: i32) -> Result<Box<dyn ProcessHandle>>;

    /// Current proportional size of one process, if it can be read.
    fn proportional(&self, pid: i32) -> Option<u64>;
}
