//! Memory pressure on the PSI scale, computed from what the VM is doing.
//!
//! macOS has no stall accounting, so this estimates it. The inputs are the
//! cumulative `vm_statistics64` counters, differenced between reads:
//!
//! * `swapins` — a page read back from the swap file: a task blocked on SSD.
//! * `decompressions` — a page pulled out of the compressor: a short stall.
//! * `swapouts` — written while memory is short; mostly asynchronous, so weighted low.
//!
//! Each event is charged a fixed cost in seconds, and the total per wall-clock
//! second is the share of time stalled (`some`). `full` is the swap-in share
//! alone, halved: every task waiting at once needs the slow path. Rates are
//! smoothed with an exponentially weighted average at 10 s, 60 s and 300 s time
//! constants, matching PSI's three windows.
//!
//! Two overrides keep the result inside what the ladder was tuned for:
//!
//! * the kernel's own pressure level is a **floor** (warn >= tabs rung,
//!   critical >= suspend rung), so a level change is never missed;
//! * a draining-memory **forecast** (see [`crate::forecast`]) lifts `some` to the
//!   tabs rung when available memory will hit the floor within [`HORIZON_SECONDS`],
//!   so tabs are unloaded before the stall, not after it.
//!
//! `full` is capped below the kill rung: this estimate is good enough to unload
//! tabs and suspend watchlisted apps, not to kill anything.

use std::sync::Mutex;
use std::time::Instant;

use ramwarden_platform::{Error, Pressure, PressureSource, PressureWaker, Result};

use crate::forecast::Forecaster;
use crate::sys::{page_size, sysctl_int, vm_stats};

/// Charged per event, in seconds. Fixed, documented estimates for Apple-silicon
/// SSD swap (16 KiB pages) and the in-RAM compressor.
const COST_SWAPIN: f64 = 150e-6;
const COST_DECOMPRESS: f64 = 10e-6;
const COST_SWAPOUT: f64 = 0.25 * COST_SWAPIN;

/// Rung values the ladder was tuned to on macOS (see `packaging/macos/config.toml`).
const WARN_SOME: f64 = 11.0;
const CRITICAL_SOME: f64 = 16.0;
const CRITICAL_FULL: f64 = 10.0;
/// Below the config's `kill_at_full = 25`.
const FULL_CAP: f64 = 20.0;

/// Lift to the tabs rung when the forecast says the floor is this close.
pub const HORIZON_SECONDS: f64 = 90.0;
/// Available memory below this share of RAM is the forecast's "floor".
const FLOOR_FRACTION: f64 = 0.06;
/// Ignore forecasts while plenty is free.
const ARM_BELOW_FRACTION: f64 = 0.25;

const TAUS: [f64; 3] = [10.0, 60.0, 300.0];
/// Reads closer together than this add noise, not information.
const MIN_DT: f64 = 0.5;

#[derive(Clone, Copy, Debug, Default)]
pub struct Counters {
    pub swapins: u64,
    pub swapouts: u64,
    pub decompressions: u64,
    pub available: u64,
    pub total: u64,
}

/// Share of wall time stalled, `(some, full)` in percent, for one interval.
pub fn stall_percent(prev: &Counters, now: &Counters, dt: f64) -> (f64, f64) {
    let rate = |a: u64, b: u64| b.saturating_sub(a) as f64 / dt;
    let swapin = rate(prev.swapins, now.swapins) * COST_SWAPIN;
    let decomp = rate(prev.decompressions, now.decompressions) * COST_DECOMPRESS;
    let swapout = rate(prev.swapouts, now.swapouts) * COST_SWAPOUT;
    let some = (swapin + decomp + swapout) * 100.0;
    (some.min(100.0), (swapin * 50.0).min(100.0))
}

/// `1 - e^(-dt/tau)`: how far an average with time constant `tau` moves in `dt`.
fn alpha(dt: f64, tau: f64) -> f64 {
    1.0 - (-dt / tau).exp()
}

pub struct Estimator {
    last: Option<(f64, Counters)>,
    some: [f64; 3],
    full: [f64; 3],
    seeded: bool,
    forecast: Forecaster,
    latest: Pressure,
}

impl Default for Estimator {
    fn default() -> Self {
        Estimator {
            last: None,
            some: [0.0; 3],
            full: [0.0; 3],
            seeded: false,
            forecast: Forecaster::default(),
            latest: Pressure { estimated: true, ..Pressure::default() },
        }
    }
}

