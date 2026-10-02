//! `process_madvise`: page out chosen regions of another process.
//!
//! # Where this sits relative to cgroup reclaim
//!
//! [`crate::cgroup::Scope::reclaim`] is the workhorse — unprivileged, and it
//! reclaims a whole application's cold memory in one write. This is the finer
//! instrument: it targets specific mappings of a specific process, so the ladder
//! can take 200 MB of a browser's cold heap while leaving the tab the user is
//! reading fully resident. It is the same mechanism Android's `lmkd` uses to
//! keep background apps warm-but-cheap instead of killing them.
//!
//! # The capability boundary
//!
//! Applying `MADV_COLD` or `MADV_PAGEOUT` to a process other than the caller
//! requires `CAP_SYS_NICE`. A systemd *user* service cannot be granted
//! capabilities — the user manager is itself unprivileged — so this lives behind
//! a small `setcap cap_sys_nice+ep` helper binary rather than in the daemon.
//! When the helper is absent, every call here returns [`crate::Error::Denied`]
//! and the ladder falls back to cgroup reclaim. That fallback is the normal
//! configuration, not a degraded one: nothing about RamWarden requires the
//! capability to work.

use std::io;

use crate::procfd::PidFd;
use crate::smaps::Vma;
use crate::{Error, Result};

/// Move pages to the inactive list, so the kernel reclaims them first under
/// pressure. Non-committal: nothing is written out until memory is actually
/// needed, which makes it the right choice at a low rung on the ladder.
pub const COLD: i32 = 20;

/// Write pages out now — to zram on this machine — and free the RAM.
pub const PAGEOUT: i32 = 21;

/// The kernel accepts at most `UIO_MAXIOV` regions per call.
const MAX_IOV: usize = 1024;

/// Apply `advice` to `vmas` of the process behind `pidfd`.
///
/// Returns the number of bytes the kernel reports acting on. Regions are sent in
/// chunks of [`MAX_IOV`]; a chunk that fails after earlier chunks succeeded
/// still reports the bytes already handled, because the pages really were freed
/// and claiming otherwise would repeat v1's habit of reporting intentions.
pub fn advise(pidfd: &PidFd, vmas: &[Vma], advice: i32) -> Result<u64> {
    if vmas.is_empty() {
        return Ok(0);
    }

    let mut total = 0u64;

    for chunk in vmas.chunks(MAX_IOV) {
        let iov: Vec<libc::iovec> = chunk
            .iter()
            .filter(|v| !v.is_empty())
            .map(|v| libc::iovec {
                iov_base: v.start as *mut libc::c_void,
                iov_len: v.len() as usize,
            })
            .collect();
        if iov.is_empty() {
            continue;
        }

        // SAFETY: `iov` lives for the duration of the call and describes address
        // ranges in the *target* process, which is what the syscall expects — the
        // kernel never dereferences them in our address space.
        let rc = unsafe {
            libc::syscall(
                libc::SYS_process_madvise,
                pidfd.as_raw(),
                iov.as_ptr(),
                iov.len(),
                advice,
                0u32,
            )
        };

        if rc < 0 {
            let err = io::Error::last_os_error();
            let mapped = match err.raw_os_error() {
                Some(libc::EPERM) => Error::Denied {
                    path: format!("/proc/{}", pidfd.pid()).into(),
                    hint: "process_madvise on another process needs CAP_SYS_NICE",
                },
                Some(libc::ENOSYS) => {
                    Error::Unsupported("process_madvise (needs Linux 5.10+)")
                }
                Some(libc::EINVAL) => {
                    Error::Unsupported("MADV_COLD/MADV_PAGEOUT via process_madvise")
                }
                // The process exited between selecting its mappings and acting
                // on them. Everything already done still counts.
                Some(libc::ESRCH) => return Ok(total),
                _ => Error::io(format!("pid {} (process_madvise)", pidfd.pid()), err),
            };
            // Report partial progress rather than losing it.
            if total > 0 {
                tracing::debug!(pid = pidfd.pid(), bytes = total, "partial page-out: {mapped}");
                return Ok(total);
            }
            return Err(mapped);
        }

        total += rc as u64;
    }

    Ok(total)
}

/// Write the given regions out to swap/zram now.
pub fn page_out(pidfd: &PidFd, vmas: &[Vma]) -> Result<u64> {
    advise(pidfd, vmas, PAGEOUT)
}

/// Mark the given regions as first in line for reclaim, without writing them
/// out yet.
pub fn cool(pidfd: &PidFd, vmas: &[Vma]) -> Result<u64> {
    advise(pidfd, vmas, COLD)
}

/// `CAP_SYS_NICE`, as a capability number.
const CAP_SYS_NICE: u32 = 23;

/// Whether the `process_madvise` syscall exists on this kernel.
///
/// Probed against our own pidfd with an empty region list. Note what this does
/// *not* tell you: acting on the caller's own memory needs no capability, so a
/// `true` here says nothing about other processes. That distinction was briefly
/// got wrong, and the result was a helper that announced "capability present"
/// while holding none.
pub fn syscall_available() -> bool {
    let Ok(fd) = PidFd::open(std::process::id() as i32) else {
        return false;
    };
    // SAFETY: an empty iovec list is valid; the kernel validates and returns 0.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_process_madvise,
            fd.as_raw(),
            std::ptr::null::<libc::iovec>(),
            0usize,
            PAGEOUT,
            0u32,
        )
    };
    rc >= 0
}

