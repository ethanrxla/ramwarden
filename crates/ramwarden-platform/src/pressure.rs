use std::time::Duration;

use crate::Result;

/// Memory pressure on the PSI scale: percent of wall time stalled.
///
/// Linux reports this directly. Windows has no equivalent, so its mechanism
/// crate computes an estimate on the same 0–100 scale and sets `estimated`, so
/// the ladder's thresholds keep one meaning and the UI can say which it is.
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize)]
pub struct Pressure {
    pub some_avg10: f64,
    pub some_avg60: f64,
    pub some_avg300: f64,
    pub full_avg10: f64,
    pub full_avg60: f64,
    pub full_avg300: f64,
    pub estimated: bool,
}

impl Pressure {
    /// Nothing stalled at all — the common case on a healthy machine.
    pub fn is_quiet(&self) -> bool {
        self.some_avg10 == 0.0 && self.full_avg10 == 0.0
    }
}

pub trait PressureSource: Send + Sync {
    fn read(&self) -> Result<Pressure>;

    /// An OS-level wakeup on rising pressure, when the platform grants one.
    /// `Err` explains why not; the monitor then polls adaptively.
    fn waker(&self) -> Result<Box<dyn PressureWaker>>;
}

/// Blocks until pressure rises, or a timeout passes.
pub trait PressureWaker: Send {
    /// `Ok(true)` if woken by pressure, `Ok(false)` on timeout.
    fn wait(&mut self, timeout: Duration) -> Result<bool>;

    /// For the startup log line.
    fn describe(&self) -> String;
}