impl Estimator {
    /// Fold in one reading taken at `t` seconds, with the kernel's pressure
    /// `level` (1 normal / 2 warn / 4 critical). Returns the estimate.
    pub fn update(&mut self, t: f64, c: Counters, level: u64) -> Pressure {
        let floor = c.total as f64 * FLOOR_FRACTION;
        self.forecast.push(t, c.available as f64);

        if let Some((t0, prev)) = self.last {
            let dt = t - t0;
            if dt < MIN_DT {
                return self.latest;
            }
            let (some, full) = stall_percent(&prev, &c, dt);
            for (i, &tau) in TAUS.iter().enumerate() {
                if self.seeded {
                    let a = alpha(dt, tau);
                    self.some[i] += a * (some - self.some[i]);
                    self.full[i] += a * (full - self.full[i]);
                } else {
                    // First interval: seed rather than climb from zero, or the
                    // 300 s window would read 0 for minutes.
                    self.some[i] = some;
                    self.full[i] = full;
                }
            }
            self.seeded = true;
        }
        self.last = Some((t, c));

        let (lvl_some, lvl_full) = match level {
            4.. => (CRITICAL_SOME, CRITICAL_FULL),
            2..=3 => (WARN_SOME, 0.0),
            _ => (0.0, 0.0),
        };
        let armed = (c.available as f64) < c.total as f64 * ARM_BELOW_FRACTION;
        let imminent = armed && self.forecast.seconds_until(floor).is_some_and(|s| s <= HORIZON_SECONDS);
        let lift = if imminent { WARN_SOME } else { 0.0 };

        let some = |v: f64| v.max(lvl_some).max(lift);
        let full = |v: f64| v.max(lvl_full).min(FULL_CAP);
        self.latest = Pressure {
            some_avg10: some(self.some[0]),
            some_avg60: some(self.some[1]),
            some_avg300: some(self.some[2]),
            full_avg10: full(self.full[0]),
            full_avg60: full(self.full[1]),
            full_avg300: full(self.full[2]),
            estimated: true,
        };
        self.latest
    }

    /// Seconds until available memory hits the floor on the current trend.
    pub fn seconds_to_floor(&self, total: u64) -> Option<f64> {
        self.forecast.seconds_until(total as f64 * FLOOR_FRACTION)
    }
}

pub struct MacPressure {
    start: Instant,
    state: Mutex<Estimator>,
}

impl MacPressure {
    pub fn new() -> Self {
        MacPressure { start: Instant::now(), state: Mutex::new(Estimator::default()) }
    }
}

impl Default for MacPressure {
    fn default() -> Self {
        Self::new()
    }
}

fn counters() -> Result<Counters> {
    let vm = vm_stats().ok_or(Error::Unsupported("host_statistics64"))?;
    let total = sysctl_int("hw.memsize").ok_or(Error::Unsupported("hw.memsize"))?;
    let ps = page_size();
    Ok(Counters {
        swapins: vm.swapins,
        swapouts: vm.swapouts,
        decompressions: vm.decompressions,
        available: (vm.free_count as u64 + vm.inactive_count as u64 + vm.speculative_count as u64) * ps,
        total,
    })
}

impl PressureSource for MacPressure {
    fn read(&self) -> Result<Pressure> {
        let level = sysctl_int("kern.memorystatus_vm_pressure_level")
            .ok_or(Error::Unsupported("kern.memorystatus_vm_pressure_level"))?;
        let c = counters()?;
        let mut est = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let p = est.update(self.start.elapsed().as_secs_f64(), c, level);
        if let Some(s) = est.seconds_to_floor(c.total).filter(|&s| s <= HORIZON_SECONDS && s > 0.0) {
            tracing::debug!("memory forecast: floor in ~{s:.0}s (available {} MB)", c.available / 1_000_000);
        }
        Ok(p)
    }

