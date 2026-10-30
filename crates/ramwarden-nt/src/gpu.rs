//! Per-process GPU memory, from the WDDM performance counters.
//!
//! On Windows, NVML cannot attribute VRAM to processes: under WDDM the driver
//! reports per-process usage as "not available" for every graphics context.
//! The OS itself tracks it, though — Task Manager's "Dedicated GPU memory"
//! column — and publishes it as the `GPU Process Memory` performance counter,
//! one instance per process per adapter:
//!
//! ```text
//! \GPU Process Memory(pid_19956_luid_0x00000000_0x0000D3F5_phys_0)\Dedicated Usage
//! ```

use std::collections::HashMap;

use ramwarden_platform::{Error, Result};
use windows::Win32::System::Performance::{
    PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_LARGE, PDH_HCOUNTER, PDH_HQUERY, PDH_MORE_DATA,
    PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW,
    PdhOpenQueryW,
};
use windows::core::PCWSTR;

const DEDICATED: &str = r"\GPU Process Memory(*)\Dedicated Usage";

/// A PDH query, closed on drop.
struct Query(PDH_HQUERY);

impl Drop for Query {
    fn drop(&mut self) {
        // SAFETY: we own the query handle.
        unsafe {
            PdhCloseQuery(self.0);
        }
    }
}

/// Extract the pid from an instance name like `pid_1234_luid_..._phys_0`.
pub fn instance_pid(name: &str) -> Option<u32> {
    name.strip_prefix("pid_")?.split('_').next()?.parse().ok()
}

/// Dedicated VRAM per pid, summed across adapters, in bytes.
pub fn dedicated_by_pid() -> Result<HashMap<u32, u64>> {
    let wide: Vec<u16> = DEDICATED.encode_utf16().chain([0]).collect();
    // SAFETY: PDH calls on a query we own; buffers sized by PDH's own answer.
    unsafe {
        let mut q = PDH_HQUERY::default();
        let rc = PdhOpenQueryW(PCWSTR::null(), 0, &mut q);
        if rc != 0 {
            return Err(Error::os(format!("PdhOpenQuery: {rc:#x}")));
        }
        let query = Query(q);
        let mut counter = PDH_HCOUNTER::default();
        let rc = PdhAddEnglishCounterW(query.0, PCWSTR(wide.as_ptr()), 0, &mut counter);
        if rc != 0 {
            return Err(Error::Unsupported("GPU Process Memory counters (no WDDM GPU?)"));
        }
        let rc = PdhCollectQueryData(query.0);
        if rc != 0 {
            return Err(Error::os(format!("PdhCollectQueryData: {rc:#x}")));
        }

        let (mut bytes, mut count) = (0u32, 0u32);
        let rc = PdhGetFormattedCounterArrayW(counter, PDH_FMT_LARGE, &mut bytes, &mut count, None);
        if rc != PDH_MORE_DATA {
            return Ok(HashMap::new());
        }
        let mut buf = vec![0u64; (bytes as usize).div_ceil(8)];
        let items = buf.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W;
        let rc = PdhGetFormattedCounterArrayW(counter, PDH_FMT_LARGE, &mut bytes, &mut count, Some(items));
        if rc != 0 {
            return Err(Error::os(format!("PdhGetFormattedCounterArray: {rc:#x}")));
        }

        let mut out: HashMap<u32, u64> = HashMap::new();
        for i in 0..count as usize {
            let item = &*items.add(i);
            let Ok(name) = item.szName.to_string() else { continue };
            let Some(pid) = instance_pid(&name) else { continue };
            let value = item.FmtValue.Anonymous.largeValue.max(0) as u64;
            *out.entry(pid).or_default() += value;
        }
        out.retain(|_, v| *v > 0);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_names_yield_pids() {
        assert_eq!(instance_pid("pid_19956_luid_0x00000000_0x0000D3F5_phys_0"), Some(19956));
        assert_eq!(instance_pid("_Total"), None);
        assert_eq!(instance_pid("pid_x_luid"), None);
    }

    /// The desktop compositor always holds VRAM on a machine with a GPU.
    #[test]
    fn the_live_counters_attribute_vram_to_the_compositor() {
        let Ok(by_pid) = dedicated_by_pid() else { return };
        if by_pid.is_empty() {
            return;
        }
        let dwm = crate::snapshot::processes()
            .unwrap()
            .into_iter()
            .find(|p| p.name.eq_ignore_ascii_case("dwm"))
            .map(|p| p.pid);
        if let Some(pid) = dwm {
            assert!(by_pid.get(&pid).copied().unwrap_or(0) > 0, "dwm holds no VRAM? {by_pid:?}");
        }
    }
}
