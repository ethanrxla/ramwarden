//! `/proc/meminfo`, read honestly.
//!
//! # Why `MemAvailable` and not `MemFree`
//!
//! `MemFree` on a healthy Linux desktop is always small, because the kernel uses
//! spare memory for page cache and gives it back on demand. Reporting it as
//! "free" is how "RAM cleaner" tools justify themselves. `MemAvailable` is the
//! kernel's own estimate of what can be allocated without swapping, which is the
//! figure that actually predicts pressure.
//!
//! Note what is *not* here: a "percent used" field. v1 triggered on
//! `used / total` and was wrong about this machine by about 4 GB, because that
//! arithmetic counts memory zram has already compressed. [`Memory::percent`]
//! exists for the UI's progress bar, and nothing decides anything from it.

use std::fs;

use crate::{Error, KIB, Result, Root};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Memory {
    pub total: u64,
    pub free: u64,
    /// The kernel's estimate of what can be allocated without swapping.
    pub available: u64,
    pub buffers: u64,
    pub cached: u64,
    pub swap_total: u64,
    pub swap_free: u64,
    /// Anonymous memory currently swapped out — on this machine, mostly in zram.
    pub swap_cached: u64,
}

impl Memory {
    /// Bytes genuinely committed to applications.
    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.available)
    }

    pub fn swap_used(&self) -> u64 {
        self.swap_total.saturating_sub(self.swap_free)
    }

    /// Used share of total, for display only.
    ///
    /// Derived from `available` rather than `free`, so page cache is not counted
    /// against the user. Never use this to decide anything — see the module note.
    pub fn percent(&self) -> f64 {
        if self.total == 0 {
            return 0.0;
        }
        self.used() as f64 / self.total as f64 * 100.0
    }
}

pub fn read(root: &Root) -> Result<Memory> {
    let path = root.join("proc/meminfo");
    let text = fs::read_to_string(&path).map_err(|e| Error::io(&path, e))?;

    let mut m = Memory::default();
    let mut saw_total = false;
    for line in text.lines() {
        let Some((key, rest)) = line.split_once(':') else {
            continue;
        };
        let Some(value) = rest.split_whitespace().next() else {
            continue;
        };
        let Ok(kib) = value.parse::<u64>() else {
            continue;
        };
        let bytes = kib * KIB;
        match key {
            "MemTotal" => {
                m.total = bytes;
                saw_total = true;
            }
            "MemFree" => m.free = bytes,
            "MemAvailable" => m.available = bytes,
            "Buffers" => m.buffers = bytes,
            "Cached" => m.cached = bytes,
            "SwapTotal" => m.swap_total = bytes,
            "SwapFree" => m.swap_free = bytes,
            "SwapCached" => m.swap_cached = bytes,
            _ => continue,
        }
    }

    if !saw_total {
        return Err(Error::parse(&path, "no MemTotal line"));
    }
    // A kernel too old for MemAvailable (pre-3.14) would otherwise report zero
    // available and look like a permanent emergency.
    if m.available == 0 {
        m.available = m.free + m.buffers + m.cached;
    }
    Ok(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shaped like /proc/meminfo on the target machine.
    const MEMINFO: &str = "\
MemTotal:       31943944 kB
MemFree:         2284512 kB
MemAvailable:    7468020 kB
Buffers:          312044 kB
Cached:          8102992 kB
SwapCached:       493820 kB
SwapTotal:      33554428 kB
SwapFree:       28735996 kB
Dirty:              1024 kB
";

    fn fixture(body: &str) -> (tempfile::TempDir, Root) {
        let dir = tempfile::tempdir().unwrap();
        let root = Root::at(dir.path());
        fs::create_dir_all(root.join("proc")).unwrap();
        fs::write(root.join("proc/meminfo"), body).unwrap();
        (dir, root)
    }

    #[test]
    fn parses_the_fields_that_matter() {
        let (_d, root) = fixture(MEMINFO);
        let m = read(&root).unwrap();
        assert_eq!(m.total, 31_943_944 * KIB);
        assert_eq!(m.available, 7_468_020 * KIB);
        assert_eq!(m.free, 2_284_512 * KIB);
        assert_eq!(m.swap_total, 33_554_428 * KIB);
        assert_eq!(m.swap_cached, 493_820 * KIB);
    }

    /// Used is measured against `available`, so page cache is not counted
    /// against the user.
    #[test]
    fn used_is_total_minus_available_not_total_minus_free() {
        let (_d, root) = fixture(MEMINFO);
        let m = read(&root).unwrap();
        assert_eq!(m.used(), (31_943_944 - 7_468_020) * KIB);
        assert!(
            m.used() < m.total - m.free,
            "free-based arithmetic overstates usage"
        );
    }

    #[test]
    fn swap_used_is_reported() {
        let (_d, root) = fixture(MEMINFO);
        let m = read(&root).unwrap();
        assert_eq!(m.swap_used(), (33_554_428 - 28_735_996) * KIB);
    }

    #[test]
    fn percent_is_derived_from_available() {
        let (_d, root) = fixture(MEMINFO);
        let p = read(&root).unwrap().percent();
        assert!((p - 76.6).abs() < 0.2, "{p}");
    }

    #[test]
    fn an_empty_machine_does_not_divide_by_zero() {
        assert_eq!(Memory::default().percent(), 0.0);
    }

    /// Pre-3.14 kernels have no MemAvailable; zero would look like a permanent
    /// out-of-memory condition.
    #[test]
    fn a_kernel_without_mem_available_gets_an_estimate() {
        let (_d, root) = fixture(
            "MemTotal:  1000000 kB\nMemFree:  100000 kB\nBuffers:  50000 kB\nCached:  200000 kB\n",
        );
        let m = read(&root).unwrap();
        assert_eq!(m.available, (100_000 + 50_000 + 200_000) * KIB);
        assert!(m.available > 0);
    }

    #[test]
    fn a_file_without_mem_total_is_an_error_rather_than_a_zeroed_struct() {
        let (_d, root) = fixture("Dirty: 1024 kB\n");
        assert!(matches!(read(&root), Err(Error::Parse { .. })));
    }

    #[test]
    fn unparseable_values_are_skipped_rather_than_fatal() {
        let (_d, root) = fixture("MemTotal: 1000 kB\nMemFree: notanumber kB\n");
        let m = read(&root).unwrap();
        assert_eq!(m.total, 1000 * KIB);
        assert_eq!(m.free, 0);
    }

    #[test]
    fn a_missing_file_is_an_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let root = Root::at(dir.path());
        assert!(read(&root).unwrap_err().is_missing());
    }
}
