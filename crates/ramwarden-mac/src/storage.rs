//! Free space per volume: `statfs` on `/` and on each entry of `/Volumes`.
//!
//! APFS splits the startup disk into a sealed system volume (`/`) and a data
//! volume that share one container, so both report the same free space.
//! Volumes are therefore keyed by the device they are mounted from, and `/`
//! stands for the whole startup disk.
//!
//! The figure is `f_bavail`. Finder shows more, because it adds purgeable
//! space (local snapshots, evictable iCloud files); writes can fail before
//! macOS purges, so the watcher uses the smaller, certain number.

use std::collections::HashSet;
use std::ffi::{CStr, CString};
use std::mem::MaybeUninit;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use ramwarden_platform::{Error, Result, StorageSource, Volume};

struct Stat {
    device: String,
    total: u64,
    free: u64,
    read_only: bool,
}

// libc has changed these field widths between releases; the casts keep both building.
#[allow(clippy::unnecessary_cast)]
fn statfs(path: &Path) -> Option<Stat> {
    let c = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut st = MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: `c` is a valid C string and `st` is writable for one statfs.
    if unsafe { libc::statfs(c.as_ptr(), st.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: statfs returned 0, so it filled the struct.
    let st = unsafe { st.assume_init() };
    // SAFETY: the kernel NUL-terminates f_mntfromname.
    let device = unsafe { CStr::from_ptr(st.f_mntfromname.as_ptr()) }.to_string_lossy().into_owned();
    let unit = st.f_bsize as u64;
    Some(Stat {
        device,
        total: (st.f_blocks as u64).saturating_mul(unit),
        free: (st.f_bavail as u64).saturating_mul(unit),
        read_only: st.f_flags & (libc::MNT_RDONLY as u32) != 0,
    })
}

/// The macOS volume reader.
pub struct MacStorage;

impl StorageSource for MacStorage {
    fn volumes(&self) -> Result<Vec<Volume>> {
        let root = statfs(Path::new("/")).ok_or_else(|| Error::os("statfs /: failed".to_string()))?;
        // The APFS container, so sibling volumes of the startup disk are not
        // listed twice: `/dev/disk3s1s1` and `/dev/disk3s5` are both `disk3`.
        let container = |device: &str| {
            let name = device.trim_start_matches("/dev/");
            match name.strip_prefix("disk").and_then(|rest| rest.find('s').map(|i| &name[..4 + i])) {
                Some(disk) => disk.to_string(),
                None => name.to_string(),
            }
        };
        let mut seen: HashSet<String> = HashSet::from([container(&root.device)]);
        let mut out = vec![Volume {
            mount: PathBuf::from("/"),
            name: "Macintosh HD".into(),
            total: root.total,
            free: root.free,
            system: true,
        }];
        for entry in std::fs::read_dir("/Volumes").into_iter().flatten().flatten() {
            let path = entry.path();
            let Some(st) = statfs(&path) else { continue };
            // Disk images and installers mount read-only; nothing can be freed.
            if st.total == 0 || st.read_only || !st.device.starts_with("/dev/") || !seen.insert(container(&st.device)) {
                continue;
            }
            out.push(Volume {
                name: entry.file_name().to_string_lossy().into_owned(),
                mount: path,
                total: st.total,
                free: st.free,
                system: false,
            });
        }
        Ok(out)
    }
}