    fn waker(&self) -> Result<Box<dyn PressureWaker>> {
        Err(Error::Unsupported("pressure wakeups (macOS has no PSI trigger; polling adaptively)"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GB: u64 = 1_000_000_000;

    fn calm(avail: u64) -> Counters {
        Counters { available: avail, total: 16 * GB, ..Counters::default() }
    }

    #[test]
    fn idle_machine_reports_nothing() {
        let mut e = Estimator::default();
        for s in 0..30 {
            let p = e.update(s as f64, calm(8 * GB), 1);
            assert!(p.is_quiet() && p.estimated);
        }
    }

    #[test]
    fn stall_math_is_exact() {
        let a = Counters::default();
        // 1000 swap-ins/s * 150us = 15% stalled; 1000 decompressions/s * 10us = 1%.
        let b = Counters { swapins: 1000, decompressions: 1000, ..a };
        let (some, full) = stall_percent(&a, &b, 1.0);
        assert!((some - 16.0).abs() < 1e-9);
        assert!((full - 7.5).abs() < 1e-9);
        // Over two seconds the same counts are half the rate.
        assert!((stall_percent(&a, &b, 2.0).0 - 8.0).abs() < 1e-9);
        // Saturates at 100.
        let huge = Counters { swapins: 10_000_000, ..a };
        assert_eq!(stall_percent(&a, &huge, 1.0), (100.0, 100.0));
    }

    #[test]
    fn heavy_swapping_climbs_and_recovers() {
        let mut e = Estimator::default();
        let mut c = calm(8 * GB);
        e.update(0.0, c, 1);
        for s in 1..=20 {
            c.swapins += 400; // 6% stalled
            c.available = 8 * GB;
            e.update(s as f64, c, 1);
        }
        let hot = e.update(21.0, Counters { swapins: c.swapins + 400, ..c }, 1);
        assert!(hot.some_avg10 > 5.0 && hot.some_avg10 < 7.0, "got {}", hot.some_avg10);
        assert!(hot.some_avg10 >= hot.some_avg300, "short window reacts first");
        let mut cool = hot;
        for s in 22..80 {
            cool = e.update(s as f64, Counters { swapins: c.swapins + 400, ..c }, 1);
        }
        assert!(cool.some_avg10 < 0.1, "decays once swapping stops, got {}", cool.some_avg10);
    }

    #[test]
    fn kernel_level_is_a_floor_and_full_never_reaches_the_kill_rung() {
        let mut e = Estimator::default();
        let p = e.update(0.0, calm(8 * GB), 4);
        assert_eq!((p.some_avg10, p.full_avg10), (CRITICAL_SOME, CRITICAL_FULL));
        let mut e = Estimator::default();
        let w = e.update(0.0, calm(8 * GB), 2);
        assert_eq!((w.some_avg10, w.full_avg10), (WARN_SOME, 0.0));

        // Pathological thrash: full is capped under kill_at_full = 25.
        let mut e = Estimator::default();
        let mut c = calm(8 * GB);
        e.update(0.0, c, 1);
        c.swapins = 1_000_000;
        let p = e.update(1.0, c, 1);
        assert_eq!(p.full_avg10, FULL_CAP);
        assert!(p.full_avg10 < 25.0);
    }

    #[test]
    fn a_draining_machine_lifts_to_the_tabs_rung_before_any_stall() {
        let mut e = Estimator::default();
        // 3 GB available, losing 20 MB/s: floor (0.96 GB) in ~100 s at first,
        // inside the 90 s horizon after a few more seconds. No swapping at all.
        let mut p = Pressure::default();
        let mut lifted_at = None;
        for s in 0..60 {
            let avail = 3 * GB - 20_000_000 * s as u64;
            p = e.update(s as f64, calm(avail), 1);
            if lifted_at.is_none() && p.some_avg10 >= WARN_SOME {
                lifted_at = Some(s);
            }
        }
        let at = lifted_at.expect("forecast should lift pressure");
        assert!((10..=30).contains(&at), "lifted at {at}s");
        assert_eq!(p.some_avg10, WARN_SOME);
    }

    #[test]
    fn plenty_of_free_memory_never_triggers_the_forecast() {
        let mut e = Estimator::default();
        let mut p = Pressure::default();
        for s in 0..60 {
            p = e.update(s as f64, calm(12 * GB - 20_000_000 * s as u64), 1);
        }
        assert!(p.is_quiet(), "12 GB free is above the arming level");
    }

    #[test]
    fn reads_live() {
        let p = MacPressure::new();
        let a = p.read().unwrap();
        assert!(a.estimated && a.some_avg10 >= 0.0 && a.full_avg10 <= FULL_CAP.max(CRITICAL_FULL));
    }
}
