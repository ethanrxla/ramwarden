//! A PSI-scale memory pressure estimate for Windows.
//!
//! # Why an estimate
//!
//! Linux reports how much wall time tasks lost waiting for memory. Windows has
//! no such counter, and "% used" is the wrong trigger for the same reason it was
//! on Linux: a 32 GB machine at 50% with a full standby cache is healthy, while
//! one at 85% paging continuously is not. So the estimate combines the three
//! things that *do* distinguish those cases, and reports them on PSI's 0–100
//! scale so the ladder's thresholds keep a single meaning on both platforms:
//!
//! * **Scarcity** — available memory (free + standby) as a fraction of RAM.
//!   0 at 20% available, rising to 9.9 at none. Capped below the tab rung's
//!   default threshold (10): being low on memory justifies reclaim and
//!   page-out, but nothing that the user would notice.
//! * **Commit** — commit charge against the limit. 0 at 85%, 9.9 at 98%, with
//!   the same cap. Running out of commit fails allocations outright.
//! * **Faulting** — hard page faults per second, the closest thing Windows has
//!   to a stall signal. 2000/s maps to 100. Counted only while memory is
//!   actually scarce (fully below 5% available, not at all above 15%), because
//!   hard faults with plenty of RAM free are an app reading its files, not
//!   memory pressure.
//!
//! `some` is the largest of the three. `full` — "nothing is getting done" — is
//! the faulting term alone, and only once available memory is below 8%.
//! Both are then averaged over 10/60/300 seconds like PSI's windows.
//!
//! Every figure that comes out of here has `estimated: true`, and `/psi` says so.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use ramwarden_platform::{Error, Pressure, PressureSource, PressureWaker, Result};
use windows::Win32::Foundation::WAIT_OBJECT_0;
use windows::Win32::System::Memory::{
    CreateMemoryResourceNotification, LowMemoryResourceNotification,
};
use windows::Win32::System::Threading::WaitForSingleObject;

use crate::handle::Handle;
use crate::{memory, snapshot};

/// What the estimate is computed from at one instant.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Inputs {
    /// available / total physical memory, 0..=1.
    pub available_fraction: f64,
    /// commit charge / commit limit, 0..=1.
    pub commit_fraction: f64,
    pub hard_faults_per_sec: f64,
}

const SCARCITY_START: f64 = 0.20;
const COMMIT_START: f64 = 0.85;
const COMMIT_END: f64 = 0.98;
/// Below the default tab threshold: low memory alone never suspends anything.
const SOFT_CAP: f64 = 9.9;
const FAULTS_AT_FULL_SCALE: f64 = 2000.0;
const FAULT_GATE_OPEN: f64 = 0.15;
const FAULT_GATE_FULL: f64 = 0.05;
const STALL_BELOW: f64 = 0.08;

fn ramp(x: f64, from: f64, to: f64) -> f64 {
    ((x - from) / (to - from)).clamp(0.0, 1.0)
}

/// The instantaneous (`some`, `full`) estimate, before averaging. Pure.
pub fn instant(i: &Inputs) -> (f64, f64) {
    let scarcity = ramp(SCARCITY_START - i.available_fraction, 0.0, SCARCITY_START) * SOFT_CAP;
    let commit = ramp(i.commit_fraction, COMMIT_START, COMMIT_END) * SOFT_CAP;
    let gate = ramp(FAULT_GATE_OPEN - i.available_fraction, 0.0, FAULT_GATE_OPEN - FAULT_GATE_FULL);
    let faulting = (i.hard_faults_per_sec / FAULTS_AT_FULL_SCALE * 100.0).clamp(0.0, 100.0) * gate;
    let some = scarcity.max(commit).max(faulting);
    let full = faulting * ramp(STALL_BELOW - i.available_fraction, 0.0, STALL_BELOW - FAULT_GATE_FULL);
    (some, full)
}

/// Exponential averages over PSI's three windows.
#[derive(Clone, Debug, Default)]
pub struct Estimator {
    some: [f64; 3],
    full: [f64; 3],
    last: Option<Instant>,
}