/// Whether this process holds `CAP_SYS_NICE` in its effective set.
///
/// Read from `/proc/self/status`, which is the only honest answer. The
/// alternative — trying the syscall on a real process — either succeeds (and has
/// then已 disturbed it) or fails for reasons unrelated to privilege.
pub fn has_cap_sys_nice() -> bool {
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return false;
    };
    status
        .lines()
        .find_map(|l| l.strip_prefix("CapEff:"))
        .and_then(|hex| u64::from_str_radix(hex.trim(), 16).ok())
        .map(|bits| bits & (1u64 << CAP_SYS_NICE) != 0)
        .unwrap_or(false)
}

/// Whether this process may page out *other* processes' memory.
///
/// Both conditions: the syscall must exist, and we must hold `CAP_SYS_NICE`.
pub fn available() -> bool {
    has_cap_sys_nice() && syscall_available()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Root, smaps};

    #[test]
    fn an_empty_region_list_is_a_no_op() {
        let fd = PidFd::open(std::process::id() as i32).unwrap();
        assert_eq!(page_out(&fd, &[]).unwrap(), 0);
    }

    #[test]
    fn zero_length_regions_are_skipped_rather_than_sent() {
        let fd = PidFd::open(std::process::id() as i32).unwrap();
        let empty = Vma {
            start: 0x1000,
            end: 0x1000,
            rss: 0,
            referenced: 0,
            anonymous: 0,
            locked: false,
            private: true,
        };
        assert_eq!(page_out(&fd, &[empty]).unwrap(), 0);
    }

    /// Paging out our *own* memory needs no capability, so this exercises the
    /// real syscall end to end: allocate, touch, page out, and watch RSS fall.
    #[test]
    fn pages_out_our_own_cold_memory_and_rss_drops() {
        const LEN: usize = 64 * 1024 * 1024;

        // Touch every page so it is genuinely resident, and keep the buffer
        // alive across the call.
        let mut buf = vec![0u8; LEN];
        for i in (0..LEN).step_by(4096) {
            buf[i] = 1;
        }
        std::hint::black_box(&buf);

        let root = Root::system();
        let pid = std::process::id() as i32;
        let fd = PidFd::open(pid).unwrap();
        let before = smaps::rollup(&root, pid).unwrap().rss;

        let region = Vma {
            start: buf.as_ptr() as u64,
            end: buf.as_ptr() as u64 + LEN as u64,
            rss: LEN as u64,
            referenced: 0,
            anonymous: LEN as u64,
            locked: false,
            private: true,
        };

        let moved = match page_out(&fd, std::slice::from_ref(&region)) {
            Ok(n) => n,
            // A kernel without the syscall, or a sandbox that blocks it: the
            // fallback path is legitimate, so do not fail the suite over it.
            Err(Error::Unsupported(_)) | Err(Error::Denied { .. }) => return,
            Err(e) => panic!("unexpected error: {e:?}"),
        };
        assert!(moved > 0, "kernel reported no bytes advised");

        let after = smaps::rollup(&root, pid).unwrap().rss;
        assert!(
            after < before,
            "RSS should fall after page-out: {before} -> {after}"
        );

        // The buffer must still be readable — paging out is not losing data.
        std::hint::black_box(&buf);
        assert_eq!(buf[0], 1, "paged-out memory must fault back in intact");
    }

    #[test]
    fn availability_is_reported_without_touching_other_processes() {
        // Either answer is correct depending on whether the caller holds
        // CAP_SYS_NICE; what matters is that probing does not panic or block.
        let _ = available();
    }

    /// The syscall existing is not the same as being allowed to use it on another
    /// process. Conflating the two produced a helper that reported a capability
    /// it did not hold.
    #[test]
    fn the_syscall_existing_is_distinguished_from_holding_the_capability() {
        // On any kernel new enough, the syscall works on our own memory.
        assert!(syscall_available(), "process_madvise should exist on 5.10+");
        // And unless this test runs with the capability granted, it is absent —
        // which is what `available()` must report.
        assert_eq!(available(), has_cap_sys_nice());
    }

    /// The capability check must agree with what actually happens when we try.
    #[test]
    fn the_capability_check_predicts_the_real_outcome() {
        let mut child = std::process::Command::new("sleep").arg("30").spawn().unwrap();
        let fd = PidFd::open(child.id() as i32).unwrap();
        let region = Vma {
            start: 0x1000_0000,
            end: 0x1000_1000,
            rss: 4096,
            referenced: 0,
            anonymous: 4096,
            locked: false,
            private: true,
        };
        let refused = matches!(
            page_out(&fd, std::slice::from_ref(&region)),
            Err(Error::Denied { .. })
        );
        if refused {
            assert!(
                !has_cap_sys_nice(),
                "refused with EPERM but the capability check says we hold it"
            );
        }
        child.kill().ok();
        child.wait().ok();
    }

    #[test]
    fn a_handle_to_a_dead_process_reports_no_progress_rather_than_erroring() {
        let mut child = std::process::Command::new("sleep").arg("60").spawn().unwrap();
        let fd = PidFd::open(child.id() as i32).unwrap();
        child.kill().unwrap();
        child.wait().unwrap();

        let region = Vma {
            start: 0x1000_0000,
            end: 0x1000_1000,
            rss: 4096,
            referenced: 0,
            anonymous: 4096,
            locked: false,
            private: true,
        };
        match page_out(&fd, std::slice::from_ref(&region)) {
            Ok(n) => assert_eq!(n, 0, "a dead process yields no progress"),
            // Without CAP_SYS_NICE the permission check fires before the
            // liveness check; that is the kernel's ordering, not a bug here.
            Err(Error::Denied { .. }) | Err(Error::Unsupported(_)) => {}
            Err(e) => panic!("unexpected error: {e:?}"),
        }
    }
}
