//! `process_madvise` has no macOS equivalent: one process cannot ask the kernel
//! to page out another's memory. Every entry point reports that, so the ladder
//! skips the page-out rung instead of failing.

use crate::procfd::PidFd;
use crate::smaps::Vma;
use crate::{Error, Result};

pub const COLD: i32 = 20;
pub const PAGEOUT: i32 = 21;

pub fn advise(_pidfd: &PidFd, _vmas: &[Vma], _advice: i32) -> Result<u64> {
    Err(Error::Unsupported("process_madvise (Linux only)"))
}

pub fn page_out(pidfd: &PidFd, vmas: &[Vma]) -> Result<u64> {
    advise(pidfd, vmas, PAGEOUT)
}

pub fn cool(pidfd: &PidFd, vmas: &[Vma]) -> Result<u64> {
    advise(pidfd, vmas, COLD)
}

pub fn syscall_available() -> bool {
    false
}

pub fn has_cap_sys_nice() -> bool {
    false
}

pub fn available() -> bool {
    false
}