const WINDOWS: [f64; 3] = [10.0, 60.0, 300.0];

impl Estimator {
    /// Fold in one observation taken at `now`.
    pub fn update(&mut self, now: Instant, inputs: &Inputs) -> Pressure {
        let (some, full) = instant(inputs);
        match self.last {
            // First reading: start the averages at the observation rather than
            // at zero, so a daemon started mid-crisis sees the crisis.
            None => {
                self.some = [some; 3];
                self.full = [full; 3];
            }
            Some(prev) => {
                let dt = now.saturating_duration_since(prev).as_secs_f64();
                for (k, tau) in WINDOWS.iter().enumerate() {
                    let a = 1.0 - (-dt / tau).exp();
                    self.some[k] += (some - self.some[k]) * a;
                    self.full[k] += (full - self.full[k]) * a;
                }
            }
        }
        self.last = Some(now);
        self.current()
    }

    pub fn current(&self) -> Pressure {
        let r = |v: f64| (v * 100.0).round() / 100.0;
        Pressure {
            some_avg10: r(self.some[0]),
            some_avg60: r(self.some[1]),
            some_avg300: r(self.some[2]),
            full_avg10: r(self.full[0]),
            full_avg60: r(self.full[1]),
            full_avg300: r(self.full[2]),
            estimated: true,
        }
    }
}

/// [`PressureSource`] over the live system.
///
/// Each read samples the machine, so the averages advance with however often
/// the monitor asks — every second under pressure, every ten when quiet.
#[derive(Default)]
pub struct NtPressure {
    state: Mutex<(Estimator, Option<(Instant, u64)>)>,
}

impl NtPressure {
    pub fn new() -> Self {
        Self::default()
    }

    fn total_hard_faults() -> Result<u64> {
        Ok(snapshot::processes()?.iter().map(|p| p.hard_faults as u64).sum())
    }
}

impl PressureSource for NtPressure {
    fn read(&self) -> Result<Pressure> {
        let m = memory::status()?;
        let faults = Self::total_hard_faults()?;
        let now = Instant::now();

        let mut guard = self.state.lock().unwrap();
        let (est, prev) = &mut *guard;
        // A delta over the processes present in both samples: an exiting
        // process can make the sum fall, which saturates to zero, not negative.
        let rate = match prev {
            Some((at, before)) => {
                let dt = now.saturating_duration_since(*at).as_secs_f64().max(0.001);
                faults.saturating_sub(*before) as f64 / dt
            }
            None => 0.0,
        };
        *prev = Some((now, faults));

        let inputs = Inputs {
            available_fraction: if m.total > 0 { m.available as f64 / m.total as f64 } else { 1.0 },
            commit_fraction: if m.commit_limit > 0 {
                m.commit as f64 / m.commit_limit as f64
            } else {
                0.0
            },
            hard_faults_per_sec: rate,
        };
        Ok(est.update(now, &inputs))
    }

    fn waker(&self) -> Result<Box<dyn PressureWaker>> {
        Ok(Box::new(LowMemoryWaker::new()?))
    }
}

/// Wakes the monitor when the memory manager signals low memory.
///
/// The notification is level-triggered: it stays signalled for as long as
/// memory is low. Waiting on it naively would spin the monitor, so a wake is
/// reported at most once per [`MIN_SPACING`]; in between, the wait simply runs
/// out its timeout like an ordinary tick.
pub struct LowMemoryWaker {
    handle: Handle,
    last_wake: Option<Instant>,
}

const MIN_SPACING: Duration = Duration::from_secs(1);

impl LowMemoryWaker {
    pub fn new() -> Result<Self> {
        // SAFETY: plain FFI; the handle is owned immediately.
        let raw = unsafe { CreateMemoryResourceNotification(LowMemoryResourceNotification) }
            .map_err(|e| Error::os(format!("CreateMemoryResourceNotification: {e}")))?;
        let handle = Handle::new(raw).ok_or_else(|| Error::os("no notification handle"))?;
        Ok(LowMemoryWaker {
            handle,
            last_wake: None,
        })
    }
}

