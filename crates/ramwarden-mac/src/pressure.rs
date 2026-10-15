use ramwarden_platform::{Error, Pressure, PressureSource, PressureWaker, Result};

use crate::sys::sysctl_int;

pub struct MacPressure;

/// Pure mapping from the kernel's pressure signals to the ladder's scale.
///
/// `level` is 1 normal / 2 warn / 4 critical; `avail_pct` is percent of memory
/// available (0-100). The numbers are synthetic: chosen to land in the ladder's
/// existing rungs (warn -> tab unload, critical -> suspend), and deliberately
/// short of the kill rung.
pub fn proxy(level: u64, avail_pct: u64) -> Pressure {
    let (some, full) = match level {
        4.. => (16.0, 10.0),
        2..=3 => (11.0, 0.0),
        // Normal: drift upward as free memory shrinks, so the first rungs
        // (advisory here) can still be observed in the action log.
        _ => ((30.0 - avail_pct.min(100) as f64).max(0.0) * 0.1, 0.0),
    };
    Pressure {
        some_avg10: some,
        some_avg60: some,
        some_avg300: some,
        full_avg10: full,
        full_avg60: full,
        full_avg300: full,
        estimated: true,
    }
}

impl PressureSource for MacPressure {
    fn read(&self) -> Result<Pressure> {
        let level = sysctl_int("kern.memorystatus_vm_pressure_level")
            .ok_or(Error::Unsupported("kern.memorystatus_vm_pressure_level"))?;
        let avail = sysctl_int("kern.memorystatus_level").unwrap_or(100);
        Ok(proxy(level, avail))
    }

    fn waker(&self) -> Result<Box<dyn PressureWaker>> {
        Err(Error::Unsupported("pressure wakeups (macOS has no PSI trigger; polling adaptively)"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_land_in_expected_rungs() {
        assert_eq!(proxy(1, 80).some_avg10, 0.0);
        assert!(proxy(1, 5).some_avg10 > 2.0, "low free memory nudges the first rung");
        assert_eq!(proxy(2, 50).some_avg10, 11.0);
        let crit = proxy(4, 5);
        assert!(crit.some_avg10 > 15.0 && crit.full_avg10 < 25.0, "critical suspends but never kills");
        assert!(crit.estimated);
    }

    #[test]
    fn reads_live() {
        assert!(MacPressure.read().unwrap().some_avg10 >= 0.0);
    }
}
