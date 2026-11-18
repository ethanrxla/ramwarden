//! Free space per mounted disk: `/proc/self/mounts` plus `statvfs`.
//!
//! # Which mounts count
//!
//! A desktop lists dozens of mounts and almost none are disks: `proc`,
//! `tmpfs`, cgroups, one `squashfs` per snap, one `overlay` per container.
//! Reporting those would bury `/` under noise and would alert on images that
//! are 100% full by design. A mount is kept when its source is a block device
//! (`/dev/...`) or a pool-style filesystem (`zfs`, `btrfs` subvolumes) and its
//! type is not a read-only image format. One row per device: btrfs subvolumes
//! and bind mounts share their free space, so only the shortest mount point is
//! reported.

use std::collections::HashMap;
use std::ffi::CString;
use std::fs;
use std::mem::MaybeUninit;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use ramwarden_platform::{Error, Result, StorageSource, Volume};

use crate::Root;

/// Image and pseudo filesystems that can sit on a `/dev` source.
const NOT_DISKS: &[&str] = &["squashfs", "iso9660", "udf", "erofs", "cramfs", "romfs"];

/// One line of the mount table that names a real disk.
#[derive(Debug, PartialEq, Eq)]
pub struct Mount {
    pub source: String,
    pub point: PathBuf,
}

/// Decode the octal escapes the kernel uses for spaces and tabs in paths.
fn unescape(field: &str) -> String {
    let b = field.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c)) {
            let n = b[i + 1..i + 4].iter().fold(0u32, |a, c| a * 8 + (c - b'0') as u32);
            out.push(n as u8);
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The disks in a mount table, one per device, shortest mount point first.
pub fn parse_mounts(table: &str) -> Vec<Mount> {
    let mut by_source: HashMap<String, PathBuf> = HashMap::new();
    for line in table.lines() {
        let mut f = line.split_whitespace();
        let (Some(source), Some(point), Some(kind)) = (f.next(), f.next(), f.next()) else { continue };
        let pooled = matches!(kind, "zfs" | "btrfs" | "bcachefs");
        if NOT_DISKS.contains(&kind) || !(source.starts_with("/dev/") || pooled) || source.starts_with("/dev/loop") {
            continue;
        }
        let point = PathBuf::from(unescape(point));
        by_source
            .entry(unescape(source))
            .and_modify(|p| {
                if point.as_os_str().len() < p.as_os_str().len() {
                    *p = point.clone();
                }
            })
            .or_insert(point);
    }
    let mut out: Vec<Mount> = by_source.into_iter().map(|(source, point)| Mount { source, point }).collect();
    out.sort_by(|a, b| a.point.cmp(&b.point));
    out
}

/// `(total, free to this user)` in bytes for the filesystem holding `path`.
// The statvfs fields are 32-bit on some targets, so the casts are not always no-ops.
#[allow(clippy::unnecessary_cast)]
pub fn space(path: &Path) -> Result<(u64, u64)> {
    let c = CString::new(path.as_os_str().as_bytes()).map_err(|_| Error::os(format!("{}: path holds a NUL", path.display())))?;
    let mut st = MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `c` is a valid C string and `st` is writable for one statvfs.
    if unsafe { libc::statvfs(c.as_ptr(), st.as_mut_ptr()) } != 0 {
        return Err(Error::os(format!("statvfs {}: {}", path.display(), std::io::Error::last_os_error())));
    }
    // SAFETY: statvfs returned 0, so it filled the struct.
    let st = unsafe { st.assume_init() };
    let unit = st.f_frsize as u64;
    Ok(((st.f_blocks as u64).saturating_mul(unit), (st.f_bavail as u64).saturating_mul(unit)))
}

/// The Linux volume reader.
pub struct LinuxStorage {
    pub root: Root,
}

impl StorageSource for LinuxStorage {
    fn volumes(&self) -> Result<Vec<Volume>> {
        let path = self.root.join("proc/self/mounts");
        let table = fs::read_to_string(&path).map_err(|e| Error::Missing(format!("{}: {e}", path.display())))?;
        let mut out = Vec::new();
        for m in parse_mounts(&table) {
            // A mount that cannot be read (stale NFS-backed loop, no permission)
            // is skipped, not an error: the others still matter.
            let Ok((total, free)) = space(&m.point) else { continue };
            if total == 0 {
                continue;
            }
            out.push(Volume {
                name: m.point.display().to_string(),
                system: m.point == Path::new("/"),
                mount: m.point,
                total,
                free,
            });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE: &str = "\
proc /proc proc rw,nosuid 0 0
tmpfs /run tmpfs rw 0 0
/dev/nvme0n1p2 / ext4 rw,relatime 0 0
/dev/nvme0n1p2 /var/lib/docker/btrfs ext4 rw 0 0
/dev/loop3 /snap/core22/1122 squashfs ro,nodev 0 0
/dev/nvme0n1p1 /boot/efi vfat rw 0 0
/dev/sda1 /media/ethan/My\\040Disk exfat rw 0 0
overlay /var/lib/docker/overlay2/x/merged overlay rw 0 0
tank/home /home zfs rw 0 0
//nas/share /mnt/nas cifs rw 0 0
";

    #[test]
    fn only_real_disks_are_listed_once_each() {
        let m = parse_mounts(TABLE);
        let points: Vec<_> = m.iter().map(|m| m.point.to_str().unwrap()).collect();
        assert_eq!(points, vec!["/", "/boot/efi", "/home", "/media/ethan/My Disk"]);
    }

    #[test]
    fn the_live_root_has_a_size() {
        let (total, free) = space(Path::new("/")).unwrap();
        assert!(total > 0 && free <= total);
    }
}