impl PressureWaker for LowMemoryWaker {
    fn wait(&mut self, timeout: Duration) -> Result<bool> {
        if let Some(at) = self.last_wake {
            let since = at.elapsed();
            if since < MIN_SPACING {
                std::thread::sleep(MIN_SPACING - since);
            }
        }
        let ms = timeout.as_millis().min(u32::MAX as u128 - 1) as u32;
        // SAFETY: waiting on a notification handle we own.
        let woken = unsafe { WaitForSingleObject(self.handle.raw(), ms) } == WAIT_OBJECT_0;
        if woken {
            self.last_wake = Some(Instant::now());
        }
        Ok(woken)
    }

    fn describe(&self) -> String {
        "low-memory resource notification".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ramwarden_platform::Pressure;

    fn at(avail: f64, commit: f64, faults: f64) -> (f64, f64) {
        instant(&Inputs {
            available_fraction: avail,
            commit_fraction: commit,
            hard_faults_per_sec: faults,
        })
    }

    /// The machine this was written for, idle: 31% used, page cache warm.
    #[test]
    fn a_healthy_machine_reads_as_no_pressure() {
        assert_eq!(at(0.69, 0.45, 50.0), (0.0, 0.0));
        assert_eq!(at(0.50, 0.60, 800.0), (0.0, 0.0), "faults with RAM free are file I/O");
    }

    /// Being low on memory alone justifies reclaim, never a visible action.
    #[test]
    fn scarcity_alone_stays_below_the_tab_rung() {
        let (some, full) = at(0.0, 0.5, 0.0);
        assert!(some < 10.0 && some > 9.0, "{some}");
        assert_eq!(full, 0.0);
        let (some, _) = at(0.15, 0.5, 0.0);
        assert!(some >= 2.0, "15% available should reach the reclaim rung: {some}");
    }

    #[test]
    fn exhausted_commit_reads_as_pressure() {
        let (some, _) = at(0.5, 0.98, 0.0);
        assert!((some - SOFT_CAP).abs() < 1e-9);
        assert_eq!(at(0.5, 0.85, 0.0).0, 0.0);
    }

    /// Thrashing — scarce memory and heavy faulting — is what reaches the
    /// suspend and kill rungs.
    #[test]
    fn thrashing_reaches_the_high_rungs() {
        let (some, full) = at(0.03, 0.9, 1500.0);
        assert!(some >= 15.0, "{some}");
        assert!(full >= 25.0, "{full}");
    }

    #[test]
    fn the_estimate_is_monotonic_in_scarcity() {
        let mut last = -1.0;
        for pct in (0..=30).rev() {
            let (some, _) = at(pct as f64 / 100.0, 0.5, 400.0);
            assert!(some >= last, "{pct}%: {some} < {last}");
            last = some;
        }
    }

    #[test]
    fn averages_start_at_the_first_reading_and_then_decay() {
        let mut e = Estimator::default();
        let t0 = Instant::now();
        let hot = Inputs { available_fraction: 0.03, commit_fraction: 0.9, hard_faults_per_sec: 1500.0 };
        let p: Pressure = e.update(t0, &hot);
        assert!(p.some_avg10 > 15.0 && p.estimated);

        let calm = Inputs { available_fraction: 0.6, ..Default::default() };
        let first = e.current().some_avg10;
        let p = e.update(t0 + Duration::from_secs(30), &calm);
        // Three time constants: e^-3 ≈ 5% of the starting value remains.
        assert!(
            p.some_avg10 < first * 0.06,
            "10 s average should have mostly decayed: {p:?}"
        );
        assert!(p.some_avg300 > p.some_avg10, "the long window decays slowest");
    }

    #[test]
    fn the_live_source_reads_and_is_marked_estimated() {
        let src = NtPressure::new();
        let p = src.read().unwrap();
        assert!(p.estimated);
        assert!((0.0..=100.0).contains(&p.some_avg10));
    }
}
