//! System memory and the Memory Compression store.

use std::ffi::c_void;

use ramwarden_platform::{Compression, Error, MemorySource, MemoryStatus, Result};
use windows::Win32::System::ProcessStatus::{GetPerformanceInfo, PERFORMANCE_INFORMATION};
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

use crate::ntdll::{self, MemCompressionInfo, SystemStoreInformation};
use crate::snapshot;

/// Physical memory, commit and cache, in bytes.
pub fn status() -> Result<MemoryStatus> {
    let mut m = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    // SAFETY: dwLength is set as the API requires.
    unsafe { GlobalMemoryStatusEx(&mut m) }
        .map_err(|e| Error::os(format!("GlobalMemoryStatusEx: {e}")))?;

    let mut p = PERFORMANCE_INFORMATION {
        cb: std::mem::size_of::<PERFORMANCE_INFORMATION>() as u32,
        ..Default::default()
    };
    // SAFETY: cb is set as the API requires.
    unsafe { GetPerformanceInfo(&mut p, p.cb) }
        .map_err(|e| Error::os(format!("GetPerformanceInfo: {e}")))?;
    let page = p.PageSize as u64;

    // "Swap" on Windows is the page file: the commit limit beyond physical RAM.
    let swap_total = m.ullTotalPageFile.saturating_sub(m.ullTotalPhys);
    let swap_free = m.ullAvailPageFile.saturating_sub(m.ullAvailPhys).min(swap_total);

    Ok(MemoryStatus {
        total: m.ullTotalPhys,
        // Truly free (zeroed + free lists): available minus the standby cache.
        free: m.ullAvailPhys.saturating_sub(p.SystemCache as u64 * page),
        available: m.ullAvailPhys,
        cached: p.SystemCache as u64 * page,
        swap_total,
        swap_free,
        commit: p.CommitTotal as u64 * page,
        commit_limit: p.CommitLimit as u64 * page,
    })
}

/// The compression store: what it holds, and the RAM it costs.
///
/// Asks the store manager directly first, which reports both figures. That
/// request needs administrator rights on some builds; without them, the RAM
/// cost is still visible as the Memory Compression process's working set, and
/// the stored size is reported as equal to it — a ratio of 1.0, claiming no
/// savings that could not be measured.
pub fn compression() -> Result<Compression> {
    if let Some(c) = store_stats() {
        return Ok(c);
    }
    let table = snapshot::processes()?;
    let resident = table
        .iter()
        .find(|p| p.name == "Memory Compression")
        .map(|p| p.working_set)
        .unwrap_or(0);
    Ok(Compression {
        original: resident,
        resident,
    })
}

fn store_stats() -> Option<Compression> {
    let mut info = MemCompressionInfo {
        version: 3,
        ..Default::default()
    };
    let mut request = SystemStoreInformation {
        version_and_class: 1 | (ntdll::MEM_COMPRESSION_INFO_REQUEST << 8),
        data: &mut info as *mut MemCompressionInfo as *mut c_void,
        length: std::mem::size_of::<MemCompressionInfo>() as u32,
    };
    // SAFETY: `request` points at `info`, both live for the call.
    let status = unsafe {
        ntdll::NtQuerySystemInformation(
            ntdll::SYSTEM_STORE_INFORMATION,
            &mut request as *mut SystemStoreInformation as *mut c_void,
            std::mem::size_of::<SystemStoreInformation>() as u32,
            std::ptr::null_mut(),
        )
    };
    if !ntdll::success(status) || info.total_compressed_size == 0 {
        return None;
    }
    Some(Compression {
        original: info.total_data_compressed as u64,
        resident: info.total_compressed_size as u64,
    })
}

/// [`MemorySource`] over the live system.
pub struct NtMemory;

impl MemorySource for NtMemory {
    fn status(&self) -> Result<MemoryStatus> {
        status()
    }

    fn compression(&self) -> Result<Compression> {
        compression()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_status_is_self_consistent() {
        let m = status().unwrap();
        assert!(m.total > 1024 * 1024 * 1024, "{m:?}");
        assert!(m.available <= m.total);
        assert!(m.commit <= m.commit_limit);
        assert!(m.percent() > 0.0 && m.percent() < 100.0);
    }

    #[test]
    fn compression_never_claims_more_savings_than_it_measured() {
        let c = compression().unwrap();
        assert!(c.ratio() >= 1.0 || c.resident == 0, "{c:?}");
    }
}
