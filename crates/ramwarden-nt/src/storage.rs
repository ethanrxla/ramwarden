//! Free space per drive: `GetLogicalDrives` and `GetDiskFreeSpaceExW`.
//!
//! Only fixed and removable drives are listed. Network drives are skipped
//! (asking a disconnected share can block for seconds, and nothing RamWarden
//! suggests frees space there), as are CD-ROM and RAM disks.
//!
//! The free figure is the one available to the calling user, which honours
//! quotas. It is the number Explorer shows and the one a write is checked
//! against.

use std::path::PathBuf;

use ramwarden_platform::{Error, Result, StorageSource, Volume};
use windows::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives};
use windows::core::HSTRING;

/// `DRIVE_REMOVABLE` and `DRIVE_FIXED` from `GetDriveTypeW`.
const REMOVABLE: u32 = 2;
const FIXED: u32 = 3;

/// The Windows volume reader.
pub struct NtStorage;

impl StorageSource for NtStorage {
    fn volumes(&self) -> Result<Vec<Volume>> {
        // SAFETY: takes no arguments and returns a bitmask.
        let mask = unsafe { GetLogicalDrives() };
        if mask == 0 {
            return Err(Error::os("GetLogicalDrives returned no drives"));
        }
        let system = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into()).to_uppercase();
        let mut out = Vec::new();
        for i in 0..26u8 {
            if mask & (1 << i) == 0 {
                continue;
            }
            let name = format!("{}:", (b'A' + i) as char);
            let root = HSTRING::from(format!("{name}\\"));
            // SAFETY: `root` is a NUL-terminated drive root.
            let kind = unsafe { GetDriveTypeW(&root) };
            if kind != FIXED && kind != REMOVABLE {
                continue;
            }
            let (mut free, mut total) = (0u64, 0u64);
            // SAFETY: both out pointers are valid for one u64 each. An empty
            // card reader fails here and is skipped.
            if unsafe { GetDiskFreeSpaceExW(&root, Some(&mut free), Some(&mut total), None) }.is_err() || total == 0 {
                continue;
            }
            out.push(Volume {
                mount: PathBuf::from(format!("{name}\\")),
                system: name == system,
                name,
                total,
                free,
            });
        }
        Ok(out)
    }
}
