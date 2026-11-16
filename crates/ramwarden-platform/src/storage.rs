use std::path::PathBuf;

use crate::{Error, Result};

/// One mounted volume with files on it, in bytes.
///
/// Only volumes a person would recognise are reported: internal and external
/// disks. Pseudo filesystems, read-only images and network shares are left
/// out, because nothing RamWarden suggests can free space on them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Volume {
    /// Where it is mounted: `/`, `/home`, `C:\`.
    pub mount: PathBuf,
    /// What the user calls it: `C:`, `/`, `Macintosh HD`.
    pub name: String,
    pub total: u64,
    /// Free to the current user. Smaller than the raw free count where the
    /// filesystem reserves blocks for root or a quota applies, and it is the
    /// figure that decides whether a write succeeds.
    pub free: u64,
    /// Holds the operating system. Running out here is what breaks updates,
    /// swap and app launches, so the watcher treats it first.
    pub system: bool,
}

impl Volume {
    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.free)
    }

    /// Percent of the volume still free.
    pub fn free_percent(&self) -> f64 {
        if self.total == 0 {
            return 0.0;
        }
        self.free as f64 / self.total as f64 * 100.0
    }
}

/// Reads free space. One cheap call per volume (`statvfs`,
/// `GetDiskFreeSpaceExW`), so the monitor can ask every minute.
pub trait StorageSource: Send + Sync {
    fn volumes(&self) -> Result<Vec<Volume>>;
}

/// A platform that cannot report volumes. The storage watcher stays quiet.
pub struct NoStorage;

impl StorageSource for NoStorage {
    fn volumes(&self) -> Result<Vec<Volume>> {
        Err(Error::Unsupported("volume free space"))
    }
}
