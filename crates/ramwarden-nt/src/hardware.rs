//! CPU model and core counts on Windows.
//!
//! Cores come from `GetLogicalProcessorInformationEx(RelationProcessorCore)`:
//! one record per physical core, whose `EfficiencyClass` separates
//! performance from efficiency cores on hybrid CPUs (higher is faster). The
//! model name comes from CPUID, which needs no registry access.

use ramwarden_platform::{Hardware, HardwareSource, Result};
use windows::Win32::System::SystemInformation::{
    GetLogicalProcessorInformationEx, RelationProcessorCore, SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX,
};

pub struct NtHardware;

impl HardwareSource for NtHardware {
    fn read(&self) -> Result<Hardware> {
        let classes = core_classes();
        let (performance, efficiency) = split(&classes);
        Ok(Hardware {
            cpu_model: ramwarden_platform::hardware::cpu_brand(),
            physical_cores: (!classes.is_empty()).then_some(classes.len() as u32),
            logical_cores: ramwarden_platform::hardware::logical_cores(),
            performance_cores: performance,
            efficiency_cores: efficiency,
        })
    }
}

/// P- and E-core counts when the CPU has more than one efficiency class.
fn split(classes: &[u8]) -> (Option<u32>, Option<u32>) {
    let Some(&top) = classes.iter().max() else { return (None, None) };
    if classes.iter().all(|&c| c == top) {
        return (None, None);
    }
    let p = classes.iter().filter(|&&c| c == top).count() as u32;
    (Some(p), Some(classes.len() as u32 - p))
}

/// The efficiency class of every physical core. Empty if the call fails.
fn core_classes() -> Vec<u8> {
    let mut len = 0u32;
    // SAFETY: a null buffer asks for the required length only.
    let _ = unsafe { GetLogicalProcessorInformationEx(RelationProcessorCore, None, &mut len) };
    if len == 0 {
        return Vec::new();
    }
    // u64 storage keeps the records 8-byte aligned.
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    let ptr = buf.as_mut_ptr().cast::<SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX>();
    // SAFETY: `buf` holds at least `len` writable bytes.
    if unsafe { GetLogicalProcessorInformationEx(RelationProcessorCore, Some(ptr), &mut len) }.is_err() {
        return Vec::new();
    }
    let bytes = buf.as_ptr().cast::<u8>();
    let mut out = Vec::new();
    let mut offset = 0usize;
    while offset + std::mem::size_of::<u32>() * 2 <= len as usize {
        // SAFETY: the API wrote a sequence of variable-sized records within
        // `len` bytes; each starts at the previous one's `Size`.
        let rec = unsafe { &*bytes.add(offset).cast::<SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX>() };
        if rec.Size == 0 {
            break;
        }
        if rec.Relationship == RelationProcessorCore {
            // SAFETY: a RelationProcessorCore record holds the Processor variant.
            out.push(unsafe { rec.Anonymous.Processor.EfficiencyClass });
        }
        offset += rec.Size as usize;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hybrid_split_needs_two_classes() {
        assert_eq!(split(&[0; 8]), (None, None));
        assert_eq!(split(&[1, 1, 0, 0, 0, 0]), (Some(2), Some(4)));
        assert_eq!(split(&[]), (None, None));
    }

    #[test]
    fn this_machine_has_cores() {
        let hw = NtHardware.read().unwrap();
        assert!(hw.physical_cores.is_some_and(|n| n >= 1));
        assert!(hw.logical_cores >= hw.physical_cores);
    }
}
