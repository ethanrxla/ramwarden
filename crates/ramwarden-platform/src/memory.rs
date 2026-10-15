use crate::Result;

/// System-wide memory, in bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemoryStatus {
    pub total: u64,
    pub free: u64,
    /// What can be allocated without paging: `MemAvailable` on Linux,
    /// `ullAvailPhys` (free + standby) on Windows.
    pub available: u64,
    /// Reclaimable cache: buffers + page cache, or the standby list.
    pub cached: u64,
    pub swap_total: u64,
    pub swap_free: u64,
    /// Commit charge and limit. Windows only; zero on Linux.
    pub commit: u64,
    pub commit_limit: u64,
}

impl MemoryStatus {
    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.available)
    }

    pub fn swap_used(&self) -> u64 {
        self.swap_total.saturating_sub(self.swap_free)
    }

    /// Percent of RAM not available. Never a trigger — shown for orientation.
    pub fn percent(&self) -> f64 {
        if self.total == 0 {
            return 0.0;
        }
        self.used() as f64 / self.total as f64 * 100.0
    }
}

/// In-RAM compressed memory: zram on Linux, the compression store on Windows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Compression {
    /// Uncompressed size of what is stored.
    pub original: u64,
    /// RAM the store actually occupies.
    pub resident: u64,
}

impl Compression {
    /// 1.0 means compression is buying nothing (or nothing is stored).
    pub fn ratio(&self) -> f64 {
        if self.resident == 0 {
            return 1.0;
        }
        self.original as f64 / self.resident as f64
    }

    /// RAM saved by compressing rather than holding pages as-is.
    pub fn saved(&self) -> u64 {
        self.original.saturating_sub(self.resident)
    }
}

pub trait MemorySource: Send + Sync {
    fn status(&self) -> Result<MemoryStatus>;
    fn compression(&self) -> Result<Compression>;
}
