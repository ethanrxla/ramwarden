//! The standby list: cache, not usage.
//!
//! Standby pages are file and trimmed pages the memory manager keeps because
//! they might be wanted again. They already count as *available*, and Windows
//! repurposes them on demand, so purging them frees nothing anyone can use — it
//! only throws away cache that would have saved a disk read.
//!
//! RamWarden therefore never purges the whole list. It offers only the
//! **low-priority** standby purge: pages at priority 0–1, which is where
//! [`crate::priority`] has sent idle background apps. Even that runs only inside
//! `ramwarden-service`, because it needs `SeProfileSingleProcessPrivilege`.

use std::ffi::c_void;

use ramwarden_platform::{Error, Result};

use crate::ntdll;
use crate::privilege;

/// Purge low-priority standby pages. Requires administrator or SYSTEM.
pub fn purge_low_priority() -> Result<()> {
    privilege::enable(privilege::PROFILE_SINGLE_PROCESS)?;
    let mut command = ntdll::MEMORY_PURGE_LOW_PRIORITY_STANDBY_LIST;
    // SAFETY: the information class takes a single u32 command.
    let status = unsafe {
        ntdll::NtSetSystemInformation(
            ntdll::SYSTEM_MEMORY_LIST_INFORMATION,
            &mut command as *mut u32 as *mut c_void,
            std::mem::size_of::<u32>() as u32,
        )
    };
    match status {
        s if ntdll::success(s) => Ok(()),
        ntdll::STATUS_PRIVILEGE_NOT_HELD | ntdll::STATUS_ACCESS_DENIED => Err(Error::denied(
            "low-priority standby purge",
            "needs SeProfileSingleProcessPrivilege (ramwarden-service)",
        )),
        s => Err(Error::os(format!("NtSetSystemInformation(MemoryListInformation): {s:#x}"))),
    }
}
