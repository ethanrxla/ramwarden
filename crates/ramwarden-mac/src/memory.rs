use ramwarden_platform::{Compression, Error, MemorySource, MemoryStatus, Result};

use crate::sys::{page_size, sysctl_int, sysctl_struct, vm_stats};

pub struct MacMemory;

impl MemorySource for MacMemory {
    fn status(&self) -> Result<MemoryStatus> {
        let total = sysctl_int("hw.memsize").ok_or(Error::Unsupported("hw.memsize"))?;
        let vm = vm_stats().ok_or(Error::Unsupported("host_statistics64"))?;
        let ps = page_size();
        // Free, inactive and speculative pages can be handed out without
        // swapping; active, wired and compressed pages cannot.
        let available = (vm.free_count as u64 + vm.inactive_count as u64 + vm.speculative_count as u64) * ps;
        let swap = sysctl_struct::<libc::xsw_usage>("vm.swapusage");
        Ok(MemoryStatus {
            total,
            free: vm.free_count as u64 * ps,
            available: available.min(total),
            cached: vm.external_page_count as u64 * ps,
            swap_total: swap.map(|s| s.xsu_total).unwrap_or(0),
            swap_free: swap.map(|s| s.xsu_avail).unwrap_or(0),
            commit: 0,
            commit_limit: 0,
        })
    }

    fn compression(&self) -> Result<Compression> {
        let vm = vm_stats().ok_or(Error::Unsupported("host_statistics64"))?;
        let ps = page_size();
        Ok(Compression {
            original: vm.total_uncompressed_pages_in_compressor * ps,
            resident: vm.compressor_page_count as u64 * ps,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_hardware() {
        let m = MacMemory.status().unwrap();
        assert_eq!(Some(m.total), sysctl_int("hw.memsize"));
        assert!(m.available > 0 && m.available <= m.total);
        assert!(MacMemory.compression().unwrap().ratio() >= 1.0);
    }
}
